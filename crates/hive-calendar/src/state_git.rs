//! Local-only, files-backend state commits. The operator MUST exclusively
//! administer this dedicated bare repository during an operation. Preflight and
//! pre-CAS checks cannot exclude concurrent worktree/configuration administration.
//! Ref CAS serializes competing ledger writers, not repository administrators.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::Path,
    time::Instant,
};

use anyhow::{Result, anyhow, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    Binding, CandidateOperation, PreparedLedgerChange,
    source::{GitSession, SourceSession},
};

const STATE_REF: &str = "refs/heads/hive-calendar-state";
const MARKER: &str = "hive-calendar-state v1\n\n";
const IDENTITY: &str = "Hive Calendar <hive-calendar@localhost>";
// Individual objects fit the existing transport's input bound; the entire
// visited tree graph (including reconstructed objects) is capped separately.
const OBJECT_CAP: usize = 1024 * 1024;
const GRAPH_CAP: usize = 64 * 1024 * 1024;
const ENTRY_CAP: usize = 100_000;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyReceipt {
    pub schema_version: &'static str,
    pub status: &'static str,
    pub state_ref: String,
    pub ledger_path: String,
    pub binding: Binding,
    pub candidate_sha256: String,
    pub source_commit: String,
    pub base_commit: String,
    pub commit: String,
    #[serde(rename = "ledgerBlobID")]
    pub ledger_blob_id: String,
    pub operation: CandidateOperation,
    pub logical_id: String,
    pub parser_change_accepted: bool,
    pub local_ref_updated: bool,
    pub publication_ready: bool,
    pub remote_verified: bool,
    pub remote_ref_verified: bool,
}

// Field order, marker, JSON encoding and final LF are the v1 wire format.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Record {
    schema_version: String,
    state_ref: String,
    path: String,
    binding: Binding,
    ledger_blob_id: String,
    candidate_sha256: String,
    source_commit: String,
    operation: String,
    logical_id: String,
    parser_change_accepted: bool,
    base_commit: String,
    expected_blob_id: Option<String>,
}

impl Record {
    fn message(&self) -> Result<Vec<u8>> {
        let mut bytes = MARKER.as_bytes().to_vec();
        bytes.extend(serde_json::to_vec(self)?);
        bytes.push(b'\n');
        Ok(bytes)
    }

    fn validate(&self, len: usize) -> Result<()> {
        ensure!(
            self.schema_version == "1"
                && self.state_ref == STATE_REF
                && valid_path(&self.path)
                && oid(&self.ledger_blob_id, len)
                && oid(&self.candidate_sha256, 64)
                && (oid(&self.source_commit, 40) || oid(&self.source_commit, 64))
                && oid(&self.base_commit, len)
                && self.expected_blob_id.as_deref().is_none_or(|v| oid(v, len))
                && matches!(self.operation.as_str(), "enroll" | "reack")
                && !self.logical_id.is_empty()
                && self.logical_id.len() <= 4096
                && !self.logical_id.chars().any(char::is_control),
            "invalid state ownership receipt"
        );
        crate::ledger::validate_binding(&self.binding)
    }
}

struct Object {
    kind: &'static str,
    bytes: Vec<u8>,
    oid: String,
}

#[derive(Clone)]
struct Entry {
    mode: Vec<u8>,
    name: Vec<u8>,
    oid: String,
}

impl Entry {
    fn key(&self) -> Vec<u8> {
        let mut key = self.name.clone();
        if self.mode == b"40000" {
            key.push(b'/');
        }
        key
    }
}

struct Commit {
    tree: String,
    parents: Vec<String>,
    time: i64,
    message: Vec<u8>,
    bytes: Vec<u8>,
}

struct Preparation {
    git: GitSession,
    len: usize,
    trees: BTreeMap<String, Vec<Entry>>,
    bytes: usize,
    entries: usize,
    objects: Vec<Object>,
}

fn oid(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && value.bytes().any(|b| b != b'0')
}

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path.to_ascii_lowercase().ends_with(".json")
        && path.split('/').count() <= 64
        && path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != ".." && !p.eq_ignore_ascii_case(".git"))
}

/// Inspect metadata without following the final component. Repository internals
/// must be operator/root owned and not writable by other users.
fn metadata(path: &Path) -> Result<Option<fs::Metadata>> {
    let m = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(anyhow!("state repository metadata unavailable")),
    };
    ensure!(
        !m.file_type().is_symlink(),
        "state repository symlink refused"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            (m.uid() == unsafe { libc::geteuid() } || m.uid() == 0) && m.mode() & 0o022 == 0,
            "state repository must be operator-owned and not group/world writable"
        );
    }
    ensure!(
        m.is_dir() || m.is_file(),
        "nonregular state repository metadata"
    );
    Ok(Some(m))
}

fn file(path: &Path, cap: usize) -> Result<Option<Vec<u8>>> {
    let Some(m) = metadata(path)? else {
        return Ok(None);
    };
    ensure!(
        m.is_file() && m.len() <= cap as u64,
        "invalid state metadata file"
    );
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut f = options
        .open(path)
        .map_err(|_| anyhow!("state metadata unreadable"))?;
    ensure!(f.metadata()?.is_file(), "invalid state metadata file");
    let mut bytes = Vec::new();
    (&mut f)
        .take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow!("state metadata unreadable"))?;
    ensure!(bytes.len() <= cap, "state metadata size limit exceeded");
    Ok(Some(bytes))
}

fn scan(git: &GitSession, path: &Path, depth: usize, count: &mut usize) -> Result<()> {
    git.check()?;
    ensure!(
        depth <= 64 && *count < ENTRY_CAP,
        "state metadata traversal limit exceeded"
    );
    *count += 1;
    let Some(m) = metadata(path)? else {
        return Ok(());
    };
    if m.is_dir() {
        for entry in fs::read_dir(path).map_err(|_| anyhow!("state metadata unreadable"))? {
            let entry = entry.map_err(|_| anyhow!("state metadata unreadable"))?;
            scan(git, &entry.path(), depth + 1, count)?;
        }
    }
    Ok(())
}

fn repository(git: &GitSession) -> Result<usize> {
    git.check()?;
    let repo = git.repo();
    ensure!(
        metadata(repo)?.is_some_and(|m| m.is_dir()),
        "state repository must be a directory"
    );
    // Only the actual bare Git directory is accepted, never a worktree, .git
    // indirection, common-dir indirection, alternate object store or reftable.
    for name in ["objects", "refs", "info", "logs"] {
        scan(git, &repo.join(name), 0, &mut 0)?;
    }
    for name in [
        "commondir",
        "gitdir",
        "shallow",
        "objects/info/alternates",
        "objects/info/http-alternates",
    ] {
        ensure!(
            metadata(&repo.join(name))?.is_none(),
            "state repository indirection refused"
        );
    }
    for name in ["objects", "refs"] {
        ensure!(
            metadata(&repo.join(name))?.is_some_and(|m| m.is_dir()),
            "missing state object/ref directory"
        );
    }
    for name in ["config", "config.worktree", "HEAD", "packed-refs"] {
        file(&repo.join(name), OBJECT_CAP)?;
    }
    if let Some(m) = metadata(&repo.join("worktrees"))? {
        ensure!(m.is_dir(), "invalid worktree registry");
        ensure!(
            fs::read_dir(repo.join("worktrees"))?.next().is_none(),
            "registered worktrees refused, including unborn worktrees"
        );
    }
    ensure!(
        git.run(&["rev-parse", "--is-bare-repository"], b"", 1024)? == b"true\n",
        "dedicated bare state repository required"
    );
    let actual = git.run(&["rev-parse", "--absolute-git-dir"], b"", 8192)?;
    let mut expected = repo.as_os_str().as_encoded_bytes().to_vec();
    expected.push(b'\n');
    ensure!(actual == expected, "state Git directory mismatch");
    ensure!(
        git.run(&["rev-parse", "--show-ref-format"], b"", 1024)? == b"files\n",
        "state repository requires files ref backend"
    );
    match git
        .run(&["rev-parse", "--show-object-format=storage"], b"", 1024)?
        .as_slice()
    {
        b"sha1\n" => Ok(40),
        b"sha256\n" => Ok(64),
        _ => Err(anyhow!("unsupported state object format")),
    }
}

fn current(git: &GitSession, len: usize) -> Result<Option<String>> {
    // for-each-ref alone omits dangling symrefs; inspect the known loose path too.
    let loose = file(&git.repo().join(STATE_REF), 1024)?;
    let loose_oid = loose
        .as_ref()
        .map(|bytes| {
            let value = std::str::from_utf8(bytes)
                .ok()
                .and_then(|s| s.strip_suffix('\n'))
                .filter(|s| oid(s, len))
                .ok_or_else(|| anyhow!("symbolic or malformed state ref refused"))?;
            Ok::<_, anyhow::Error>(value.to_owned())
        })
        .transpose()?;
    let bytes = git.run(
        &[
            "for-each-ref",
            "--format=%(refname)%00%(objectname)%00%(symref)%00",
            "refs/heads",
        ],
        b"",
        GRAPH_CAP,
    )?;
    let mut found = None;
    for line in bytes.split(|b| *b == b'\n').filter(|s| !s.is_empty()) {
        let fields: Vec<_> = line.split(|b| *b == 0).collect();
        ensure!(
            fields.len() == 4 && fields[3].is_empty(),
            "invalid state ref query"
        );
        let name = std::str::from_utf8(fields[0]).map_err(|_| anyhow!("invalid ref encoding"))?;
        ensure!(
            !name.starts_with(&format!("{STATE_REF}/"))
                && !STATE_REF.starts_with(&format!("{name}/")),
            "state ref namespace collision"
        );
        if name == STATE_REF {
            let value =
                std::str::from_utf8(fields[1]).map_err(|_| anyhow!("invalid ref object ID"))?;
            ensure!(
                fields[2].is_empty() && oid(value, len) && found.is_none(),
                "symbolic or malformed state ref refused"
            );
            found = Some(value.to_owned());
        }
    }
    ensure!(
        loose_oid.is_none() || loose_oid == found,
        "state loose ref/query mismatch"
    );
    Ok(found)
}

impl Preparation {
    fn hash(&self, kind: &str, bytes: &[u8], write: bool) -> Result<String> {
        self.git.check()?;
        ensure!(
            bytes.len() <= OBJECT_CAP,
            "state object size limit exceeded"
        );
        let mut args = vec!["hash-object", "--no-filters", "-t", kind, "--stdin"];
        if write {
            args.push("-w");
        }
        let out = self.git.run(&args, bytes, 1024)?;
        let value = std::str::from_utf8(&out)
            .ok()
            .and_then(|s| s.strip_suffix('\n'))
            .filter(|s| oid(s, self.len))
            .ok_or_else(|| anyhow!("invalid computed state object ID"))?;
        Ok(value.into())
    }

    fn read(&mut self, kind: &str, id: &str) -> Result<Vec<u8>> {
        self.git.check()?;
        ensure!(oid(id, self.len), "invalid state object ID");
        // Batch framing proves the type, size and requested full ID. Reading a
        // missing/corrupt object is never interpreted as path/ref absence.
        let out = self.git.run(
            &["cat-file", "--batch"],
            format!("{id}\n").as_bytes(),
            OBJECT_CAP + 256,
        )?;
        let n = out
            .iter()
            .position(|b| *b == b'\n')
            .ok_or_else(|| anyhow!("invalid state object framing"))?;
        let header =
            std::str::from_utf8(&out[..n]).map_err(|_| anyhow!("invalid state object header"))?;
        let fields: Vec<_> = header.split(' ').collect();
        ensure!(
            fields.len() == 3 && fields[0] == id && fields[1] == kind,
            "missing or incorrect state object type"
        );
        let size = fields[2]
            .parse::<usize>()
            .map_err(|_| anyhow!("invalid state object size"))?;
        ensure!(
            size <= OBJECT_CAP && out.len() == n + size + 2 && out.last() == Some(&b'\n'),
            "invalid state object size/framing"
        );
        self.bytes = self
            .bytes
            .checked_add(size)
            .ok_or_else(|| anyhow!("state graph limit exceeded"))?;
        ensure!(self.bytes <= GRAPH_CAP, "state graph limit exceeded");
        let bytes = out[n + 1..n + 1 + size].to_vec();
        ensure!(
            self.hash(kind, &bytes, false)? == id,
            "state object hash mismatch"
        );
        Ok(bytes)
    }

    fn object(&mut self, kind: &'static str, bytes: Vec<u8>) -> Result<String> {
        self.git.check()?;
        self.bytes += bytes.len();
        ensure!(self.bytes <= GRAPH_CAP, "state graph limit exceeded");
        let oid = self.hash(kind, &bytes, false)?;
        self.objects.push(Object {
            kind,
            bytes,
            oid: oid.clone(),
        });
        Ok(oid)
    }

    fn commit(&mut self, id: &str) -> Result<Commit> {
        self.git.check()?;
        let bytes = self.read("commit", id)?;
        let split = bytes
            .windows(2)
            .position(|w| w == b"\n\n")
            .ok_or_else(|| anyhow!("malformed state base commit"))?;
        let header =
            std::str::from_utf8(&bytes[..split]).map_err(|_| anyhow!("invalid commit headers"))?;
        ensure!(
            !header.contains('\0') && !header.contains('\r'),
            "malformed commit headers"
        );
        let mut lines = header.lines();
        let tree = lines
            .next()
            .and_then(|s| s.strip_prefix("tree "))
            .filter(|s| oid(s, self.len))
            .ok_or_else(|| anyhow!("invalid commit tree"))?
            .to_owned();
        let mut parents = Vec::new();
        let mut author = false;
        let mut time = None;
        for line in lines {
            if let Some(parent) = line.strip_prefix("parent ") {
                ensure!(
                    !author && oid(parent, self.len) && parents.len() < 64,
                    "invalid commit parent"
                );
                parents.push(parent.into());
            } else if line.starts_with("author ") {
                ensure!(!author && time.is_none(), "invalid commit author");
                parse_time(line)?;
                author = true;
            } else if line.starts_with("committer ") {
                ensure!(author && time.is_none(), "invalid commit committer");
                time = Some(parse_time(line)?);
            } else {
                ensure!(
                    time.is_some() && !line.starts_with("tree "),
                    "invalid commit header order"
                );
            }
        }
        ensure!(author, "missing commit author");
        let time = time.ok_or_else(|| anyhow!("missing commit committer"))?;
        Ok(Commit {
            tree,
            parents,
            time,
            message: bytes[split + 2..].to_vec(),
            bytes,
        })
    }

    fn tree(&mut self, id: &str, depth: usize) -> Result<Vec<Entry>> {
        self.git.check()?;
        ensure!(depth <= 64, "state tree depth limit exceeded");
        if let Some(entries) = self.trees.get(id) {
            return Ok(entries.clone());
        }
        let bytes = self.read("tree", id)?;
        let entries = parse_tree(&bytes, self.len)?;
        self.entries += entries.len();
        ensure!(self.entries <= ENTRY_CAP, "state tree entry limit exceeded");
        // A mode is not proof of object type. Validate local blobs without
        // materializing unrelated contents. Gitlinks belong to another repo.
        let blobs: Vec<_> = entries
            .iter()
            .filter(|e| matches!(e.mode.as_slice(), b"100644" | b"100755" | b"120000"))
            .collect();
        for chunk in blobs.chunks(8192) {
            let input: String = chunk.iter().map(|e| format!("{}\n", e.oid)).collect();
            let out = self.git.run(
                &["cat-file", "--batch-check=%(objectname) %(objecttype)"],
                input.as_bytes(),
                OBJECT_CAP,
            )?;
            let expected: String = chunk.iter().map(|e| format!("{} blob\n", e.oid)).collect();
            ensure!(
                out == expected.as_bytes(),
                "missing or malformed base tree blob"
            );
        }
        for entry in &entries {
            self.git.check()?;
            if entry.mode == b"40000" {
                self.tree(&entry.oid, depth + 1)?;
            }
        }
        self.trees.insert(id.into(), entries.clone());
        Ok(entries)
    }

    fn replace(
        &mut self,
        tree: Option<&str>,
        path: &[&str],
        blob: &str,
        expected: Option<&str>,
        depth: usize,
    ) -> Result<String> {
        self.git.check()?;
        let mut entries = match tree {
            Some(id) => self.tree(id, depth)?,
            None => Vec::new(),
        };
        let pos = entries.iter().position(|e| e.name == path[0].as_bytes());
        let old = pos.map(|p| entries.remove(p));
        let (mode, id) = if path.len() == 1 {
            match (&old, expected) {
                (None, None) => {}
                (Some(e), Some(id))
                    if matches!(e.mode.as_slice(), b"100644" | b"100755") && e.oid == id => {}
                _ => return Err(anyhow!("ledger base path/blob condition failed")),
            }
            (b"100644".to_vec(), blob.to_owned())
        } else {
            ensure!(
                old.as_ref().is_none_or(|e| e.mode == b"40000"),
                "ledger parent must be a regular Git tree"
            );
            let id = self.replace(
                old.as_ref().map(|e| e.oid.as_str()),
                &path[1..],
                blob,
                expected,
                depth + 1,
            )?;
            (b"40000".to_vec(), id)
        };
        entries.push(Entry {
            mode,
            name: path[0].as_bytes().to_vec(),
            oid: id,
        });
        entries.sort_by_key(Entry::key);
        self.object("tree", encode_tree(&entries)?)
    }

    fn lookup(&mut self, tree: &str, path: &[&str]) -> Result<String> {
        let mut id = tree.to_owned();
        for (i, name) in path.iter().enumerate() {
            let entries = self.tree(&id, i)?;
            let e = entries
                .iter()
                .find(|e| e.name == name.as_bytes())
                .ok_or_else(|| anyhow!("owned ledger path missing"))?;
            ensure!(
                if i + 1 == path.len() {
                    e.mode == b"100644"
                } else {
                    e.mode == b"40000"
                },
                "invalid owned ledger mode"
            );
            id = e.oid.clone();
        }
        Ok(id)
    }

    fn ownership(&mut self, base: &Commit, change: &PreparedLedgerChange) -> Result<()> {
        self.git.check()?;
        let json = base
            .message
            .strip_prefix(MARKER.as_bytes())
            .ok_or_else(|| anyhow!("foreign state base commit: owned receipt required"))?;
        let record: Record = serde_json::from_slice(json)
            .map_err(|_| anyhow!("malformed state ownership receipt"))?;
        record.validate(self.len)?;
        ensure!(
            record.message()? == base.message
                && record.path == change.path
                && record.binding == change.binding
                && Some(&record.ledger_blob_id) == change.expected_blob_id.as_ref()
                && base.parents == [record.base_commit.clone()],
            "state ownership receipt mismatch"
        );
        let path: Vec<_> = change.path.split('/').collect();
        ensure!(
            self.lookup(&base.tree, &path)? == record.ledger_blob_id,
            "owned ledger tree/blob mismatch"
        );
        self.read("blob", &record.ledger_blob_id)?;
        let parent = self.commit(&record.base_commit)?;
        let time = next_time(parent.time)?;
        ensure!(
            base.bytes == commit_bytes(&base.tree, &record.base_commit, time, &base.message),
            "noncanonical owned state commit"
        );
        // Prove that even an owned-looking commit changed only its ledger path.
        let n = self.objects.len();
        let expected_tree = self.replace(
            Some(&parent.tree),
            &path,
            &record.ledger_blob_id,
            record.expected_blob_id.as_deref(),
            0,
        )?;
        self.objects.truncate(n);
        ensure!(
            expected_tree == base.tree,
            "owned state commit has unrelated tree changes"
        );
        Ok(())
    }
}

fn parse_time(line: &str) -> Result<i64> {
    let (identity, tail) = line
        .rsplit_once("> ")
        .ok_or_else(|| anyhow!("invalid commit identity/time"))?;
    ensure!(identity.contains(" <"), "invalid commit identity");
    let (seconds, zone) = tail
        .split_once(' ')
        .ok_or_else(|| anyhow!("invalid commit time"))?;
    ensure!(
        !seconds.is_empty()
            && seconds.bytes().all(|b| b.is_ascii_digit())
            && zone.len() == 5
            && matches!(zone.as_bytes()[0], b'+' | b'-')
            && zone[1..].bytes().all(|b| b.is_ascii_digit())
            && zone[1..3].parse::<u8>()? <= 23
            && zone[3..].parse::<u8>()? <= 59,
        "invalid commit time"
    );
    let time: i64 = seconds
        .parse()
        .map_err(|_| anyhow!("invalid commit timestamp"))?;
    ensure!(
        (0..=253_402_300_799).contains(&time),
        "unsupported commit timestamp"
    );
    Ok(time)
}

fn next_time(time: i64) -> Result<i64> {
    ensure!(
        (0..253_402_300_799).contains(&time),
        "state commit timestamp overflow"
    );
    Ok(time + 1)
}

fn commit_bytes(tree: &str, parent: &str, time: i64, message: &[u8]) -> Vec<u8> {
    let mut bytes = format!("tree {tree}\nparent {parent}\nauthor {IDENTITY} {time} +0000\ncommitter {IDENTITY} {time} +0000\n\n").into_bytes();
    bytes.extend_from_slice(message);
    bytes
}

fn parse_tree(mut bytes: &[u8], len: usize) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    let mut names = BTreeSet::new();
    let mut previous = None;
    while !bytes.is_empty() {
        let space = bytes
            .iter()
            .position(|b| *b == b' ')
            .ok_or_else(|| anyhow!("malformed raw Git tree"))?;
        let mode = &bytes[..space];
        ensure!(
            matches!(
                mode,
                b"40000" | b"100644" | b"100755" | b"120000" | b"160000"
            ),
            "invalid raw Git tree mode"
        );
        bytes = &bytes[space + 1..];
        let nul = bytes
            .iter()
            .position(|b| *b == 0)
            .ok_or_else(|| anyhow!("malformed raw Git tree name"))?;
        let name = &bytes[..nul];
        ensure!(
            !name.is_empty()
                && name.len() <= 4096
                && !name.contains(&b'/')
                && name != b"."
                && name != b".."
                && !name.eq_ignore_ascii_case(b".git")
                && names.insert(name.to_vec()),
            "invalid or duplicate raw Git tree name"
        );
        bytes = &bytes[nul + 1..];
        ensure!(bytes.len() >= len / 2, "truncated raw Git tree object ID");
        let id: String = bytes[..len / 2]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        ensure!(oid(&id, len), "zero raw Git tree object ID");
        bytes = &bytes[len / 2..];
        let e = Entry {
            mode: mode.to_vec(),
            name: name.to_vec(),
            oid: id,
        };
        let key = e.key();
        ensure!(
            previous.as_ref().is_none_or(|p| p < &key),
            "noncanonical raw Git tree order"
        );
        previous = Some(key);
        entries.push(e);
        ensure!(
            entries.len() <= ENTRY_CAP,
            "state tree entry limit exceeded"
        );
    }
    Ok(entries)
}

fn encode_tree(entries: &[Entry]) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for e in entries {
        bytes.extend_from_slice(&e.mode);
        bytes.push(b' ');
        bytes.extend_from_slice(&e.name);
        bytes.push(0);
        for i in (0..e.oid.len()).step_by(2) {
            bytes.push(
                u8::from_str_radix(&e.oid[i..i + 2], 16)
                    .map_err(|_| anyhow!("invalid tree object ID"))?,
            );
        }
    }
    Ok(bytes)
}

fn prepare(
    repo: &Path,
    state_ref: &str,
    change: &PreparedLedgerChange,
    create_ref: bool,
    deadline: Instant,
    operation: &SourceSession,
) -> Result<(Preparation, ApplyReceipt)> {
    operation.check()?;
    ensure!(
        state_ref == STATE_REF,
        "only refs/heads/hive-calendar-state is supported"
    );
    ensure!(
        repo.is_absolute() && metadata(repo)?.is_some_and(|m| m.is_dir()),
        "state repository must be an absolute directory, not a symlink"
    );
    ensure!(valid_path(&change.path), "invalid ledger JSON path");
    ensure!(
        create_ref == change.expected_blob_id.is_none(),
        "create-ref must exactly match ledger initialization"
    );
    let git = GitSession::with_operation(repo, deadline, operation)?;
    let len = repository(&git)?;
    ensure!(
        oid(&change.base_commit, len),
        "base commit must be a full storage-format object ID"
    );
    let mut p = Preparation {
        git,
        len,
        trees: BTreeMap::new(),
        bytes: 0,
        entries: 0,
        objects: Vec::new(),
    };
    let base = p.commit(&change.base_commit)?;
    p.tree(&base.tree, 0)?;
    if change.expected_blob_id.is_some() {
        p.ownership(&base, change)?;
    } else {
        ensure!(
            !base
                .message
                .windows(b"hive-calendar-state".len())
                .any(|w| w.eq_ignore_ascii_case(b"hive-calendar-state")),
            "owned or malformed receipt cannot seed initialization"
        );
    }
    let blob = p.object("blob", change.candidate_ledger.clone())?;
    let record = Record {
        schema_version: "1".into(),
        state_ref: state_ref.into(),
        path: change.path.clone(),
        binding: change.binding.clone(),
        ledger_blob_id: blob.clone(),
        candidate_sha256: change.candidate_sha256.clone(),
        source_commit: change.source_commit.clone(),
        operation: match change.operation {
            CandidateOperation::Enroll => "enroll",
            CandidateOperation::Reack => "reack",
        }
        .into(),
        logical_id: change.logical_id.clone(),
        parser_change_accepted: change.parser_change_accepted,
        base_commit: change.base_commit.clone(),
        expected_blob_id: change.expected_blob_id.clone(),
    };
    record.validate(len)?;
    let path: Vec<_> = change.path.split('/').collect();
    let tree = p.replace(
        Some(&base.tree),
        &path,
        &blob,
        change.expected_blob_id.as_deref(),
        0,
    )?;
    let commit = p.object(
        "commit",
        commit_bytes(
            &tree,
            &change.base_commit,
            next_time(base.time)?,
            &record.message()?,
        ),
    )?;
    let tip = current(&p.git, len)?;
    let already = tip.as_deref() == Some(commit.as_str());
    ensure!(
        already
            || if create_ref {
                tip.is_none()
            } else {
                tip.as_deref() == Some(change.base_commit.as_str())
            },
        "state ref conflict: exact base head required; never rewind an advanced ref"
    );
    p.git.check()?;
    let receipt = ApplyReceipt {
        schema_version: "1",
        status: if already {
            "already_applied"
        } else {
            "verified"
        },
        state_ref: state_ref.into(),
        ledger_path: change.path.clone(),
        binding: change.binding.clone(),
        candidate_sha256: change.candidate_sha256.clone(),
        source_commit: change.source_commit.clone(),
        base_commit: change.base_commit.clone(),
        commit,
        ledger_blob_id: blob,
        operation: change.operation,
        logical_id: change.logical_id.clone(),
        parser_change_accepted: change.parser_change_accepted,
        local_ref_updated: false,
        publication_ready: false,
        remote_verified: false,
        remote_ref_verified: false,
    };
    Ok((p, receipt))
}

pub(crate) fn verify(
    repo: &Path,
    state_ref: &str,
    change: &PreparedLedgerChange,
    create_ref: bool,
    deadline: Instant,
    operation: &SourceSession,
) -> Result<ApplyReceipt> {
    let (_, receipt) = prepare(repo, state_ref, change, create_ref, deadline, operation)?;
    operation.check()?;
    Ok(receipt)
}

pub(crate) fn apply(
    repo: &Path,
    state_ref: &str,
    change: &PreparedLedgerChange,
    create_ref: bool,
    deadline: Instant,
    operation: &SourceSession,
) -> Result<ApplyReceipt> {
    let (p, mut receipt) = prepare(repo, state_ref, change, create_ref, deadline, operation)?;
    p.git.check()?;
    if receipt.status == "already_applied" {
        return Ok(receipt);
    }
    for object in &p.objects {
        ensure!(
            p.hash(object.kind, &object.bytes, true)? == object.oid,
            "written state object ID mismatch"
        );
    }
    ensure!(repository(&p.git)? == p.len, "state object format changed");
    let tip = current(&p.git, p.len)?;
    p.git.check()?;
    if tip.as_deref() == Some(receipt.commit.as_str()) {
        receipt.status = "already_applied";
        return Ok(receipt);
    }
    ensure!(
        if create_ref {
            tip.is_none()
        } else {
            tip.as_deref() == Some(change.base_commit.as_str())
        },
        "state ref conflict before CAS"
    );
    #[cfg(all(test, unix))]
    tests::observe_write(tests::WritePhase::BeforeCas)?;
    p.git.check()?;
    let old = if create_ref {
        "0".repeat(p.len)
    } else {
        change.base_commit.clone()
    };
    // From this point any transport error/cancellation may be a lost response.
    // Never roll back: an exact retry reconstructs the same commit and reports
    // already_applied, or refuses if another writer has subsequently advanced it.
    let outcome = p.git.run(
        &["update-ref", "--no-deref", STATE_REF, &receipt.commit, &old],
        b"",
        1024,
    );
    let outcome = outcome.and_then(|out| {
        #[cfg(all(test, unix))]
        tests::observe_write(tests::WritePhase::AfterCas)?;
        p.git.check()?;
        Ok(out)
    });
    outcome
        .map_err(|_| anyhow!("state ref CAS outcome unknown (possibly conflict or cancellation); retry the exact same candidate and options to determine already_applied; never roll back"))?;
    receipt.status = "applied";
    receipt.local_ref_updated = true;
    Ok(receipt)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        os::unix::fs::{PermissionsExt, symlink},
        path::PathBuf,
        process::Command,
        time::Duration,
    };

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(120)
    }

    // Existing unit callers still own a root for the entire writer call.
    fn verify(
        repo: &Path,
        state_ref: &str,
        change: &PreparedLedgerChange,
        create_ref: bool,
        deadline: Instant,
    ) -> Result<ApplyReceipt> {
        let operation = SourceSession::operation(deadline)?;
        super::verify(repo, state_ref, change, create_ref, deadline, &operation)
    }

    fn apply(
        repo: &Path,
        state_ref: &str,
        change: &PreparedLedgerChange,
        create_ref: bool,
        deadline: Instant,
    ) -> Result<ApplyReceipt> {
        let operation = SourceSession::operation(deadline)?;
        super::apply(repo, state_ref, change, create_ref, deadline, &operation)
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub(super) enum WritePhase {
        BeforeCas,
        AfterCas,
    }

    type Observer = Box<dyn FnMut(WritePhase) -> Result<()>>;
    thread_local! {
        static WRITE_OBSERVER: std::cell::RefCell<Option<Observer>> = const { std::cell::RefCell::new(None) };
    }

    pub(super) fn observe_write(phase: WritePhase) -> Result<()> {
        WRITE_OBSERVER.with_borrow_mut(|observer| match observer.as_mut() {
            Some(f) => f(phase),
            None => Ok(()),
        })
    }

    fn observing<T>(
        observer: impl FnMut(WritePhase) -> Result<()> + 'static,
        run: impl FnOnce() -> T,
    ) -> T {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                WRITE_OBSERVER.with_borrow_mut(|o| *o = None);
            }
        }
        WRITE_OBSERVER.with_borrow_mut(|o| {
            assert!(o.is_none());
            *o = Some(Box::new(observer));
        });
        let _reset = Reset;
        run()
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        repo: PathBuf,
        base: String,
    }

    impl Fixture {
        fn new(format: &str) -> Self {
            let temp = tempfile::tempdir_in("/tmp").unwrap();
            let repo = temp.path().canonicalize().unwrap().join("state.git");
            let out = Command::new("/usr/bin/git")
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", temp.path())
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .args([
                    "-c",
                    "core.hooksPath=/dev/null",
                    "init",
                    "--bare",
                    "--quiet",
                    &format!("--object-format={format}"),
                ])
                .arg(&repo)
                .output()
                .unwrap();
            assert!(out.status.success());
            let mut f = Self {
                _temp: temp,
                repo,
                base: String::new(),
            };
            let json = f.write("blob", b"{}\n");
            let executable = f.write("blob", b"#!/bin/sh\nexit 0\n");
            let link = f.write("blob", b"empty.json");
            let subtree = f.write(
                "tree",
                &encode_tree(&[Entry {
                    mode: b"100644".to_vec(),
                    name: b"empty.json".to_vec(),
                    oid: json.clone(),
                }])
                .unwrap(),
            );
            let mut entries = vec![
                Entry {
                    mode: b"40000".to_vec(),
                    name: b"dir".to_vec(),
                    oid: subtree,
                },
                Entry {
                    mode: b"100644".to_vec(),
                    name: b"empty.json".to_vec(),
                    oid: json,
                },
                Entry {
                    mode: b"100755".to_vec(),
                    name: b"executable".to_vec(),
                    oid: executable,
                },
                Entry {
                    mode: b"120000".to_vec(),
                    name: b"link".to_vec(),
                    oid: link,
                },
            ];
            entries.sort_by_key(Entry::key);
            let tree = f.write("tree", &encode_tree(&entries).unwrap());
            f.base = f.seed(&tree, "seed fixture\n", 1_700_000_000);
            f.run(&["update-ref", "refs/heads/unrelated", &f.base], b"");
            f
        }

        fn run(&self, args: &[&str], input: &[u8]) -> Vec<u8> {
            GitSession::before(&self.repo, deadline())
                .unwrap()
                .run(args, input, GRAPH_CAP)
                .unwrap()
        }

        fn write(&self, kind: &str, bytes: &[u8]) -> String {
            String::from_utf8(self.run(
                &["hash-object", "--no-filters", "-w", "-t", kind, "--stdin"],
                bytes,
            ))
            .unwrap()
            .trim()
            .into()
        }

        fn seed(&self, tree: &str, message: &str, time: i64) -> String {
            self.write("commit", format!("tree {tree}\nauthor Fixture <fixture@example.invalid> {time} +0000\ncommitter Fixture <fixture@example.invalid> {time} +0000\n\n{message}").as_bytes())
        }

        fn change(&self) -> PreparedLedgerChange {
            PreparedLedgerChange {
                candidate_sha256: "a".repeat(64),
                operation: CandidateOperation::Enroll,
                logical_id: "0123456789abcdef0123456789abcdef".into(),
                source_commit: self.base.clone(),
                binding: Binding {
                    provider: "google_calendar".into(),
                    account_id: format!("google-sub-v1:{}", "1".repeat(64)),
                    calendar_id: "fixture-calendar".into(),
                    source_repo_id: "fixture-source".into(),
                },
                parser_change_accepted: false,
                base_commit: self.base.clone(),
                path: "state/nested/ledger.json".into(),
                expected_blob_id: None,
                candidate_ledger: b"{\"fixture\":1}\n".to_vec(),
            }
        }

        fn update(&self, receipt: &ApplyReceipt) -> PreparedLedgerChange {
            PreparedLedgerChange {
                base_commit: receipt.commit.clone(),
                expected_blob_id: Some(receipt.ledger_blob_id.clone()),
                candidate_sha256: "b".repeat(64),
                operation: CandidateOperation::Reack,
                candidate_ledger: b"{\"fixture\":2}\n".to_vec(),
                ..self.change()
            }
        }

        fn tip(&self) -> Option<String> {
            current(
                &GitSession::before(&self.repo, deadline()).unwrap(),
                self.base.len(),
            )
            .unwrap()
        }
    }

    fn snapshot(path: &Path) -> BTreeMap<PathBuf, (u32, Vec<u8>)> {
        fn visit(root: &Path, path: &Path, map: &mut BTreeMap<PathBuf, (u32, Vec<u8>)>) {
            let m = fs::symlink_metadata(path).unwrap();
            let data = if m.file_type().is_symlink() {
                fs::read_link(path)
                    .unwrap()
                    .as_os_str()
                    .as_encoded_bytes()
                    .to_vec()
            } else if m.is_file() {
                fs::read(path).unwrap()
            } else {
                Vec::new()
            };
            map.insert(
                path.strip_prefix(root).unwrap().to_owned(),
                (m.permissions().mode(), data),
            );
            if m.is_dir() {
                for entry in fs::read_dir(path).unwrap() {
                    visit(root, &entry.unwrap().path(), map);
                }
            }
        }
        let mut map = BTreeMap::new();
        visit(path, path, &mut map);
        map
    }

    #[test]
    #[ignore = "internal subprocess fixture, invoked by the phase-boundary signal test"]
    fn root_cancellation_phase_boundary_child() {
        let control = PathBuf::from(std::env::var_os("HIVE_STATE_CANCEL_CONTROL").unwrap());
        let f = Fixture::new("sha1");
        let end = deadline();
        let operation = SourceSession::operation(end).unwrap();
        // Each helper has returned and unregistered its own actions. The root
        // must still catch a signal in the CPU/phase gap before a writer exists.
        crate::source::require_json_absent_before(&f.repo, &f.base, "missing.json", end).unwrap();
        crate::source::read_json_blob_before(&f.repo, &f.base, "empty.json", end).unwrap();
        crate::source::require_ancestor_before(&f.repo, &f.base, &f.base, end).unwrap();
        let before = snapshot(&f.repo);
        fs::write(control.join("ready"), b"helpers returned").unwrap();
        while !control.join("resume").exists() {
            assert!(Instant::now() < end, "parent never released phase pause");
            std::thread::sleep(Duration::from_millis(2));
        }
        // Delivery is asynchronous with the parent's release-file write.
        while operation.check().is_ok() {
            assert!(
                Instant::now() < end,
                "root missed the phase-boundary signal"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        let change = f.change();
        let checked =
            super::verify(&f.repo, STATE_REF, &change, true, end, &operation).unwrap_err();
        let applied = super::apply(&f.repo, STATE_REF, &change, true, end, &operation).unwrap_err();
        assert_eq!(checked.to_string(), "source operation cancelled");
        assert_eq!(applied.to_string(), "source operation cancelled");
        assert_eq!(
            snapshot(&f.repo),
            before,
            "cancelled phase wrote objects or refs"
        );
        drop(operation);
        fs::write(control.join("finished"), b"cancelled without writes").unwrap();
    }

    #[test]
    fn sigint_and_sigterm_between_helpers_and_writer_are_not_lost() {
        use std::process::{Child, Stdio};
        struct ChildGuard(Option<Child>);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                if let Some(child) = &mut self.0 {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
        for signal in [libc::SIGINT, libc::SIGTERM] {
            let control = tempfile::tempdir_in("/tmp").unwrap();
            let mut child = ChildGuard(Some(
                Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "state_git::tests::root_cancellation_phase_boundary_child",
                        "--ignored",
                        "--nocapture",
                    ])
                    .env_clear()
                    .env("HIVE_STATE_CANCEL_CONTROL", control.path())
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap(),
            ));
            let end = Instant::now() + Duration::from_secs(20);
            while !control.path().join("ready").exists() {
                assert!(
                    Instant::now() < end,
                    "phase-boundary child did not reach pause"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            // Only our own unreaped child is signaled, never the test runner.
            assert_eq!(
                unsafe { libc::kill(child.0.as_ref().unwrap().id() as libc::pid_t, signal) },
                0
            );
            fs::write(control.path().join("resume"), b"continue").unwrap();
            let status = loop {
                if let Some(status) = child.0.as_mut().unwrap().try_wait().unwrap() {
                    child.0.take();
                    break status;
                }
                assert!(
                    Instant::now() < end,
                    "phase-boundary child failed to unwind"
                );
                std::thread::sleep(Duration::from_millis(5));
            };
            assert!(status.success(), "phase-boundary cancellation child failed");
            assert_eq!(
                fs::read(control.path().join("finished")).unwrap(),
                b"cancelled without writes"
            );
        }
    }

    #[test]
    fn root_cancellation_at_pre_cas_stops_the_actual_writer() {
        for initialize in [true, false] {
            let f = Fixture::new("sha1");
            let change = if initialize {
                f.change()
            } else {
                let first = apply(&f.repo, STATE_REF, &f.change(), true, deadline()).unwrap();
                f.update(&first)
            };
            let expected = verify(&f.repo, STATE_REF, &change, initialize, deadline()).unwrap();
            let old_tip = f.tip();
            let end = deadline();
            let operation = SourceSession::operation(end).unwrap();
            let cancellation = operation.child(end).unwrap();
            let reached = std::rc::Rc::new(std::cell::Cell::new(false));
            let observed = reached.clone();
            let error = observing(
                move |phase| {
                    assert!(
                        phase == WritePhase::BeforeCas,
                        "CAS ran after root cancellation"
                    );
                    observed.set(true);
                    cancellation.cancel();
                    Ok(())
                },
                || super::apply(&f.repo, STATE_REF, &change, initialize, end, &operation),
            )
            .unwrap_err();
            assert!(reached.get());
            assert_eq!(error.to_string(), "source operation cancelled");
            assert!(operation.check().is_err());
            assert_eq!(f.tip(), old_tip, "cancelled writer performed CAS");
            // Object writes really completed before the pre-CAS observation.
            assert_eq!(
                f.run(&["cat-file", "-t", &expected.commit], b""),
                b"commit\n"
            );
        }
    }

    #[test]
    fn cancellation_after_a_real_successful_cas_reports_unknown_and_exact_retry() {
        let f = Fixture::new("sha1");
        let change = f.change();
        let expected = verify(&f.repo, STATE_REF, &change, true, deadline()).unwrap();
        let end = deadline();
        let operation = SourceSession::operation(end).unwrap();
        let cancellation = operation.child(end).unwrap();
        let won = std::rc::Rc::new(std::cell::Cell::new(false));
        let observed = won.clone();
        let error = observing(
            move |phase| {
                if phase == WritePhase::AfterCas {
                    // The real hardened Git transport has already returned success:
                    // the actual transaction won. Inject only its lost response.
                    observed.set(true);
                    cancellation.cancel();
                }
                Ok(())
            },
            || super::apply(&f.repo, STATE_REF, &change, true, end, &operation),
        )
        .unwrap_err();
        assert!(won.get());
        assert!(operation.check().is_err());
        assert!(error.to_string().contains("CAS outcome unknown"));
        assert!(error.to_string().contains("retry the exact same candidate"));
        assert_eq!(f.tip().as_deref(), Some(expected.commit.as_str()));
        let before = snapshot(&f.repo);
        let retry = apply(&f.repo, STATE_REF, &change, true, deadline()).unwrap();
        assert_eq!(retry.status, "already_applied");
        assert!(!retry.local_ref_updated);
        assert_eq!(snapshot(&f.repo), before);
    }

    #[test]
    fn initialize_reack_exact_retry_and_verify_are_deterministic_and_read_only() {
        for format in ["sha1", "sha256"] {
            let f = Fixture::new(format);
            let change = f.change();
            let before = snapshot(&f.repo);
            let verified = verify(&f.repo, STATE_REF, &change, true, deadline()).unwrap();
            assert_eq!(verified.status, "verified");
            assert!(!verified.local_ref_updated);
            assert_eq!(snapshot(&f.repo), before);
            let again = verify(&f.repo, STATE_REF, &change, true, deadline()).unwrap();
            assert_eq!(again.commit, verified.commit);
            let applied = apply(&f.repo, STATE_REF, &change, true, deadline()).unwrap();
            assert_eq!(applied.commit, verified.commit);
            assert_eq!(applied.status, "applied");
            assert!(applied.local_ref_updated);
            assert_eq!(f.tip().as_deref(), Some(applied.commit.as_str()));
            assert_eq!(
                f.run(&["rev-parse", "refs/heads/unrelated"], b""),
                format!("{}\n", f.base).as_bytes()
            );
            assert_eq!(
                f.run(
                    &[
                        "ls-tree",
                        &f.base,
                        "--",
                        "dir",
                        "empty.json",
                        "executable",
                        "link"
                    ],
                    b""
                ),
                f.run(
                    &[
                        "ls-tree",
                        &applied.commit,
                        "--",
                        "dir",
                        "empty.json",
                        "executable",
                        "link"
                    ],
                    b""
                )
            );
            assert_eq!(
                f.run(
                    &[
                        "cat-file",
                        "blob",
                        &format!("{}:{}", applied.commit, change.path)
                    ],
                    b""
                ),
                change.candidate_ledger
            );
            let after = snapshot(&f.repo);
            for retry in [
                verify(&f.repo, STATE_REF, &change, true, deadline()).unwrap(),
                apply(&f.repo, STATE_REF, &change, true, deadline()).unwrap(),
            ] {
                assert_eq!(retry.status, "already_applied");
                assert!(!retry.local_ref_updated);
                assert!(
                    !retry.publication_ready
                        && !retry.remote_verified
                        && !retry.remote_ref_verified
                );
            }
            assert_eq!(snapshot(&f.repo), after);
            let update = f.update(&applied);
            let checked = verify(&f.repo, STATE_REF, &update, false, deadline()).unwrap();
            assert_eq!(snapshot(&f.repo), after);
            let updated = apply(&f.repo, STATE_REF, &update, false, deadline()).unwrap();
            assert_eq!(updated.commit, checked.commit);
            assert!(apply(&f.repo, STATE_REF, &change, true, deadline()).is_err());
            assert_eq!(
                apply(&f.repo, STATE_REF, &update, false, deadline())
                    .unwrap()
                    .status,
                "already_applied"
            );
            assert!(!f.repo.join("index").exists());
            assert_eq!(
                fs::read(f.repo.join("config")).unwrap(),
                before[Path::new("config")].1
            );
            let receipt = serde_json::to_string(&updated).unwrap();
            assert!(receipt.contains("\"remoteRefVerified\":false"));
            assert!(!receipt.contains("fixture\\\":2"));
        }
    }

    #[test]
    fn stale_head_is_refused_even_with_identical_ledger_and_foreign_heads_are_never_adopted() {
        let f = Fixture::new("sha1");
        let change = f.change();
        f.run(&["update-ref", STATE_REF, &f.base], b"");
        let before = snapshot(&f.repo);
        assert!(apply(&f.repo, STATE_REF, &change, true, deadline()).is_err());
        assert_eq!(snapshot(&f.repo), before);
        f.run(&["update-ref", "-d", STATE_REF], b"");
        let first = apply(&f.repo, STATE_REF, &change, true, deadline()).unwrap();
        let mut a = f.update(&first);
        a.candidate_ledger = change.candidate_ledger.clone();
        let second = apply(&f.repo, STATE_REF, &a, false, deadline()).unwrap();
        assert_eq!(first.ledger_blob_id, second.ledger_blob_id);
        let mut stale = f.update(&first);
        stale.candidate_sha256 = "c".repeat(64);
        let before = snapshot(&f.repo);
        assert!(apply(&f.repo, STATE_REF, &stale, false, deadline()).is_err());
        assert!(apply(&f.repo, STATE_REF, &change, true, deadline()).is_err());
        assert_eq!(snapshot(&f.repo), before);
        // Manually crafted commit with exactly the ledger is still foreign.
        let tree =
            String::from_utf8(f.run(&["rev-parse", &format!("{}^{{tree}}", first.commit)], b""))
                .unwrap();
        let foreign = f.seed(tree.trim(), "manual foreign state\n", 1_700_000_010);
        f.run(&["update-ref", STATE_REF, &foreign], b"");
        stale.base_commit = foreign;
        assert!(verify(&f.repo, STATE_REF, &stale, false, deadline()).is_err());
    }

    #[test]
    fn unsafe_refs_and_repository_layouts_are_refused_without_writes() {
        for poison in [
            "symbolic",
            "dangling",
            "ref-symlink",
            "refs-symlink",
            "objects-symlink",
            "alternates",
            "worktree",
            "unborn",
            "collision",
            "ordinary",
            "reftable",
            "shared",
        ] {
            let f = Fixture::new("sha1");
            let target = f.repo.join(STATE_REF);
            match poison {
                "symbolic" => {
                    f.run(&["symbolic-ref", STATE_REF, "refs/heads/unrelated"], b"");
                }
                "dangling" => {
                    fs::write(&target, "ref: refs/heads/missing\n").unwrap();
                }
                "ref-symlink" => {
                    symlink("/no/such/ref", &target).unwrap();
                }
                "refs-symlink" | "objects-symlink" => {
                    let name = if poison == "refs-symlink" {
                        "refs"
                    } else {
                        "objects"
                    };
                    fs::rename(f.repo.join(name), f.repo.join(format!("{name}-saved"))).unwrap();
                    symlink(format!("{name}-saved"), f.repo.join(name)).unwrap();
                }
                "alternates" => {
                    fs::write(f.repo.join("objects/info/alternates"), "/no/such/objects\n")
                        .unwrap();
                }
                "worktree" | "unborn" => {
                    fs::create_dir_all(f.repo.join("worktrees/registered")).unwrap();
                    fs::write(
                        f.repo.join("worktrees/registered/HEAD"),
                        if poison == "unborn" {
                            "ref: refs/heads/not-yet-born\n".into()
                        } else {
                            format!("{}\n", f.base)
                        },
                    )
                    .unwrap();
                }
                "collision" => {
                    f.run(&["update-ref", &format!("{STATE_REF}/child"), &f.base], b"");
                }
                "ordinary" => {
                    f.run(&["config", "core.bare", "false"], b"");
                }
                "reftable" => {
                    f.run(&["refs", "migrate", "--ref-format=reftable"], b"");
                }
                "shared" => {
                    fs::set_permissions(f.repo.join("objects"), fs::Permissions::from_mode(0o777))
                        .unwrap();
                }
                _ => unreachable!(),
            }
            let before = snapshot(&f.repo);
            assert!(
                verify(&f.repo, STATE_REF, &f.change(), true, deadline()).is_err(),
                "{poison}"
            );
            assert!(
                apply(&f.repo, STATE_REF, &f.change(), true, deadline()).is_err(),
                "{poison}"
            );
            assert_eq!(snapshot(&f.repo), before, "{poison}");
        }
    }

    #[test]
    fn real_linked_worktrees_including_unborn_and_checked_out_state_are_refused() {
        for unborn in [false, true] {
            let f = Fixture::new("sha1");
            let worktree = f.repo.parent().unwrap().join("worktree");
            if unborn {
                f.run(
                    &[
                        "worktree",
                        "add",
                        "--orphan",
                        "-b",
                        "unborn",
                        worktree.to_str().unwrap(),
                    ],
                    b"",
                );
            } else {
                f.run(&["update-ref", STATE_REF, &f.base], b"");
                f.run(
                    &[
                        "worktree",
                        "add",
                        worktree.to_str().unwrap(),
                        "hive-calendar-state",
                    ],
                    b"",
                );
            }
            let before = snapshot(f.repo.parent().unwrap());
            assert!(verify(&f.repo, STATE_REF, &f.change(), true, deadline()).is_err());
            assert!(apply(&f.repo, STATE_REF, &f.change(), true, deadline()).is_err());
            assert!(apply(&worktree, STATE_REF, &f.change(), true, deadline()).is_err());
            assert_eq!(snapshot(f.repo.parent().unwrap()), before);
        }
    }

    #[test]
    fn existing_nested_parent_preserves_siblings_and_malformed_local_objects_fail_closed() {
        let f = Fixture::new("sha1");
        let mut c = f.change();
        c.path = "dir/new/ledger.json".into();
        let base_sibling = f.run(&["ls-tree", &f.base, "--", "dir/empty.json"], b"");
        let applied = apply(&f.repo, STATE_REF, &c, true, deadline()).unwrap();
        assert_eq!(
            f.run(&["ls-tree", &applied.commit, "--", "dir/empty.json"], b""),
            base_sibling
        );
        f.run(&["update-ref", "-d", STATE_REF], b"");
        for kind in ["missing-blob", "wrong-type", "truncated-tree", "bad-commit"] {
            let tree = if kind == "truncated-tree" {
                String::from_utf8(f.run(
                    &["hash-object", "--literally", "-w", "-t", "tree", "--stdin"],
                    b"100644 bad\0abc",
                ))
                .unwrap()
                .trim()
                .to_owned()
            } else {
                let entry = Entry {
                    mode: b"100644".to_vec(),
                    name: b"bad".to_vec(),
                    oid: if kind == "missing-blob" {
                        "f".repeat(40)
                    } else {
                        f.base.clone()
                    },
                };
                f.write("tree", &encode_tree(&[entry]).unwrap())
            };
            c.base_commit = if kind == "bad-commit" {
                String::from_utf8(f.run(
                    &[
                        "hash-object",
                        "--literally",
                        "-w",
                        "-t",
                        "commit",
                        "--stdin",
                    ],
                    b"not a commit\n",
                ))
                .unwrap()
                .trim()
                .to_owned()
            } else {
                f.seed(&tree, "malformed fixture\n", 1_700_000_000)
            };
            let before = snapshot(&f.repo);
            assert!(
                apply(&f.repo, STATE_REF, &c, true, deadline()).is_err(),
                "{kind}"
            );
            assert_eq!(snapshot(&f.repo), before);
        }
    }

    #[test]
    fn packed_refs_work_and_hooks_never_run() {
        let f = Fixture::new("sha1");
        let sentinel = f.repo.join("hook-ran");
        for name in [
            "reference-transaction",
            "pre-commit",
            "post-commit",
            "post-update",
        ] {
            let path = f.repo.join("hooks").join(name);
            fs::write(
                &path,
                format!(
                    "#!/bin/sh\n/usr/bin/touch '{}'\nexit 1\n",
                    sentinel.display()
                ),
            )
            .unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let first = apply(&f.repo, STATE_REF, &f.change(), true, deadline()).unwrap();
        f.run(&["pack-refs", "--all"], b"");
        assert!(!f.repo.join(STATE_REF).exists());
        apply(&f.repo, STATE_REF, &f.update(&first), false, deadline()).unwrap();
        assert!(!sentinel.exists());
    }

    #[test]
    fn competing_writers_use_whole_head_cas_and_lost_response_retries_are_exact() {
        let f = Fixture::new("sha1");
        let first = apply(&f.repo, STATE_REF, &f.change(), true, deadline()).unwrap();
        let barrier = std::sync::Barrier::new(2);
        let outcomes = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..2)
                .map(|i| {
                    let mut change = f.update(&first);
                    change.candidate_sha256 = if i == 0 { "c" } else { "d" }.repeat(64);
                    let f = &f;
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        apply(&f.repo, STATE_REF, &change, false, deadline())
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(outcomes.iter().filter(|r| r.is_ok()).count(), 1);
        let winner = outcomes.into_iter().find_map(Result::ok).unwrap();
        let mut exact = f.update(&first);
        exact.candidate_sha256 = winner.candidate_sha256.clone();
        let before = snapshot(&f.repo);
        // Ignore the original receipt, as if the caller lost the CAS response.
        let retry = apply(&f.repo, STATE_REF, &exact, false, deadline()).unwrap();
        assert_eq!(retry.status, "already_applied");
        assert!(!retry.local_ref_updated);
        assert_eq!(snapshot(&f.repo), before);
    }

    #[test]
    fn invalid_parents_blobs_paths_receipts_and_timestamps_fail_before_writes() {
        let f = Fixture::new("sha1");
        for path in [
            "empty.json/ledger.json",
            "link/ledger.json",
            "executable/ledger.json",
            "dir",
            "../ledger.json",
            ".git/ledger.json",
            "a//ledger.json",
            "empty.json",
        ] {
            let mut c = f.change();
            c.path = path.into();
            let before = snapshot(&f.repo);
            assert!(
                apply(&f.repo, STATE_REF, &c, true, deadline()).is_err(),
                "{path}"
            );
            assert_eq!(snapshot(&f.repo), before);
        }
        for (create, expected) in [(false, None), (true, Some("a".repeat(40)))] {
            let mut c = f.change();
            c.expected_blob_id = expected;
            assert!(verify(&f.repo, STATE_REF, &c, create, deadline()).is_err());
        }
        assert!(verify(&f.repo, "refs/heads/other", &f.change(), true, deadline()).is_err());
        assert!(verify(&f.repo, STATE_REF, &f.change(), true, Instant::now()).is_err());
        let applied = apply(&f.repo, STATE_REF, &f.change(), true, deadline()).unwrap();
        let original = f.run(&["cat-file", "commit", &applied.commit], b"");
        for corruption in ["marker", "receipt", "identity", "unrelated"] {
            let mut bytes = original.clone();
            match corruption {
                "marker" => {
                    let p = bytes
                        .windows(MARKER.len())
                        .position(|w| w == MARKER.as_bytes())
                        .unwrap();
                    bytes[p] = b'H';
                }
                "receipt" => {
                    let p = bytes
                        .windows(64)
                        .position(|w| w == "a".repeat(64).as_bytes())
                        .unwrap();
                    bytes[p] = b'z';
                }
                "identity" => {
                    let p = bytes
                        .windows(IDENTITY.len())
                        .position(|w| w == IDENTITY.as_bytes())
                        .unwrap();
                    bytes[p] = b'h';
                }
                "unrelated" => {
                    let tree = String::from_utf8(
                        f.run(&["rev-parse", &format!("{}^{{tree}}", applied.commit)], b""),
                    )
                    .unwrap();
                    let raw = f.run(&["cat-file", "tree", tree.trim()], b"");
                    let mut entries = parse_tree(&raw, 40).unwrap();
                    entries.retain(|e| e.name != b"executable");
                    let replacement = f.write("tree", &encode_tree(&entries).unwrap());
                    bytes.splice(5..45, replacement.bytes());
                }
                _ => unreachable!(),
            }
            let foreign = f.write("commit", &bytes);
            f.run(&["update-ref", STATE_REF, &foreign], b"");
            let mut c = f.update(&applied);
            c.base_commit = foreign;
            let before = snapshot(&f.repo);
            assert!(
                apply(&f.repo, STATE_REF, &c, false, deadline()).is_err(),
                "{corruption}"
            );
            assert_eq!(snapshot(&f.repo), before);
        }
        assert!(next_time(253_402_300_799).is_err());
        assert!(parse_time("committer X <x@y> -1 +0000").is_err());
        assert!(parse_time("committer X <x@y> 1 +2460").is_err());
    }

    #[test]
    fn raw_tree_encoding_rejects_duplicates_bad_order_modes_and_truncation() {
        let id = "a".repeat(40);
        let e = |name: &[u8], mode: &[u8]| Entry {
            name: name.to_vec(),
            mode: mode.to_vec(),
            oid: id.clone(),
        };
        for entries in [
            vec![e(b"a", b"100644"), e(b"a", b"40000")],
            vec![e(b"z", b"100644"), e(b"a", b"100644")],
            vec![e(b"a/b", b"100644")],
            vec![e(b"..", b"100644")],
            vec![e(b"a", b"040000")],
        ] {
            assert!(parse_tree(&encode_tree(&entries).unwrap(), 40).is_err());
        }
        assert!(parse_tree(b"100644 a\0abc", 40).is_err());
        let entries = vec![
            e(b"foo.bar", b"100644"),
            e(b"foo", b"40000"),
            e(b"foo0", b"100644"),
        ];
        let raw = encode_tree(&entries).unwrap();
        assert_eq!(encode_tree(&parse_tree(&raw, 40).unwrap()).unwrap(), raw);
    }

    #[test]
    fn receipt_wire_encoding_is_frozen() {
        let record = Record {
            schema_version: "1".into(),
            state_ref: STATE_REF.into(),
            path: "ledger.json".into(),
            binding: Binding {
                provider: "google-calendar".into(),
                account_id: "a".into(),
                calendar_id: "c".into(),
                source_repo_id: "s".into(),
            },
            ledger_blob_id: "1".repeat(40),
            candidate_sha256: "2".repeat(64),
            source_commit: "3".repeat(40),
            operation: "enroll".into(),
            logical_id: "id".into(),
            parser_change_accepted: false,
            base_commit: "4".repeat(40),
            expected_blob_id: None,
        };
        let expected = concat!(
            "hive-calendar-state v1\n\n",
            "{\"schemaVersion\":\"1\",\"stateRef\":\"refs/heads/hive-calendar-state\",\"path\":\"ledger.json\",",
            "\"binding\":{\"provider\":\"google-calendar\",\"accountId\":\"a\",\"calendarId\":\"c\",\"sourceRepoId\":\"s\"},",
            "\"ledgerBlobId\":\"1111111111111111111111111111111111111111\",",
            "\"candidateSha256\":\"2222222222222222222222222222222222222222222222222222222222222222\",",
            "\"sourceCommit\":\"3333333333333333333333333333333333333333\",\"operation\":\"enroll\",",
            "\"logicalId\":\"id\",\"parserChangeAccepted\":false,\"baseCommit\":\"4444444444444444444444444444444444444444\",\"expectedBlobId\":null}\n"
        );
        assert_eq!(record.message().unwrap(), expected.as_bytes());
        let bytes = commit_bytes(
            &"5".repeat(40),
            &record.base_commit,
            1_700_000_001,
            expected.as_bytes(),
        );
        let header = concat!(
            "tree 5555555555555555555555555555555555555555\nparent 4444444444444444444444444444444444444444\n",
            "author Hive Calendar <hive-calendar@localhost> 1700000001 +0000\n",
            "committer Hive Calendar <hive-calendar@localhost> 1700000001 +0000\n\n"
        );
        assert_eq!(bytes, [header.as_bytes(), expected.as_bytes()].concat());
    }
}

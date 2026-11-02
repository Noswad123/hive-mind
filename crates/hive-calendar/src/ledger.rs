//! Versioned publication evidence, not a credential store or a provider client.
use anyhow::{Result, anyhow, ensure};
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fmt};

use crate::ProspectiveEvent;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Binding {
    pub provider: String,
    pub account_id: String,
    pub calendar_id: String,
    pub source_repo_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Ledger {
    pub schema_version: String,
    pub binding: Binding,
    pub source_watermark: String,
    pub entries: Vec<LedgerEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LedgerEntry {
    pub logical_id: String,
    pub lifecycle: Lifecycle,
    pub source_ack: SourceAck,
    pub provider_ack: Option<ProviderAck>,
    pub pending: Option<PendingOperation>,
    pub tombstone: Option<Tombstone>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Enrolled,
    Active,
    Retired,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceAck {
    pub commit: String,
    pub parser_sha256: String,
    pub path: String,
    pub source_id: String,
    pub line: usize,
    pub mw_id: String,
    #[serde(rename = "gitBlobID")]
    pub git_blob_id: String,
    pub source_revision: String,
    pub stable_id: Option<String>,
    pub expected: ExpectedTask,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExpectedTask {
    pub text: String,
    pub literal_text: String,
    pub done: bool,
    pub due: String,
    pub raw: String,
    pub start: String,
    pub estimate: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderAck {
    pub event_id: String,
    pub etag: String,
    pub last_payload_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PendingOperation {
    pub operation_id: String,
    pub kind: OperationKind,
    pub phase: OperationPhase,
    pub intended_source: SourceAck,
    pub target_event_id: String,
    pub base_etag: Option<String>,
    pub desired_payload_hash: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Create,
    Update,
    Delete,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperationPhase {
    Prepared,
    Uncertain,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Tombstone {
    pub reason: String,
    pub source_commit: String,
    pub operation_id: Option<String>,
    pub remote_disposition: RemoteDisposition,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteDisposition {
    Deleted,
    Retained,
    Unknown,
}

const JSON_LIMIT: usize = 1024 * 1024;
const ENTRY_LIMIT: usize = 256;
const COMMIT_LIMIT: usize = 8;
const LINE_LIMIT: usize = 1_000_000;
const TEXT_LIMIT: usize = 64 * 1024;

/// Hash only the four managed provider fields, never a provider response or UI label.
pub fn payload_hash(event: &ProspectiveEvent) -> String {
    let mut hash = Sha256::new();
    hash.update(b"hive-calendar-managed-payload:v1\0");
    for value in [
        event.summary.as_str(),
        event.start.date.as_str(),
        event.end.date.as_str(),
        event.transparency,
    ] {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value.as_bytes());
    }
    format!("sha256:{:x}", hash.finalize())
}

fn hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn revision(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|v| hex(v, 64))
}

fn uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
        && value.bytes().any(|b| b != b'0' && b != b'-')
}

fn display_text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn event_id(value: &str) -> bool {
    (5..=1024).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'v').contains(&b))
}

/// This validates a claimed namespace, not authentication or account ownership.
pub(crate) fn validate_binding(binding: &Binding) -> Result<()> {
    ensure!(
        binding.provider == "google_calendar",
        "invalid ledger provider"
    );
    ensure!(
        binding
            .account_id
            .strip_prefix("google-sub-v1:")
            .is_some_and(|v| hex(v, 64)),
        "invalid ledger account namespace"
    );
    ensure!(
        display_text(&binding.calendar_id, 1024)
            && !binding.calendar_id.chars().any(char::is_whitespace)
            && !binding.calendar_id.eq_ignore_ascii_case("primary")
            && !binding.calendar_id.eq_ignore_ascii_case("default"),
        "invalid ledger calendar identity"
    );
    // An opaque portable identifier, not a filename, URL, or local repository path.
    ensure!(
        (1..=256).contains(&binding.source_repo_id.len())
            && binding
                .source_repo_id
                .bytes()
                .all(|b| { b.is_ascii_alphanumeric() || b"._:-".contains(&b) })
            && binding.source_repo_id != "."
            && binding.source_repo_id != "..",
        "invalid ledger repository identity"
    );
    Ok(())
}

// serde_json::Value normally overwrites duplicate keys. Build it ourselves at every
// depth, including unknown fields, before any typed deserialization can occur.
struct UniqueJson(serde_json::Value);

pub(crate) fn decode_unique_json<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    let value = serde_json::from_slice::<UniqueJson>(bytes)
        .map_err(|_| anyhow!("invalid JSON"))?
        .0;
    serde_json::from_value(value).map_err(|_| anyhow!("invalid JSON schema"))
}

impl<'de> Deserialize<'de> for UniqueJson {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct JsonVisitor;
        impl<'de> Visitor<'de> for JsonVisitor {
            type Value = UniqueJson;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("unique-key JSON")
            }

            fn visit_map<M: MapAccess<'de>>(
                self,
                mut map: M,
            ) -> std::result::Result<Self::Value, M::Error> {
                let mut values = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(de::Error::custom("duplicate JSON key"));
                    }
                    values.insert(key, map.next_value::<UniqueJson>()?.0);
                }
                Ok(UniqueJson(serde_json::Value::Object(values)))
            }

            fn visit_seq<S: SeqAccess<'de>>(
                self,
                mut seq: S,
            ) -> std::result::Result<Self::Value, S::Error> {
                let mut values = Vec::new();
                while let Some(value) = seq.next_element::<UniqueJson>()? {
                    values.push(value.0);
                }
                Ok(UniqueJson(serde_json::Value::Array(values)))
            }

            fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<Self::Value, E> {
                Ok(UniqueJson(v.into()))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Self::Value, E> {
                Ok(UniqueJson(v.into()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Self::Value, E> {
                Ok(UniqueJson(v.into()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<Self::Value, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| UniqueJson(n.into()))
                    .ok_or_else(|| de::Error::custom("invalid JSON number"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Self::Value, E> {
                Ok(UniqueJson(v.into()))
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(UniqueJson(serde_json::Value::Null))
            }
        }
        deserializer.deserialize_any(JsonVisitor)
    }
}

pub(crate) fn validate_source(source: &SourceAck, commit_len: usize) -> Result<()> {
    ensure!(
        hex(&source.commit, commit_len),
        "invalid ledger source commit"
    );
    ensure!(
        hex(&source.git_blob_id, commit_len),
        "invalid ledger source blob"
    );
    ensure!(
        hex(&source.parser_sha256, 64),
        "invalid ledger parser digest"
    );
    ensure!(
        revision(&source.source_revision),
        "invalid ledger source revision"
    );
    ensure!(
        display_text(&source.path, 4096)
            && source.path.to_ascii_lowercase().ends_with(".md")
            && !source.path.contains('\\')
            && source.path.split('/').count() <= 64
            && source.path.split('/').all(|part| {
                !part.is_empty()
                    && part != "."
                    && part != ".."
                    && !part.eq_ignore_ascii_case(".git")
            }),
        "invalid ledger source path"
    );
    ensure!(
        display_text(&source.source_id, 4096),
        "invalid ledger source identity"
    );
    ensure!(
        (1..=LINE_LIMIT).contains(&source.line),
        "invalid ledger source line"
    );
    let identity_ok = match &source.stable_id {
        Some(stable) => hex(stable, 32) && source.mw_id == format!("task:{stable}"),
        None => source
            .mw_id
            .strip_prefix(&format!("{}:{}:", source.source_id, source.line))
            .is_some_and(|order| {
                !order.is_empty()
                    && order.bytes().all(|b| b.is_ascii_digit())
                    && order
                        .parse::<usize>()
                        .is_ok_and(|n| n < LINE_LIMIT && order == n.to_string())
            }),
    };
    ensure!(identity_ok, "invalid ledger task identity");
    let task = &source.expected;
    ensure!(
        task.text.len() <= TEXT_LIMIT
            && task.literal_text.len() <= TEXT_LIMIT
            && task.raw.len() <= TEXT_LIMIT
            && task.due.len() <= 1024
            && task.start.len() <= 1024
            && task.estimate.len() <= 1024,
        "ledger expected task limit exceeded"
    );
    // Only a lexical checkbox check here. The source helper proves the exact
    // committed line and reparses metadata; this is not another Markdown parser.
    // CRLF source lines retain their terminal CR in the acknowledgement.
    let literal = task
        .literal_text
        .strip_suffix('\r')
        .unwrap_or(&task.literal_text);
    let checkbox = literal.trim_start_matches([' ', '\t']);
    let checkbox = checkbox
        .strip_prefix('-')
        .or_else(|| checkbox.strip_prefix('*'))
        .map(|v| v.trim_start_matches([' ', '\t']));
    ensure!(
        !task.text.trim().is_empty()
            && !literal.contains(['\n', '\r', '\0'])
            && checkbox.is_some_and(|v| {
                ["[ ]", "[x]", "[X]"].iter().any(|prefix| {
                    v.strip_prefix(prefix).is_some_and(|rest| {
                        rest.starts_with([' ', '\t']) && !rest.trim().is_empty()
                    })
                })
            }),
        "invalid ledger expected checkbox"
    );
    Ok(())
}

/// Decode untrusted evidence using an independently supplied binding. No intent
/// is cleared and no source object existence or provider ownership is inferred.
pub(crate) fn decode(bytes: &[u8], expected: &Binding) -> Result<Ledger> {
    validate_binding(expected)?;
    ensure!(bytes.len() <= JSON_LIMIT, "ledger JSON limit exceeded");
    let value = serde_json::from_slice::<UniqueJson>(bytes)
        .map_err(|_| anyhow!("invalid ledger JSON"))?
        .0;
    let ledger: Ledger =
        serde_json::from_value(value).map_err(|_| anyhow!("invalid ledger schema"))?;
    ensure!(
        ledger.schema_version == "1",
        "unsupported ledger schema version"
    );
    validate_binding(&ledger.binding)?;
    ensure!(&ledger.binding == expected, "ledger binding mismatch");
    let commit_len = ledger.source_watermark.len();
    ensure!(
        matches!(commit_len, 40 | 64) && hex(&ledger.source_watermark, commit_len),
        "invalid ledger source watermark"
    );
    ensure!(
        ledger.entries.len() <= ENTRY_LIMIT,
        "ledger entry limit exceeded"
    );
    let mut logical_ids = BTreeSet::new();
    let mut event_ids = BTreeSet::new();
    let mut operation_ids = BTreeSet::new();
    let mut observations = BTreeSet::new();
    let mut stable_ids = BTreeSet::new();
    let mut commits = BTreeSet::new();
    for entry in &ledger.entries {
        ensure!(uuid(&entry.logical_id), "invalid ledger logical identity");
        ensure!(
            logical_ids.insert(&entry.logical_id),
            "duplicate ledger logical identity"
        );
        ensure!(
            match entry.lifecycle {
                Lifecycle::Enrolled => entry.provider_ack.is_none() && entry.tombstone.is_none(),
                Lifecycle::Active => entry.provider_ack.is_some() && entry.tombstone.is_none(),
                Lifecycle::Retired =>
                    entry.provider_ack.is_some()
                        && entry.tombstone.is_some()
                        && entry.pending.is_none(),
            },
            "invalid ledger lifecycle"
        );
        let mut entry_events = BTreeSet::new();
        if let Some(ack) = &entry.provider_ack {
            ensure!(
                event_id(&ack.event_id),
                "invalid ledger provider event identity"
            );
            ensure!(
                display_text(&ack.etag, 1024),
                "invalid ledger provider etag"
            );
            ensure!(
                revision(&ack.last_payload_hash),
                "invalid ledger payload digest"
            );
            entry_events.insert(&ack.event_id);
        }
        if let Some(pending) = &entry.pending {
            ensure!(
                uuid(&pending.operation_id),
                "invalid ledger operation identity"
            );
            ensure!(
                operation_ids.insert(&pending.operation_id),
                "duplicate ledger operation identity"
            );
            ensure!(
                event_id(&pending.target_event_id),
                "invalid ledger pending event identity"
            );
            if let Some(hash) = &pending.desired_payload_hash {
                ensure!(revision(hash), "invalid ledger pending payload digest");
            }
            let valid = match pending.kind {
                OperationKind::Create => {
                    entry.lifecycle == Lifecycle::Enrolled
                        && pending.base_etag.is_none()
                        && pending.desired_payload_hash.is_some()
                }
                OperationKind::Update | OperationKind::Delete => {
                    entry.lifecycle == Lifecycle::Active
                        && entry.provider_ack.as_ref().is_some_and(|ack| {
                            pending.target_event_id == ack.event_id
                                && pending.base_etag.as_ref() == Some(&ack.etag)
                        })
                        && pending.desired_payload_hash.is_some()
                            == (pending.kind == OperationKind::Update)
                }
            };
            ensure!(valid, "invalid ledger pending operation");
            entry_events.insert(&pending.target_event_id);
        }
        for id in entry_events {
            ensure!(
                event_ids.insert(id),
                "duplicate ledger provider event identity"
            );
        }
        if let Some(tombstone) = &entry.tombstone {
            // Reason is inert display text, never an instruction or credential.
            ensure!(
                display_text(&tombstone.reason, 256),
                "invalid ledger tombstone reason"
            );
            ensure!(
                hex(&tombstone.source_commit, commit_len),
                "invalid ledger retirement commit"
            );
            if let Some(id) = &tombstone.operation_id {
                ensure!(uuid(id), "invalid ledger retirement operation identity");
                ensure!(
                    operation_ids.insert(id),
                    "duplicate ledger operation identity"
                );
            }
            // A claim of successful remote deletion needs an operation identity.
            ensure!(
                tombstone.remote_disposition != RemoteDisposition::Deleted
                    || tombstone.operation_id.is_some(),
                "ledger deletion acknowledgement requires operation identity"
            );
        }
        let mut entry_observations = BTreeSet::new();
        let mut entry_stable = BTreeSet::new();
        for source in std::iter::once(&entry.source_ack).chain(
            entry
                .pending
                .as_ref()
                .map(|pending| &pending.intended_source),
        ) {
            validate_source(source, commit_len)?;
            commits.insert(&source.commit);
            ensure!(
                commits.len() <= COMMIT_LIMIT,
                "ledger source commit limit exceeded"
            );
            entry_observations.insert((
                &source.path,
                &source.git_blob_id,
                &source.source_revision,
                source.line,
                &source.mw_id,
            ));
            if let Some(stable) = &source.stable_id {
                entry_stable.insert(stable);
            }
        }
        for basis in entry_observations {
            ensure!(
                observations.insert(basis),
                "duplicate ledger source observation"
            );
        }
        for stable in entry_stable {
            ensure!(stable_ids.insert(stable), "duplicate ledger stable hint");
        }
    }
    Ok(ledger)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CalendarDate;
    use serde_json::{Value, json};

    fn binding() -> Binding {
        Binding {
            provider: "google_calendar".into(),
            account_id: format!("google-sub-v1:{}", "a".repeat(64)),
            calendar_id: "calendar@example.test".into(),
            source_repo_id: "repo:portable-v1".into(),
        }
    }

    fn source() -> SourceAck {
        SourceAck {
            commit: "a".repeat(40),
            parser_sha256: "b".repeat(64),
            path: "notes/tasks.md".into(),
            source_id: "notes:tasks".into(),
            line: 1,
            mw_id: "notes:tasks:1:0".into(),
            git_blob_id: "c".repeat(40),
            source_revision: format!("sha256:{}", "d".repeat(64)),
            stable_id: None,
            expected: ExpectedTask {
                text: "Ship".into(),
                literal_text: "- [ ] Ship due:2026-11-01".into(),
                done: false,
                due: "2026-11-01".into(),
                raw: "due:2026-11-01".into(),
                start: String::new(),
                estimate: String::new(),
            },
        }
    }

    fn entry() -> LedgerEntry {
        LedgerEntry {
            logical_id: "00000000-0000-0000-0000-000000000001".into(),
            lifecycle: Lifecycle::Enrolled,
            source_ack: source(),
            provider_ack: None,
            pending: None,
            tombstone: None,
        }
    }

    fn ledger() -> Ledger {
        Ledger {
            schema_version: "1".into(),
            binding: binding(),
            source_watermark: "a".repeat(40),
            entries: vec![entry()],
        }
    }

    fn decoded(ledger: &Ledger) -> Result<Ledger> {
        decode(&serde_json::to_vec(ledger).unwrap(), &binding())
    }

    fn check_value(value: &Value) -> Result<Ledger> {
        decode(&serde_json::to_vec(value).unwrap(), &binding())
    }

    fn active(entry: &mut LedgerEntry) {
        entry.lifecycle = Lifecycle::Active;
        entry.provider_ack = Some(ProviderAck {
            event_id: "abcde01234".into(),
            etag: "\"version-1\"".into(),
            last_payload_hash: format!("sha256:{}", "e".repeat(64)),
        });
    }

    fn pending(entry: &mut LedgerEntry, kind: OperationKind) {
        entry.pending = Some(PendingOperation {
            operation_id: "00000000-0000-0000-0000-000000000002".into(),
            kind,
            phase: OperationPhase::Prepared,
            intended_source: entry.source_ack.clone(),
            target_event_id: "abcde01234".into(),
            base_etag: entry.provider_ack.as_ref().map(|ack| ack.etag.clone()),
            desired_payload_hash: (kind != OperationKind::Delete)
                .then(|| format!("sha256:{}", "f".repeat(64))),
        });
    }

    #[test]
    fn strict_schema_and_required_expected_fields() {
        let base = serde_json::to_value(ledger()).unwrap();
        assert!(check_value(&base).is_ok());
        for version in [json!(1), json!(null), json!("2"), json!("01")] {
            let mut value = base.clone();
            value["schemaVersion"] = version;
            assert!(check_value(&value).is_err());
        }
        for pointer in [
            "",
            "/binding",
            "/entries/0",
            "/entries/0/sourceAck",
            "/entries/0/sourceAck/expected",
        ] {
            let mut value = base.clone();
            value
                .pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("private-unknown-key".into(), json!("private-value"));
            let error = format!("{:#}", check_value(&value).unwrap_err());
            assert!(!error.contains("private"));
        }
        for field in [
            "text",
            "literalText",
            "done",
            "due",
            "raw",
            "start",
            "estimate",
        ] {
            let mut value = base.clone();
            value["entries"][0]["sourceAck"]["expected"]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(check_value(&value).is_err(), "{field}");
        }
        let mut full = ledger();
        active(&mut full.entries[0]);
        pending(&mut full.entries[0], OperationKind::Update);
        let value = serde_json::to_value(&full).unwrap();
        for pointer in [
            "/entries/0/providerAck",
            "/entries/0/pending",
            "/entries/0/pending/intendedSource",
            "/entries/0/pending/intendedSource/expected",
        ] {
            let mut bad = value.clone();
            bad.pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("extra".into(), json!(true));
            assert!(check_value(&bad).is_err());
        }
    }

    #[test]
    fn duplicate_keys_at_every_depth_and_hidden_values_are_rejected() {
        let value = serde_json::to_value(ledger()).unwrap();
        let bytes = serde_json::to_string(&value).unwrap();
        for key in ["schemaVersion", "provider", "logicalId", "commit", "text"] {
            let needle = format!("\"{key}\":");
            let duplicate =
                bytes.replacen(&needle, &format!("\"{key}\":\"private-value\",{needle}"), 1);
            let error = format!(
                "{:#}",
                decode(duplicate.as_bytes(), &binding()).unwrap_err()
            );
            assert_eq!(error, "invalid ledger JSON");
        }
        for bytes in [
            br#"{"hidden":[{"secret":1,"secret":2}]}"#.as_slice(),
            br#"{"a":1,"\u0061":2}"#.as_slice(),
            br#"{"private-key":"private-value""#.as_slice(),
            b"{} {}".as_slice(),
        ] {
            assert_eq!(
                decode(bytes, &binding()).unwrap_err().to_string(),
                "invalid ledger JSON"
            );
        }
    }

    #[test]
    fn bindings_are_explicit_and_independent() {
        for (field, bad) in [
            ("provider", "google"),
            ("accountId", "google-sub-v1:ABC"),
            ("calendarId", "PRIMARY"),
            ("calendarId", "default"),
            ("calendarId", "a b"),
            ("calendarId", "a\rb"),
            ("sourceRepoId", "../repo"),
            ("sourceRepoId", "/tmp/repo"),
            ("sourceRepoId", "https://repo"),
            ("sourceRepoId", ""),
        ] {
            let mut value = serde_json::to_value(ledger()).unwrap();
            value["binding"][field] = json!(bad);
            assert!(check_value(&value).is_err());
        }
        let mut expected = binding();
        expected.calendar_id = "other@example.test".into();
        assert_eq!(
            decode(&serde_json::to_vec(&ledger()).unwrap(), &expected)
                .unwrap_err()
                .to_string(),
            "ledger binding mismatch"
        );
        expected.provider = "invalid".into();
        assert_eq!(
            decode(b"not JSON", &expected).unwrap_err().to_string(),
            "invalid ledger provider"
        );
        let mut valid = binding();
        valid.calendar_id = "c".repeat(1024);
        valid.source_repo_id = "r".repeat(256);
        assert!(validate_binding(&valid).is_ok());
        valid.calendar_id.push('c');
        assert!(validate_binding(&valid).is_err());
        valid.calendar_id = "calendar".into();
        valid.source_repo_id.push('r');
        assert!(validate_binding(&valid).is_err());
    }

    #[test]
    fn source_shapes_and_bounds() {
        let base = serde_json::to_value(ledger()).unwrap();
        for (field, bad) in [
            ("commit", json!("a".repeat(39))),
            ("commit", json!("A".repeat(40))),
            ("gitBlobID", json!("c".repeat(64))),
            ("parserSha256", json!("sha256:bad")),
            ("sourceRevision", json!("d".repeat(64))),
            ("line", json!(0)),
            ("line", json!(LINE_LIMIT + 1)),
            ("sourceId", json!("\nsecret")),
            ("sourceId", json!("s".repeat(4097))),
            ("mwId", json!("notes:tasks:1:00")),
            ("stableId", json!("a".repeat(32))),
        ] {
            let mut value = base.clone();
            value["entries"][0]["sourceAck"][field] = bad;
            assert!(check_value(&value).is_err(), "{field}");
        }
        for path in [
            "/a.md",
            "../a.md",
            "a//b.md",
            "a/./b.md",
            "a/.git/b.md",
            "a\\b.md",
            "a.txt",
            "a\nb.md",
        ] {
            let mut value = base.clone();
            value["entries"][0]["sourceAck"]["path"] = json!(path);
            assert!(check_value(&value).is_err(), "{path}");
        }
        for literal in [
            "Ship [ ]",
            "- [ ]",
            "- [y] Ship",
            "- [ ]Ship",
            "- [ ] Ship\n- [ ] Other",
        ] {
            let mut value = base.clone();
            value["entries"][0]["sourceAck"]["expected"]["literalText"] = json!(literal);
            assert!(check_value(&value).is_err());
        }
        let mut full = ledger();
        full.source_watermark = "a".repeat(64);
        full.entries[0].source_ack.commit = "b".repeat(64);
        full.entries[0].source_ack.git_blob_id = "c".repeat(64);
        full.entries[0].source_ack.stable_id = Some("d".repeat(32));
        full.entries[0].source_ack.mw_id = format!("task:{}", "d".repeat(32));
        assert!(decoded(&full).is_ok());
        full.entries[0].source_ack.expected.text = "x".repeat(TEXT_LIMIT + 1);
        assert!(decoded(&full).is_err());
    }

    #[test]
    fn crlf_literal_round_trips_unchanged_and_embedded_controls_are_rejected() {
        let mut full = ledger();
        full.entries[0].source_ack.expected.literal_text.push('\r');
        assert!(validate_source(&full.entries[0].source_ack, 40).is_ok());
        let round_trip = decoded(&full).unwrap();
        assert_eq!(round_trip, full);
        assert_eq!(
            round_trip.entries[0]
                .source_ack
                .expected
                .literal_text
                .as_bytes(),
            b"- [ ] Ship due:2026-11-01\r"
        );
        for literal in [
            "- [ ] private\rvalue",
            "- [ ] private\rvalue\r",
            "- [ ] private\nvalue",
            "- [ ] private\r\n",
            "- [ ] private\0value",
            "- [ ] private\0value\r",
            "- [ ] private-value\r\r",
        ] {
            full.entries[0].source_ack.expected.literal_text = literal.into();
            assert_eq!(
                validate_source(&full.entries[0].source_ack, 40)
                    .unwrap_err()
                    .to_string(),
                "invalid ledger expected checkbox"
            );
            assert_eq!(
                format!("{:#}", decoded(&full).unwrap_err()),
                "invalid ledger expected checkbox"
            );
        }
    }

    #[test]
    fn bootstrap_compatible_paths_round_trip_without_allowing_escapes() {
        for path in [
            "a.MD".to_owned(),
            "notes:work/tasks.Md".to_owned(),
            format!("{}tasks.MD", "notes/".repeat(63)),
            format!("{}.md", "a".repeat(4093)),
        ] {
            let mut full = ledger();
            full.entries[0].source_ack.path = path;
            assert_eq!(decoded(&full).unwrap(), full);
        }
        for path in [
            "/a.MD".to_owned(),
            "notes:work/../tasks.Md".to_owned(),
            "notes:work//tasks.Md".to_owned(),
            "notes:work/./tasks.Md".to_owned(),
            "notes:work/.GiT/tasks.Md".to_owned(),
            "notes:work\\tasks.Md".to_owned(),
            "notes:work/\0tasks.Md".to_owned(),
            "notes:work/\ntasks.Md".to_owned(),
            "notes:work/tasks.Md/".to_owned(),
            format!("{}tasks.MD", "notes/".repeat(64)),
            format!("{}.md", "a".repeat(4094)),
        ] {
            let mut full = ledger();
            full.entries[0].source_ack.path = path;
            assert!(decoded(&full).is_err());
        }
    }

    #[test]
    fn lifecycle_pending_and_retirement_preserve_evidence() {
        let mut full = ledger();
        assert_eq!(decoded(&full).unwrap(), full);
        pending(&mut full.entries[0], OperationKind::Create);
        for phase in [OperationPhase::Prepared, OperationPhase::Uncertain] {
            full.entries[0].pending.as_mut().unwrap().phase = phase;
            assert_eq!(decoded(&full).unwrap(), full);
        }
        full.entries[0].pending.as_mut().unwrap().base_etag = Some("bad".into());
        assert!(decoded(&full).is_err());
        active(&mut full.entries[0]);
        for kind in [OperationKind::Update, OperationKind::Delete] {
            pending(&mut full.entries[0], kind);
            assert!(decoded(&full).is_ok());
            let mut bad = full.clone();
            bad.entries[0].pending.as_mut().unwrap().target_event_id = "other0".into();
            assert!(decoded(&bad).is_err());
            bad = full.clone();
            bad.entries[0].pending.as_mut().unwrap().base_etag = None;
            assert!(decoded(&bad).is_err());
            bad = full.clone();
            bad.entries[0]
                .pending
                .as_mut()
                .unwrap()
                .desired_payload_hash =
                (kind == OperationKind::Delete).then(|| format!("sha256:{}", "f".repeat(64)));
            assert!(decoded(&bad).is_err());
        }
        pending(&mut full.entries[0], OperationKind::Create);
        assert!(decoded(&full).is_err());
        full.entries[0].pending = None;
        full.entries[0].lifecycle = Lifecycle::Retired;
        assert!(decoded(&full).is_err());
        full.entries[0].tombstone = Some(Tombstone {
            reason: "source removed".into(),
            source_commit: "b".repeat(40),
            operation_id: None,
            remote_disposition: RemoteDisposition::Retained,
        });
        assert_eq!(decoded(&full).unwrap(), full);
        full.entries[0]
            .tombstone
            .as_mut()
            .unwrap()
            .remote_disposition = RemoteDisposition::Unknown;
        assert!(decoded(&full).is_ok());
        full.entries[0]
            .tombstone
            .as_mut()
            .unwrap()
            .remote_disposition = RemoteDisposition::Deleted;
        assert!(decoded(&full).is_err());
        full.entries[0].tombstone.as_mut().unwrap().operation_id =
            Some("00000000-0000-0000-0000-000000000003".into());
        assert_eq!(decoded(&full).unwrap(), full);
        pending(&mut full.entries[0], OperationKind::Delete);
        assert!(decoded(&full).is_err());
        full.entries[0].pending = None;
        full.entries[0].provider_ack = None;
        assert!(decoded(&full).is_err());
    }

    #[test]
    fn duplicate_identities_include_retired_and_intended_sources() {
        let mut full = ledger();
        let mut other = entry();
        other.logical_id = "00000000-0000-0000-0000-000000000003".into();
        other.source_ack.path = "other.md".into();
        full.entries.push(other);
        assert!(decoded(&full).is_ok());
        let mut bad = full.clone();
        bad.entries[1].logical_id = bad.entries[0].logical_id.clone();
        assert!(decoded(&bad).is_err());
        bad = full.clone();
        bad.entries[1].source_ack = bad.entries[0].source_ack.clone();
        bad.entries[1].source_ack.commit = "b".repeat(40);
        assert!(decoded(&bad).is_err());
        for entry in &mut full.entries {
            active(entry);
        }
        assert!(decoded(&full).is_err());
        full.entries[1].provider_ack.as_mut().unwrap().event_id = "other0".into();
        assert!(decoded(&full).is_ok());
        for entry in &mut full.entries {
            pending(entry, OperationKind::Update);
            let ack = entry.provider_ack.as_ref().unwrap();
            entry.pending.as_mut().unwrap().target_event_id = ack.event_id.clone();
        }
        assert!(decoded(&full).is_err());
        full.entries[1].pending.as_mut().unwrap().operation_id =
            "00000000-0000-0000-0000-000000000004".into();
        full.entries[1].pending.as_mut().unwrap().intended_source =
            full.entries[0].source_ack.clone();
        assert!(decoded(&full).is_err());
        full.entries[1].pending = None;
        full.entries[0].pending = None;
        for (index, entry) in full.entries.iter_mut().enumerate() {
            entry.source_ack.stable_id = Some("f".repeat(32));
            entry.source_ack.mw_id = format!("task:{}", "f".repeat(32));
            if index == 0 {
                entry.lifecycle = Lifecycle::Retired;
                entry.tombstone = Some(Tombstone {
                    reason: "retired".into(),
                    source_commit: "a".repeat(40),
                    operation_id: None,
                    remote_disposition: RemoteDisposition::Retained,
                });
            }
        }
        assert!(decoded(&full).is_err());
    }

    #[test]
    fn limits_and_identifier_shapes() {
        let mut full = ledger();
        full.entries.clear();
        for index in 0..ENTRY_LIMIT {
            let mut item = entry();
            item.logical_id = format!("00000000-0000-0000-0000-{:012x}", index + 1);
            item.source_ack.path = format!("{index}.md");
            item.source_ack.commit = format!("{:040x}", index % COMMIT_LIMIT);
            full.entries.push(item);
        }
        assert!(decoded(&full).is_ok());
        full.entries[0].source_ack.commit = "f".repeat(40);
        assert!(decoded(&full).is_err());
        full.entries[0].source_ack.commit = "0".repeat(40);
        full.entries.push(entry());
        assert!(decoded(&full).is_err());
        assert_eq!(
            decode(&vec![b' '; JSON_LIMIT + 1], &binding())
                .unwrap_err()
                .to_string(),
            "ledger JSON limit exceeded"
        );
        let mut bytes = serde_json::to_vec(&ledger()).unwrap();
        bytes.resize(JSON_LIMIT, b' ');
        assert!(decode(&bytes, &binding()).is_ok());
        for id in [
            "00000000-0000-0000-0000-000000000000",
            "ABCDEF00-0000-0000-0000-000000000001",
            "task:0123456789abcdef0123456789abcdef",
        ] {
            let mut full = ledger();
            full.entries[0].logical_id = id.into();
            assert!(decoded(&full).is_err());
        }
        for id in ["abcd", "abcdew", "ABCDEF", "a/bcd"] {
            let mut full = ledger();
            active(&mut full.entries[0]);
            full.entries[0].provider_ack.as_mut().unwrap().event_id = id.into();
            assert!(decoded(&full).is_err());
        }
    }

    #[test]
    fn payload_hash_frozen_golden_and_field_changes() {
        let mut event = ProspectiveEvent {
            summary: "Ship".into(),
            start: CalendarDate {
                date: "2026-11-01".into(),
            },
            end: CalendarDate {
                date: "2026-11-02".into(),
            },
            availability: "free",
            transparency: "transparent",
        };
        // Independently computed with Python hashlib and struct.pack(">Q", len).
        let golden = "sha256:db3019a39ee70f4d9c986a5f11bd9c3564260a8f74ea5c9c8c8b2128c0ee7db2";
        assert_eq!(payload_hash(&event), golden);
        event.availability = "UI-only label";
        assert_eq!(payload_hash(&event), golden);
        event.summary.push('!');
        assert_ne!(payload_hash(&event), golden);
        event.summary.pop();
        event.start.date = "2026-11-02".into();
        assert_ne!(payload_hash(&event), golden);
        event.start.date = "2026-11-01".into();
        event.end.date = "2026-11-03".into();
        assert_ne!(payload_hash(&event), golden);
        event.end.date = "2026-11-02".into();
        event.transparency = "opaque";
        assert_ne!(payload_hash(&event), golden);
    }
}

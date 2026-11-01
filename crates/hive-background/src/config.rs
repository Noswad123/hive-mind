//! Configuration contains public routing information only, never credentials.
use crate::{
    WorkerConfig,
    state::{self, Paths},
};
use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap, ffi::OsString, os::unix::fs::MetadataExt, path::Path, time::Duration,
};

fn path(value: &Path) -> Result<()> {
    let text = value
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("path must be UTF-8"))?;
    ensure!(
        value.is_absolute() && text.len() <= 4096 && !text.chars().any(char::is_control),
        "invalid configuration path"
    );
    Ok(())
}

enum PathRole {
    WorkingDirectory,
    Executable,
    EditorIdentity,
}

fn trusted(value: &Path, role: PathRole) -> Result<()> {
    path(value)?;
    let meta = std::fs::symlink_metadata(value)?;
    ensure!(
        !meta.file_type().is_symlink(),
        "store canonical executable paths"
    );
    ensure!(
        meta.uid() == 0 || meta.uid() == unsafe { libc::geteuid() },
        "untrusted path owner"
    );
    ensure!(meta.mode() & 0o022 == 0, "path is writable by other users");
    let canonical = std::fs::canonicalize(value)?;
    // Neovim is only an OS-process identity locator, not code execution
    // authority. Its canonical ancestors may be group writable, never world writable.
    let ancestor_write_mask = if matches!(role, PathRole::EditorIdentity) {
        ensure!(value == canonical, "store canonical editor identity paths");
        0o002
    } else {
        0o022
    };
    let resolved = std::fs::metadata(&canonical)?;
    ensure!(
        meta.dev() == resolved.dev() && meta.ino() == resolved.ino(),
        "configured path changed identity"
    );
    for ancestor in canonical.ancestors().skip(1) {
        let ancestor = std::fs::symlink_metadata(ancestor)?;
        ensure!(
            ancestor.is_dir()
                && (ancestor.uid() == 0 || ancestor.uid() == unsafe { libc::geteuid() })
                && ancestor.mode() & ancestor_write_mask == 0,
            "untrusted configured path ancestor"
        );
    }
    if !matches!(role, PathRole::WorkingDirectory) {
        ensure!(
            meta.is_file() && meta.nlink() == 1 && meta.mode() & 0o111 != 0,
            "binary must be a regular executable"
        );
    } else {
        ensure!(meta.is_dir(), "working directory must exist");
    }
    Ok(())
}

/// Validate paths before source discovery or authentication. This performs only
/// filesystem checks; Neovim remains an identity locator, not execution authority.
pub fn validate_programs(mw: &Path, hive: &Path, nvim: &Path, cwd: &Path) -> Result<()> {
    trusted(cwd, PathRole::WorkingDirectory)?;
    for binary in [mw, hive] {
        trusted(binary, PathRole::Executable)?;
    }
    trusted(nvim, PathRole::EditorIdentity)
}

pub fn validate(config: &WorkerConfig) -> Result<()> {
    ensure!(
        config.schema_version == "1",
        "unsupported background config schema"
    );
    hive_google_auth::validate_connection(&config.connection)?;
    let hash = config
        .account_namespace
        .strip_prefix("google-sub-v1:")
        .ok_or_else(|| anyhow::anyhow!("verified account namespace required"))?;
    ensure!(
        hash.len() == 64
            && hash
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
        "invalid verified account namespace"
    );
    ensure!(
        !config.list_id.is_empty()
            && config.list_id.len() <= 4096
            && !config.list_id.chars().any(char::is_control),
        "invalid list ID"
    );
    path(&config.queue_path)?;
    validate_programs(
        &config.mw_binary,
        &config.hive_binary,
        &config.nvim_binary,
        &config.working_directory,
    )?;
    config.destination.validate()?;
    for value in [
        &config.destination.notes_dir,
        &config.destination.inbox_path,
        &config.destination.dashboard_path,
    ] {
        path(Path::new(value))?;
    }
    // Resolve the actual filesystem, rather than lexically erasing link/.. pairs.
    let root = std::fs::canonicalize(&config.destination.notes_dir)?;
    ensure!(root.is_dir(), "notes root must exist");
    for value in [
        &config.destination.inbox_path,
        &config.destination.dashboard_path,
    ] {
        let value = Path::new(value);
        let resolved = match std::fs::canonicalize(value) {
            Ok(resolved) => resolved,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                ensure!(
                    std::fs::symlink_metadata(value).is_err(),
                    "dangling destination link"
                );
                std::fs::canonicalize(
                    value
                        .parent()
                        .ok_or_else(|| anyhow::anyhow!("destination has no parent"))?,
                )?
                .join(
                    value
                        .file_name()
                        .ok_or_else(|| anyhow::anyhow!("destination has no filename"))?,
                )
            }
            Err(e) => return Err(e.into()),
        };
        ensure!(
            resolved != root && resolved.starts_with(&root),
            "destination escapes notes root"
        );
        if resolved.exists() {
            ensure!(resolved.is_file(), "destination must be a file");
        }
    }
    for (key, value) in &config.mw_environment {
        ensure!(
            matches!(
                key.as_str(),
                "MW_CONFIG"
                    | "NOTES_DIR"
                    | "INBOX_PATH"
                    | "MW_INBOX_PATH"
                    | "DASHBOARD_PATH"
                    | "XDG_CONFIG_HOME"
                    | "XDG_DATA_HOME"
            ),
            "unsupported source environment variable"
        );
        path(Path::new(value))?;
    }
    ensure!(
        (30..=3600).contains(&config.interval_seconds),
        "interval must be 30..3600 seconds"
    );
    ensure!(
        (10..=120).contains(&config.cycle_timeout_seconds)
            && config.cycle_timeout_seconds < config.interval_seconds,
        "invalid cycle timeout"
    );
    ensure!(
        config.delivery_limit == 1,
        "initial background delivery limit must be one"
    );
    Ok(())
}

pub fn load(paths: &Paths) -> Result<WorkerConfig> {
    let config = state::read_json(&paths.config)?;
    validate(&config)?;
    Ok(config)
}

pub fn save(paths: &Paths, config: &WorkerConfig, replace: bool) -> Result<String> {
    validate(config)?;
    hive_capture::store::CaptureStore::inspect(&config.queue_path)?;
    paths.ensure()?;
    let _lock = state::FileLock::acquire(&paths.admission_lock, false, Duration::from_secs(5))?;
    let old = state::existing_bytes(&paths.config)?;
    if let Some(bytes) = &old {
        let previous: WorkerConfig = serde_json::from_slice(bytes)?;
        ensure!(
            previous.schema_version == "1",
            "refusing unknown existing config schema"
        );
        ensure!(
            replace,
            "background config already exists; replacement must be explicit"
        );
    }
    state::write_bytes(&paths.config, &serde_json::to_vec(config)?, old.as_deref())?;
    digest(config)
}

/// SHA-256 of compact serde_json serialization of WorkerConfig (field order is
/// the struct declaration order; environment keys are sorted by BTreeMap).
/// Pretty file whitespace is deliberately not part of the child RPC identity.
pub fn digest(config: &WorkerConfig) -> Result<String> {
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(config)?)
    ))
}

pub fn source_environment(config: &WorkerConfig) -> Result<BTreeMap<OsString, OsString>> {
    validate(config)?;
    let mut environment = BTreeMap::from([
        (
            OsString::from("HOME"),
            hive_google_auth::storage::system_user_home()?.into_os_string(),
        ),
        (
            OsString::from("PATH"),
            OsString::from("/usr/bin:/bin:/opt/homebrew/bin"),
        ),
    ]);
    environment.extend(
        config
            .mw_environment
            .iter()
            .map(|(k, v)| (k.into(), v.into())),
    );
    Ok(environment)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    pub(crate) fn fixture() -> (tempfile::TempDir, WorkerConfig) {
        let temp = super::super::state::tests::fixture();
        let root = temp.path();
        let binary = root.join("hive<&\"'");
        std::fs::write(&binary, b"fixture, never executed").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = WorkerConfig {
            schema_version: "1".into(),
            connection: "personal".into(),
            account_namespace: format!("google-sub-v1:{}", "a".repeat(64)),
            queue_path: root.join("queue.sqlite"),
            list_id: "inbox".into(),
            mw_binary: binary.clone(),
            hive_binary: binary.clone(),
            nvim_binary: binary,
            working_directory: root.into(),
            destination: hive_capture::DestinationBinding {
                notes_dir: root.to_str().unwrap().into(),
                inbox_path: root.join("inbox.md").to_str().unwrap().into(),
                dashboard_path: root.join("dashboard.md").to_str().unwrap().into(),
            },
            mw_environment: BTreeMap::new(),
            interval_seconds: 60,
            cycle_timeout_seconds: 45,
            delivery_limit: 1,
        };
        (temp, config)
    }

    #[test]
    fn strict_config_and_digest() {
        let (_temp, mut cfg) = fixture();
        validate(&cfg).unwrap();
        let original = digest(&cfg).unwrap();
        assert_eq!(
            original,
            digest(&serde_json::from_str(&serde_json::to_string_pretty(&cfg).unwrap()).unwrap())
                .unwrap()
        );
        assert!(original.starts_with("sha256:"));
        cfg.mw_environment.insert("HOME".into(), "/tmp".into());
        assert!(validate(&cfg).is_err());
        cfg.mw_environment.clear();
        cfg.delivery_limit = 2;
        assert!(validate(&cfg).is_err());
        cfg.delivery_limit = 1;
        cfg.cycle_timeout_seconds = 60;
        assert!(validate(&cfg).is_err());
    }

    #[test]
    fn save_requires_identified_queue_and_explicit_known_replacement() {
        let (temp, mut cfg) = fixture();
        let paths = Paths::in_root(temp.path().join("state"));
        assert!(save(&paths, &cfg, false).is_err());
        assert!(!paths.root.exists());
        drop(hive_capture::store::CaptureStore::open(&cfg.queue_path).unwrap());
        let before = std::fs::read(&cfg.queue_path).unwrap();
        assert_eq!(save(&paths, &cfg, false).unwrap(), digest(&cfg).unwrap());
        assert_eq!(load(&paths).unwrap(), cfg);
        assert_eq!(std::fs::read(&cfg.queue_path).unwrap(), before);
        assert!(save(&paths, &cfg, false).is_err());
        cfg.interval_seconds = 90;
        save(&paths, &cfg, true).unwrap();
        let mut unknown = serde_json::to_value(&cfg).unwrap();
        unknown["schemaVersion"] = "2".into();
        state::write_json(&paths.config, &unknown).unwrap();
        assert!(save(&paths, &cfg, true).is_err());
        assert_eq!(
            state::read_json::<serde_json::Value>(&paths.config).unwrap(),
            unknown
        );
    }

    #[test]
    fn programs_preflight_rejects_unsafe_executed_sources() {
        let (temp, cfg) = fixture();
        let source = temp.path().join("source");
        std::fs::write(&source, b"source fixture, never executed").unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o755)).unwrap();
        let check = |mw: &Path, hive: &Path| {
            validate_programs(mw, hive, &cfg.nvim_binary, &cfg.working_directory)
        };
        check(&source, &cfg.hive_binary).unwrap();
        check(&cfg.mw_binary, &source).unwrap();
        let rejected = |candidate: &Path| {
            assert!(check(candidate, &cfg.hive_binary).is_err());
            assert!(check(&cfg.mw_binary, candidate).is_err());
        };
        for mode in [0o644, 0o775, 0o777] {
            std::fs::set_permissions(&source, std::fs::Permissions::from_mode(mode)).unwrap();
            rejected(&source);
        }
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o755)).unwrap();
        let hardlink = temp.path().join("source-hardlink");
        std::fs::hard_link(&source, &hardlink).unwrap();
        rejected(&source);
        std::fs::remove_file(hardlink).unwrap();
        let alias = temp.path().join("source-alias");
        symlink(&source, &alias).unwrap();
        rejected(&alias);
        let unsafe_parent = temp.path().join("unsafe");
        std::fs::create_dir(&unsafe_parent).unwrap();
        let unsafe_source = unsafe_parent.join("source");
        std::fs::rename(&source, &unsafe_source).unwrap();
        std::fs::set_permissions(&unsafe_parent, std::fs::Permissions::from_mode(0o775)).unwrap();
        rejected(&unsafe_source);
    }

    #[test]
    fn editor_identity_allows_owned_group_writable_ancestors_only() {
        let (temp, mut cfg) = fixture();
        let cellar = temp.path().join("Cellar");
        let bin = cellar.join("neovim/0.12.5_1/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::set_permissions(&cellar, std::fs::Permissions::from_mode(0o775)).unwrap();
        let nvim = bin.join("nvim");
        std::fs::write(&nvim, b"identity fixture, never executed").unwrap();
        std::fs::set_permissions(&nvim, std::fs::Permissions::from_mode(0o755)).unwrap();
        cfg.nvim_binary = std::fs::canonicalize(&nvim).unwrap();
        validate_programs(
            &cfg.mw_binary,
            &cfg.hive_binary,
            &cfg.nvim_binary,
            &cfg.working_directory,
        )
        .unwrap();
        validate(&cfg).unwrap();

        let mut executed = cfg.clone();
        executed.hive_binary = cfg.nvim_binary.clone();
        assert!(validate(&executed).is_err());
        executed = cfg.clone();
        executed.mw_binary = cfg.nvim_binary.clone();
        assert!(validate(&executed).is_err());
        executed = cfg.clone();
        executed.working_directory = bin;
        assert!(validate(&executed).is_err());

        std::fs::set_permissions(&cellar, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            validate_programs(
                &cfg.mw_binary,
                &cfg.hive_binary,
                &cfg.nvim_binary,
                &cfg.working_directory
            )
            .is_err()
        );
        assert!(validate(&cfg).is_err());
        std::fs::set_permissions(&cellar, std::fs::Permissions::from_mode(0o775)).unwrap();
        validate(&cfg).unwrap();
    }

    #[test]
    fn editor_identity_retains_file_and_canonical_path_checks() {
        let (temp, mut cfg) = fixture();
        let bin = temp.path().join("Cellar/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let nvim = bin.join("nvim");
        std::fs::write(&nvim, b"identity fixture, never executed").unwrap();
        cfg.nvim_binary = nvim.clone();
        for mode in [0o775, 0o777, 0o644] {
            std::fs::set_permissions(&nvim, std::fs::Permissions::from_mode(mode)).unwrap();
            assert!(validate(&cfg).is_err());
        }
        std::fs::set_permissions(&nvim, std::fs::Permissions::from_mode(0o755)).unwrap();
        validate(&cfg).unwrap();
        let link = bin.join("nvim-hardlink");
        std::fs::hard_link(&nvim, &link).unwrap();
        assert!(validate(&cfg).is_err());
        std::fs::remove_file(&link).unwrap();
        let alias = temp.path().join("bin-alias");
        symlink(&bin, &alias).unwrap();
        cfg.nvim_binary = alias.join("nvim");
        assert!(validate(&cfg).is_err());
        cfg.nvim_binary = std::fs::canonicalize(&cfg.nvim_binary).unwrap();
        validate(&cfg).unwrap();
    }

    #[test]
    fn filesystem_escape_and_read_only_load() {
        let (temp, mut cfg) = fixture();
        let outside = super::super::state::tests::fixture();
        symlink(outside.path(), temp.path().join("escape")).unwrap();
        cfg.destination.inbox_path = temp.path().join("escape/out.md").to_str().unwrap().into();
        assert!(validate(&cfg).is_err());
        let paths = Paths::in_root(temp.path().join("absent"));
        assert!(load(&paths).is_err());
        assert!(!paths.root.exists());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn environment_is_public_and_executable_aliases_must_be_canonical() {
        let (temp, mut cfg) = fixture();
        cfg.mw_environment
            .insert("NOTES_DIR".into(), cfg.destination.notes_dir.clone());
        let environment = source_environment(&cfg).unwrap();
        assert_eq!(environment.len(), 3);
        assert_eq!(
            environment.get(&OsString::from("PATH")).unwrap(),
            "/usr/bin:/bin:/opt/homebrew/bin"
        );
        assert_eq!(
            environment.get(&OsString::from("HOME")).unwrap(),
            hive_google_auth::storage::system_user_home()
                .unwrap()
                .as_os_str()
        );
        for key in [
            "PATH",
            "HOME",
            "NVIM",
            "GOOGLE_TOKEN",
            "MW_PROVIDER",
            "BASH_ENV",
        ] {
            cfg.mw_environment.insert(key.into(), "/public/path".into());
            assert!(validate(&cfg).is_err());
            cfg.mw_environment.remove(key);
        }
        let alias = temp.path().join("binary-alias");
        symlink(&cfg.nvim_binary, &alias).unwrap();
        cfg.nvim_binary = alias;
        assert!(validate(&cfg).is_err());
        cfg.nvim_binary = std::fs::canonicalize(&cfg.nvim_binary).unwrap();
        validate(&cfg).unwrap();
    }
}

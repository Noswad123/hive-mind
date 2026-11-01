//! Owned plist management only. Service bootstrap/bootout belongs to the CLI.
use crate::{
    SERVICE_LABEL, WorkerConfig, config,
    state::{self, Paths},
};
use anyhow::{Result, ensure};
use std::{
    ffi::CString,
    path::{Path, PathBuf},
};

const MARKER: &str = "<!-- hive-captures-background:v1 -->";

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub fn render(config: &WorkerConfig) -> Result<String> {
    config::validate(config)?;
    let environment = config::source_environment(config)?;
    let mut xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n{MARKER}\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>{SERVICE_LABEL}</string>\n<key>ProgramArguments</key><array>\n"
    );
    for argument in [
        config
            .hive_binary
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("invalid binary path"))?,
        "captures",
        "background",
        "run-once",
    ] {
        xml.push_str(&format!("<string>{}</string>\n", escape(argument)));
    }
    xml.push_str(&format!("</array>\n<key>StartInterval</key><integer>{}</integer>\n<key>RunAtLoad</key><true/>\n<key>WorkingDirectory</key><string>{}</string>\n<key>EnvironmentVariables</key><dict>\n", config.interval_seconds, escape(config.working_directory.to_str().unwrap())));
    for (key, value) in environment {
        xml.push_str(&format!(
            "<key>{}</key><string>{}</string>\n",
            escape(
                key.to_str()
                    .ok_or_else(|| anyhow::anyhow!("invalid environment key"))?
            ),
            escape(
                value
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("invalid environment path"))?
            )
        ));
    }
    xml.push_str("</dict>\n</dict></plist>\n");
    Ok(xml)
}

pub fn managed_plist_path(launch_agents_dir: &Path) -> PathBuf {
    launch_agents_dir.join(format!("{SERVICE_LABEL}.plist"))
}

// Only byte-exact generated content is ours. A marker pasted into an arbitrary
// plist is not sufficient authority to replace or delete it.
fn owned(bytes: &[u8], config: &WorkerConfig) -> Result<()> {
    ensure!(
        bytes == render(config)?.as_bytes(),
        "refusing unknown, changed, or differently versioned launch agent"
    );
    Ok(())
}

pub fn install_plist(
    paths: &Paths,
    config: &WorkerConfig,
    launch_agents_dir: &Path,
) -> Result<PathBuf> {
    let xml = render(config)?;
    // Do not recursively create arbitrary caller-supplied directory trees.
    let parent = launch_agents_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("agent directory has no parent"))?;
    ensure!(
        std::fs::symlink_metadata(parent)?.is_dir(),
        "agent parent must exist"
    );
    let directory = state::directory_policy(launch_agents_dir, true, false)?;
    let path = managed_plist_path(launch_agents_dir);
    let name = CString::new(format!("{SERVICE_LABEL}.plist"))?;
    let old = state::existing_at(&directory, &name)?;
    if let Some(bytes) = &old {
        owned(bytes, config)?;
    }
    // The caller's config is authoritative; installing never touches queue,
    // credentials, or the background state root.
    let _ = paths;
    state::write_at(&directory, &name, xml.as_bytes(), old.as_deref())?;
    Ok(path)
}

pub fn remove_plist(paths: &Paths, config: &WorkerConfig, launch_agents_dir: &Path) -> Result<()> {
    let _ = paths;
    match std::fs::symlink_metadata(launch_agents_dir) {
        Ok(metadata) => ensure!(metadata.is_dir(), "agent directory must not be a symlink"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let directory = state::directory_policy(launch_agents_dir, false, false)?;
    let name = CString::new(format!("{SERVICE_LABEL}.plist"))?;
    let Some(bytes) = state::existing_at(&directory, &name)? else {
        return Ok(());
    };
    owned(&bytes, config)?;
    state::remove_at(&directory, &name, &bytes)
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn plist_is_valid_xml() {
        use std::{
            io::Write,
            process::{Command, Stdio},
        };
        let (_temp, cfg) = crate::config::tests::fixture();
        let mut child = Command::new("/usr/bin/plutil")
            .args(["-lint", "-s", "--", "-"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(render(&cfg).unwrap().as_bytes())
            .unwrap();
        assert!(child.wait().unwrap().success());
    }

    #[test]
    fn escaped_shell_free_and_owned_removal() {
        let (temp, cfg) = crate::config::tests::fixture();
        let xml = render(&cfg).unwrap();
        assert!(xml.contains("hive&lt;&amp;&quot;&apos;"));
        assert!(xml.contains(MARKER));
        for forbidden in ["KeepAlive", "NVIM", "/bin/sh", "TOKEN", "launchctl"] {
            assert!(!xml.contains(forbidden));
        }
        assert!(xml.contains("<string>run-once</string>"));
        let paths = Paths::in_root(temp.path().join("state"));
        let agents = temp.path().join("agents");
        let path = install_plist(&paths, &cfg, &agents).unwrap();
        install_plist(&paths, &cfg, &agents).unwrap();
        assert!(!paths.root.exists());
        std::fs::write(agents.join("unrelated.plist"), b"unrelated").unwrap();
        remove_plist(&paths, &cfg, &agents).unwrap();
        assert!(!path.exists());
        assert!(agents.join("unrelated.plist").exists());
    }

    #[test]
    fn collision_and_marker_spoof_refused() {
        let (temp, cfg) = crate::config::tests::fixture();
        let paths = Paths::in_root(temp.path().join("state"));
        let agents = temp.path().join("agents");
        let path = install_plist(&paths, &cfg, &agents).unwrap();
        let spoof = format!("{MARKER}<plist>unowned</plist>");
        std::fs::write(&path, spoof.as_bytes()).unwrap();
        assert!(install_plist(&paths, &cfg, &agents).is_err());
        assert!(remove_plist(&paths, &cfg, &agents).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), spoof.as_bytes());
    }

    #[test]
    fn existing_directory_and_symlink_refusal() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let (temp, cfg) = crate::config::tests::fixture();
        let paths = Paths::in_root(temp.path().join("state"));
        let agents = temp.path().join("agents");
        std::fs::create_dir(&agents).unwrap();
        std::fs::set_permissions(&agents, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = install_plist(&paths, &cfg, &agents).unwrap();
        std::fs::remove_file(&path).unwrap();
        symlink(temp.path().join("absent"), &path).unwrap();
        assert!(install_plist(&paths, &cfg, &agents).is_err());
        assert!(remove_plist(&paths, &cfg, &agents).is_err());
        let alias = temp.path().join("alias");
        symlink(&agents, &alias).unwrap();
        assert!(install_plist(&paths, &cfg, &alias).is_err());
        assert!(remove_plist(&paths, &cfg, &alias).is_err());
        assert!(install_plist(&paths, &cfg, &temp.path().join("missing/agents")).is_err());
        assert!(!temp.path().join("missing").exists());
    }
}

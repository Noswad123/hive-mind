//! Native, non-synchronizing Keychain storage. Device-local means no cloud sync,
//! not hardware binding. Callers must hold `lock` across load/network/save (and
//! status reads); load/save deliberately never acquire a second lock.
use crate::{
    AuthError, AuthProfile, ClientConfig, CredentialGuard, CredentialStore, GoogleIdentity,
    GrantRecord, Result, SecretString, StoredConnection, validate_connection,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use zeroize::Zeroizing;

const MAX_RECORD: usize = 64 * 1024;
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Stable OS-user identity for coordinating local services, never inherited HOME/XDG.
pub fn system_user_home() -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    return os_user_home(unsafe { libc::geteuid() });
    #[cfg(not(target_os = "macos"))]
    Err(AuthError::UnsupportedPlatform)
}

/// Construction resolves the OS user's home but creates no files and performs
/// no Keychain I/O. App configuration must never split the credential lock domain.
pub struct MacKeychainStore {
    profile: AuthProfile,
    lock_dir: PathBuf,
    background: bool,
}

impl MacKeychainStore {
    pub fn new() -> Result<Self> {
        Self::for_profile(AuthProfile::TasksReadonly)
    }

    pub fn for_profile(profile: AuthProfile) -> Result<Self> {
        #[cfg(target_os = "macos")]
        return Self::with_profile_os_home(os_user_home, false, profile);
        #[cfg(not(target_os = "macos"))]
        {
            let _ = profile;
            Err(AuthError::UnsupportedPlatform)
        }
    }

    /// Uses the same credentials and lock as `new`, but fails rather than
    /// displaying authentication UI on any Keychain read or write.
    pub fn new_background() -> Result<Self> {
        #[cfg(target_os = "macos")]
        return Self::with_profile_os_home(os_user_home, true, AuthProfile::TasksReadonly);
        #[cfg(not(target_os = "macos"))]
        Err(AuthError::UnsupportedPlatform)
    }

    #[cfg(all(test, unix))]
    fn with_os_home(
        resolve: impl FnOnce(libc::uid_t) -> Result<PathBuf>,
        background: bool,
    ) -> Result<Self> {
        Self::with_profile_os_home(resolve, background, AuthProfile::TasksReadonly)
    }

    #[cfg(unix)]
    fn with_profile_os_home(
        resolve: impl FnOnce(libc::uid_t) -> Result<PathBuf>,
        background: bool,
        profile: AuthProfile,
    ) -> Result<Self> {
        let home =
            resolve(unsafe { libc::geteuid() }).map_err(|_| AuthError::StorageUnavailable)?;
        if !home.is_absolute()
            || home.components().any(|part| {
                !matches!(
                    part,
                    std::path::Component::RootDir | std::path::Component::Normal(_)
                )
            })
        {
            return Err(AuthError::StorageUnavailable);
        }
        Ok(Self {
            profile,
            lock_dir: home
                .join("Library/Application Support/Hive")
                .join(profile.lock_namespace()),
            background,
        })
    }

    #[cfg(all(test, unix))]
    fn with_lock_dir(lock_dir: PathBuf) -> Self {
        Self {
            profile: AuthProfile::TasksReadonly,
            lock_dir,
            background: false,
        }
    }
}

#[cfg(target_os = "macos")]
fn os_user_home(uid: libc::uid_t) -> Result<PathBuf> {
    use std::{ffi::CStr, os::unix::ffi::OsStrExt};
    let mut buffer = vec![0u8; 1024];
    loop {
        let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut found = std::ptr::null_mut();
        // getpwuid_r owns no returned allocation: all strings live in buffer.
        let status = unsafe {
            libc::getpwuid_r(
                uid,
                entry.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut found,
            )
        };
        if status == libc::ERANGE && buffer.len() < 1024 * 1024 {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if status != 0 || found.is_null() {
            return Err(AuthError::StorageUnavailable);
        }
        let entry = unsafe { entry.assume_init() };
        if entry.pw_uid != uid || entry.pw_dir.is_null() {
            return Err(AuthError::StorageUnavailable);
        }
        let home = unsafe { CStr::from_ptr(entry.pw_dir) };
        return Ok(PathBuf::from(std::ffi::OsStr::from_bytes(home.to_bytes())));
    }
}

impl CredentialStore for MacKeychainStore {
    fn profile(&self) -> AuthProfile {
        self.profile
    }

    fn lock(&self, connection: &str) -> Result<Box<dyn CredentialGuard + '_>> {
        validate_connection(connection)?;
        #[cfg(target_os = "macos")]
        return Ok(Box::new(FileLock::acquire(
            &self.lock_dir,
            connection,
            LOCK_WAIT,
        )?));
        #[cfg(not(target_os = "macos"))]
        Err(AuthError::UnsupportedPlatform)
    }

    fn load(&self, connection: &str) -> Result<Option<StoredConnection>> {
        validate_connection(connection)?;
        #[cfg(target_os = "macos")]
        return load_with(
            &NativeKeychain {
                profile: self.profile,
                background: self.background,
            },
            connection,
        );
        #[cfg(not(target_os = "macos"))]
        Err(AuthError::UnsupportedPlatform)
    }

    fn save(&self, connection: &str, record: &StoredConnection) -> Result<()> {
        validate_connection(connection)?;
        #[cfg(target_os = "macos")]
        return save_with(
            &NativeKeychain {
                profile: self.profile,
                background: self.background,
            },
            connection,
            record,
        );
        #[cfg(not(target_os = "macos"))]
        {
            let _ = record;
            Err(AuthError::UnsupportedPlatform)
        }
    }
}

// The backend boundary exposes only numeric OSStatus values, never native error
// messages or Debug output. Tests use fakes, never the user's Keychain.
trait KeychainBackend {
    fn profile(&self) -> AuthProfile {
        AuthProfile::TasksReadonly
    }
    fn read(&self, account: &str) -> std::result::Result<Zeroizing<Vec<u8>>, i32>;
    fn write(&self, account: &str, bytes: &[u8]) -> std::result::Result<(), i32>;
}

#[cfg(target_os = "macos")]
struct NativeKeychain {
    profile: AuthProfile,
    background: bool,
}

// security-framework-sys 2.17 exposes the UI key but not the Fail value.
#[cfg(target_os = "macos")]
#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecUseAuthenticationUIFail: core_foundation::string::CFStringRef;
}

#[cfg(target_os = "macos")]
fn options(
    account: &str,
    background: bool,
    profile: AuthProfile,
) -> security_framework::passwords::PasswordOptions {
    let mut options = security_framework::passwords::PasswordOptions::new_generic_password(
        profile.service(),
        account,
    );
    options.set_access_synchronized(Some(false));
    if background {
        use core_foundation::{base::TCFType, string::CFString};
        // No setter exists in 3.7. These framework constants are retained by
        // the options and apply to reads, adds, and duplicate-item updates.
        #[allow(deprecated)]
        options.query.push(unsafe {
            (
                CFString::wrap_under_get_rule(
                    security_framework_sys::item::kSecUseAuthenticationUI,
                ),
                CFString::wrap_under_get_rule(kSecUseAuthenticationUIFail).into_CFType(),
            )
        });
    }
    options
}

#[cfg(target_os = "macos")]
impl KeychainBackend for NativeKeychain {
    fn profile(&self) -> AuthProfile {
        self.profile
    }
    fn read(&self, account: &str) -> std::result::Result<Zeroizing<Vec<u8>>, i32> {
        security_framework::passwords::generic_password(options(
            account,
            self.background,
            self.profile,
        ))
        .map(Zeroizing::new)
        .map_err(|e| e.code())
    }

    fn write(&self, account: &str, bytes: &[u8]) -> std::result::Result<(), i32> {
        // SecItemAdd, then SecItemUpdate on duplicate; never delete/re-add.
        security_framework::passwords::set_generic_password_options(
            bytes,
            options(account, self.background, self.profile),
        )
        .map_err(|e| e.code())
    }
}

fn native_error(code: i32) -> AuthError {
    match code {
        -128 | -25293 => AuthError::StorageDenied,
        -25308 | -25315 => AuthError::StorageLocked, // interaction not allowed / required
        -26275 => AuthError::StorageCorrupt,         // errSecDecode
        _ => AuthError::StorageUnavailable,
    }
}

fn load_with(backend: &impl KeychainBackend, connection: &str) -> Result<Option<StoredConnection>> {
    validate_connection(connection)?;
    match backend.read(connection) {
        Ok(bytes) => decode_for(backend.profile(), connection, &bytes).map(Some),
        Err(-25300) => Ok(None), // errSecItemNotFound is only absence on reads.
        Err(code) => Err(native_error(code)),
    }
}

fn save_with(
    backend: &impl KeychainBackend,
    connection: &str,
    record: &StoredConnection,
) -> Result<()> {
    let bytes = encode_for(backend.profile(), connection, record)?;
    backend.write(connection, &bytes).map_err(native_error)
}

// SecretString intentionally has no public Serialize implementation. Only this
// private DTO may expose its contents to the bounded Keychain byte encoder.
struct WireSecret(SecretString);

impl Serialize for WireSecret {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0.expose())
    }
}

impl<'de> Deserialize<'de> for WireSecret {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let mut value = Zeroizing::new(String::deserialize(deserializer)?);
        if value.trim().is_empty() || value.len() > 16 * 1024 || value.chars().any(char::is_control)
        {
            return Err(serde::de::Error::custom("invalid stored secret"));
        }
        // Move, rather than copy, the allocation into SecretString's zeroizing buffer.
        Ok(Self(SecretString(Zeroizing::new(std::mem::take(
            &mut *value,
        )))))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireClient {
    client_id: String,
    client_secret: Option<WireSecret>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireGrant {
    client: WireClient,
    refresh_token: WireSecret,
    scopes: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRecord {
    version: u32,
    connection: String,
    identity: GoogleIdentity,
    generation: u64,
    grant: Option<WireGrant>,
}

fn validate_record(
    profile: AuthProfile,
    connection: &str,
    record: &StoredConnection,
) -> Result<()> {
    validate_connection(connection)?;
    if record.version != 1
        || record.generation == 0
        || record.connection != connection
        || record.identity.validate().is_err()
    {
        return Err(AuthError::StorageCorrupt);
    }
    if let Some(grant) = &record.grant {
        let id = &grant.client.client_id;
        let prefix = id.strip_suffix(".apps.googleusercontent.com").unwrap_or("");
        let valid_secret = |s: &SecretString| {
            !s.expose().trim().is_empty()
                && s.expose().len() <= 16 * 1024
                && !s.expose().chars().any(char::is_control)
        };
        if id.len() > 512
            || prefix.is_empty()
            || !prefix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            || !valid_secret(&grant.refresh_token)
            || grant
                .client
                .client_secret
                .as_ref()
                .is_some_and(|s| !valid_secret(s))
            || !profile.accepts_scopes(&grant.scopes)
        {
            return Err(AuthError::StorageCorrupt);
        }
    }
    Ok(())
}

struct BoundedBytes(Zeroizing<Vec<u8>>);

impl std::io::Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_RECORD.saturating_sub(self.0.len()) {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn encode_for(
    profile: AuthProfile,
    connection: &str,
    record: &StoredConnection,
) -> Result<Zeroizing<Vec<u8>>> {
    validate_record(profile, connection, record)?;
    let wire = WireRecord {
        version: record.version,
        connection: record.connection.clone(),
        identity: record.identity.clone(),
        generation: record.generation,
        grant: record.grant.as_ref().map(|grant| WireGrant {
            client: WireClient {
                client_id: grant.client.client_id.clone(),
                client_secret: grant.client.client_secret.clone().map(WireSecret),
            },
            refresh_token: WireSecret(grant.refresh_token.clone()),
            scopes: grant.scopes.clone(),
        }),
    };
    // Reserve the bound up front so growing the secret buffer never leaves old
    // allocations behind. Zeroization is best effort, not an HTTP-copy guarantee.
    let mut bytes = BoundedBytes(Zeroizing::new(Vec::with_capacity(MAX_RECORD)));
    serde_json::to_writer(&mut bytes, &wire).map_err(|_| AuthError::StorageCorrupt)?;
    Ok(bytes.0)
}

fn decode_for(profile: AuthProfile, connection: &str, bytes: &[u8]) -> Result<StoredConnection> {
    validate_connection(connection)?;
    if bytes.is_empty() || bytes.len() > MAX_RECORD {
        return Err(AuthError::StorageCorrupt);
    }
    let wire: WireRecord = serde_json::from_slice(bytes).map_err(|_| AuthError::StorageCorrupt)?;
    let record = StoredConnection {
        version: wire.version,
        connection: wire.connection,
        identity: wire.identity,
        generation: wire.generation,
        grant: wire.grant.map(|grant| GrantRecord {
            client: ClientConfig {
                client_id: grant.client.client_id,
                client_secret: grant.client.client_secret.map(|s| s.0),
            },
            refresh_token: grant.refresh_token.0,
            scopes: grant.scopes,
        }),
    };
    validate_record(profile, connection, &record)?;
    Ok(record)
}

#[cfg(unix)]
struct FileLock {
    // Closing the descriptor releases flock even on process exit. Never unlink:
    // deleting a lock file would let later callers lock a different inode.
    _file: std::fs::File,
}

#[cfg(unix)]
impl CredentialGuard for FileLock {}

#[cfg(unix)]
impl FileLock {
    fn acquire(
        path: &std::path::Path,
        connection: &str,
        wait: std::time::Duration,
    ) -> Result<Self> {
        use sha2::{Digest, Sha256};
        use std::{
            ffi::CString,
            os::fd::{AsRawFd, FromRawFd},
            os::unix::fs::MetadataExt,
            path::Component,
        };
        validate_connection(connection)?;
        if !path.is_absolute() {
            return Err(AuthError::StorageUnavailable);
        }
        let fail = |_| AuthError::StorageUnavailable;
        // Descriptor-relative traversal prevents symlink and path-swap races.
        let mut dir = std::fs::File::open("/").map_err(fail)?;
        let parts: Vec<_> = path.components().collect();
        let uid = unsafe { libc::geteuid() };
        for (index, part) in parts.iter().enumerate() {
            let Component::Normal(name) = part else {
                if matches!(part, Component::RootDir) {
                    continue;
                }
                return Err(AuthError::StorageUnavailable);
            };
            use std::os::unix::ffi::OsStrExt;
            let name = CString::new(name.as_bytes()).map_err(|_| AuthError::StorageUnavailable)?;
            let parent = dir.as_raw_fd();
            let created = unsafe { libc::mkdirat(parent, name.as_ptr(), 0o700) } == 0;
            if !created && std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) {
                return Err(AuthError::StorageUnavailable);
            }
            let fd = unsafe {
                libc::openat(
                    parent,
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(AuthError::StorageUnavailable);
            }
            let child = unsafe { std::fs::File::from_raw_fd(fd) };
            let meta = child.metadata().map_err(fail)?;
            let private = index + 2 >= parts.len() || created;
            let trusted_owner = meta.uid() == uid || meta.uid() == 0;
            let writable = meta.mode() & 0o022 != 0;
            let sticky_root = meta.uid() == 0 && meta.mode() & u32::from(libc::S_ISVTX) != 0;
            if !meta.is_dir()
                || !trusted_owner
                || (private && (meta.uid() != uid || meta.mode() & 0o7777 != 0o700))
                || (!private && writable && !sticky_root)
            {
                return Err(AuthError::StorageUnavailable);
            }
            dir = child;
        }
        let name =
            CString::new(format!("{:x}.lock", Sha256::digest(connection.as_bytes()))).unwrap();
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR
                    | libc::O_CREAT
                    | libc::O_NOFOLLOW
                    | libc::O_CLOEXEC
                    | libc::O_NONBLOCK,
                0o600,
            )
        };
        if fd < 0 {
            return Err(AuthError::StorageUnavailable);
        }
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        let check = || -> Result<()> {
            let meta = file.metadata().map_err(fail)?;
            let mut named = std::mem::MaybeUninit::<libc::stat>::uninit();
            if unsafe {
                libc::fstatat(
                    dir.as_raw_fd(),
                    name.as_ptr(),
                    named.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } != 0
            {
                return Err(AuthError::StorageUnavailable);
            }
            let named = unsafe { named.assume_init() };
            if !meta.is_file()
                || meta.uid() != uid
                || meta.mode() & 0o7777 != 0o600
                || meta.nlink() != 1
                || meta.ino() != named.st_ino
                || meta.dev() != named.st_dev as u64
            {
                return Err(AuthError::StorageUnavailable);
            }
            Ok(())
        };
        check()?;
        let start = std::time::Instant::now();
        loop {
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                check()?;
                return Ok(Self { _file: file });
            }
            let error = std::io::Error::last_os_error().raw_os_error();
            if !matches!(error, Some(libc::EWOULDBLOCK) | Some(libc::EINTR)) {
                return Err(AuthError::StorageUnavailable);
            }
            if start.elapsed() >= wait {
                return Err(AuthError::Busy);
            }
            std::thread::sleep(
                std::time::Duration::from_millis(20).min(wait.saturating_sub(start.elapsed())),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::REQUIRED_SCOPES;

    fn encode(connection: &str, record: &StoredConnection) -> Result<Zeroizing<Vec<u8>>> {
        encode_for(AuthProfile::TasksReadonly, connection, record)
    }

    fn decode(connection: &str, bytes: &[u8]) -> Result<StoredConnection> {
        decode_for(AuthProfile::TasksReadonly, connection, bytes)
    }

    #[test]
    fn profile_codecs_reject_cross_combined_extra_and_duplicate_scopes() {
        for profile in [AuthProfile::TasksReadonly, AuthProfile::CalendarEvents] {
            let mut original = record();
            let exact: Vec<String> = profile.scopes().iter().map(|s| (*s).into()).collect();
            original.grant.as_mut().unwrap().scopes = exact.clone();
            let bytes = encode_for(profile, "work", &original).unwrap();
            assert_eq!(
                decode_for(profile, "work", &bytes)
                    .unwrap()
                    .grant
                    .unwrap()
                    .scopes,
                exact
            );
            let other = if profile == AuthProfile::TasksReadonly {
                AuthProfile::CalendarEvents
            } else {
                AuthProfile::TasksReadonly
            };
            assert!(matches!(
                decode_for(other, "work", &bytes),
                Err(AuthError::StorageCorrupt)
            ));
            for scopes in [
                other.scopes().iter().map(|s| (*s).into()).collect(),
                vec![
                    crate::TASKS_SCOPE.into(),
                    crate::CALENDAR_EVENTS_SCOPE.into(),
                    "openid".into(),
                ],
                [exact.clone(), vec!["extra".into()]].concat(),
                vec![exact[0].clone(), exact[0].clone()],
                [exact.clone(), vec!["openid".into()]].concat(),
            ] {
                original.grant.as_mut().unwrap().scopes = scopes.clone();
                assert!(matches!(
                    encode_for(profile, "work", &original),
                    Err(AuthError::StorageCorrupt)
                ));
                let mut wire: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                wire["grant"]["scopes"] = serde_json::to_value(scopes).unwrap();
                assert!(matches!(
                    decode_for(profile, "work", &serde_json::to_vec(&wire).unwrap()),
                    Err(AuthError::StorageCorrupt)
                ));
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn profile_namespaces_isolate_same_alias_without_native_io() {
        use core_foundation::{base::TCFType, string::CFString};
        use std::{collections::HashMap, sync::Mutex, time::Duration};
        type Records = Mutex<HashMap<(&'static str, String), Zeroizing<Vec<u8>>>>;
        struct Memory<'a>(AuthProfile, &'a Records);
        impl KeychainBackend for Memory<'_> {
            fn profile(&self) -> AuthProfile {
                self.0
            }
            fn read(&self, account: &str) -> std::result::Result<Zeroizing<Vec<u8>>, i32> {
                self.1
                    .lock()
                    .unwrap()
                    .get(&(self.0.service(), account.into()))
                    .cloned()
                    .ok_or(-25300)
            }
            fn write(&self, account: &str, bytes: &[u8]) -> std::result::Result<(), i32> {
                self.1.lock().unwrap().insert(
                    (self.0.service(), account.into()),
                    Zeroizing::new(bytes.to_vec()),
                );
                Ok(())
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let records = Records::default();
        let tasks = Memory(AuthProfile::TasksReadonly, &records);
        let calendar = Memory(AuthProfile::CalendarEvents, &records);
        let tasks_store = MacKeychainStore::with_os_home(|_| Ok(home.clone()), false).unwrap();
        let calendar_store =
            MacKeychainStore::with_profile_os_home(|_| Ok(home.clone()), false, calendar.0)
                .unwrap();
        assert_eq!(tasks_store.profile(), AuthProfile::TasksReadonly);
        assert_eq!(calendar_store.profile(), AuthProfile::CalendarEvents);
        assert_eq!(
            calendar_store.lock_dir,
            home.join("Library/Application Support/Hive/google-calendar-auth")
        );
        assert_eq!(calendar.0.service(), "hive/google-calendar/v1");
        let _tasks_lock = FileLock::acquire(&tasks_store.lock_dir, "work", Duration::ZERO).unwrap();
        let _calendar_lock =
            FileLock::acquire(&calendar_store.lock_dir, "work", Duration::ZERO).unwrap();
        for profile in [tasks.0, calendar.0] {
            let options = options("work", false, profile);
            let key = unsafe {
                CFString::wrap_under_get_rule(security_framework_sys::item::kSecAttrService)
            };
            #[allow(deprecated)]
            let service = options
                .query
                .iter()
                .find(|(k, _)| *k == key)
                .unwrap()
                .1
                .clone();
            assert_eq!(service, CFString::new(profile.service()).into_CFType());
        }
        save_with(&tasks, "work", &record()).unwrap();
        let tasks_bytes = records.lock().unwrap()[&(tasks.0.service(), "work".into())].clone();
        assert!(load_with(&calendar, "work").unwrap().is_none());
        let mut calendar_record = record();
        calendar_record.grant.as_mut().unwrap().scopes =
            calendar.0.scopes().iter().map(|s| (*s).into()).collect();
        save_with(&calendar, "work", &calendar_record).unwrap();
        calendar_record.generation += 1;
        calendar_record.grant = None;
        save_with(&calendar, "work", &calendar_record).unwrap();
        assert!(
            load_with(&calendar, "work")
                .unwrap()
                .unwrap()
                .grant
                .is_none()
        );
        assert_eq!(
            &*records.lock().unwrap()[&(tasks.0.service(), "work".into())],
            &*tasks_bytes
        );
        assert!(load_with(&tasks, "work").unwrap().unwrap().grant.is_some());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn background_options_fail_ui_without_changing_credential_identity() {
        use core_foundation::{base::TCFType, boolean::CFBoolean, string::CFString};
        use security_framework_sys::item::{
            kSecAttrAccount, kSecAttrService, kSecAttrSynchronizable, kSecUseAuthenticationUI,
        };

        for background in [false, true] {
            let options = options("work", background, AuthProfile::TasksReadonly);
            #[allow(deprecated)]
            let query = &options.query;
            let value = |key| {
                let key = unsafe { CFString::wrap_under_get_rule(key) };
                let values: Vec<_> = query.iter().filter(|(k, _)| *k == key).collect();
                assert!(values.len() <= 1);
                values.first().map(|(_, value)| value.clone())
            };
            assert_eq!(query.len(), if background { 5 } else { 4 });
            assert_eq!(
                value(unsafe { kSecAttrService }),
                Some(CFString::new(AuthProfile::TasksReadonly.service()).into_CFType())
            );
            assert_eq!(
                value(unsafe { kSecAttrAccount }),
                Some(CFString::new("work").into_CFType())
            );
            assert_eq!(
                value(unsafe { kSecAttrSynchronizable }),
                Some(CFBoolean::false_value().into_CFType())
            );
            assert_eq!(
                value(unsafe { kSecUseAuthenticationUI }),
                background.then(|| unsafe {
                    CFString::wrap_under_get_rule(kSecUseAuthenticationUIFail).into_CFType()
                })
            );
        }
    }

    fn record() -> StoredConnection {
        StoredConnection {
            version: 1,
            connection: "work".into(),
            identity: GoogleIdentity::new("12345").unwrap(),
            generation: 7,
            grant: Some(GrantRecord {
                client: ClientConfig {
                    client_id: "123-desktop.apps.googleusercontent.com".into(),
                    client_secret: Some(SecretString::new("CLIENT_CANARY").unwrap()),
                },
                refresh_token: SecretString::new("REFRESH_CANARY").unwrap(),
                scopes: REQUIRED_SCOPES.iter().map(|s| (*s).into()).collect(),
            }),
        }
    }

    #[test]
    fn private_codec_roundtrip_and_redaction() {
        let original = record();
        let bytes = encode("work", &original).unwrap();
        let decoded = decode("work", &bytes).unwrap();
        assert_eq!(decoded.identity, original.identity);
        assert_eq!(decoded.generation, 7);
        assert!(decoded.grant.as_ref().unwrap().refresh_token.expose() == "REFRESH_CANARY");
        let debug = format!("{decoded:?}");
        assert!(!debug.contains("REFRESH_CANARY"));
        assert!(!debug.contains("CLIENT_CANARY"));
        assert!(
            !std::str::from_utf8(&bytes)
                .unwrap()
                .contains("access_token")
        );
    }

    #[test]
    fn tombstone_retains_binding_only() {
        let mut original = record();
        original.grant = None;
        let bytes = encode("work", &original).unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        for forbidden in [
            "CLIENT_CANARY",
            "REFRESH_CANARY",
            "client_secret",
            "client_id",
            "refresh_token",
            "scopes",
        ] {
            assert!(!text.contains(forbidden));
        }
        let decoded = decode("work", &bytes).unwrap();
        assert!(decoded.grant.is_none());
        assert_eq!(decoded.identity, original.identity);
        assert_eq!(decoded.generation, original.generation);
    }

    #[test]
    fn rejects_wrong_version_alias_identity_client_and_scopes() {
        let mut bad = record();
        bad.version = 2;
        assert!(matches!(
            encode("work", &bad),
            Err(AuthError::StorageCorrupt)
        ));
        assert!(matches!(
            encode("other", &record()),
            Err(AuthError::StorageCorrupt)
        ));
        assert!(matches!(
            encode("../work", &record()),
            Err(AuthError::InvalidConnection)
        ));
        let bytes = encode("work", &record()).unwrap();
        assert!(matches!(
            decode("other", &bytes),
            Err(AuthError::StorageCorrupt)
        ));
        for id in [
            "bad",
            ".apps.googleusercontent.com",
            "a.apps.googleusercontent.com.evil",
            "a\n.apps.googleusercontent.com",
        ] {
            let mut bad = record();
            bad.grant.as_mut().unwrap().client.client_id = id.into();
            assert!(encode("work", &bad).is_err());
        }
        for scopes in [
            vec![],
            vec!["openid".into()],
            vec!["openid".into(), "openid".into()],
            vec!["openid".into(), "broad".into()],
        ] {
            let mut bad = record();
            bad.grant.as_mut().unwrap().scopes = scopes;
            assert!(encode("work", &bad).is_err());
        }
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["identity"]["issuer"] = "https://evil.invalid".into();
        assert!(decode("work", &serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn generation_and_client_id_bounds_match_provider() {
        let mut bad = record();
        bad.generation = 0;
        assert!(matches!(
            encode("work", &bad),
            Err(AuthError::StorageCorrupt)
        ));
        bad.grant = None;
        assert!(matches!(
            encode("work", &bad),
            Err(AuthError::StorageCorrupt)
        ));
        let bytes = encode("work", &record()).unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(matches!(
            decode(
                "work",
                text.replace("\"generation\":7", "\"generation\":0")
                    .as_bytes()
            ),
            Err(AuthError::StorageCorrupt)
        ));
        let suffix = ".apps.googleusercontent.com";
        for length in [512, 513] {
            let id = format!("{}{suffix}", "a".repeat(length - suffix.len()));
            let mut candidate = record();
            candidate.grant.as_mut().unwrap().client.client_id = id.clone();
            assert_eq!(encode("work", &candidate).is_ok(), length == 512);
            let json = text.replace("123-desktop.apps.googleusercontent.com", &id);
            assert_eq!(decode("work", json.as_bytes()).is_ok(), length == 512);
        }
    }

    #[test]
    fn strict_json_and_bounds() {
        let bytes = encode("work", &record()).unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        for json in [
            text.replacen("{", "{\"version\":1,", 1),
            text.replacen("{", "{\"unknown\":null,", 1),
            text.replace("\"client_id\":", "\"unknown\":null,\"client_id\":"),
            text.replace(
                "\"refresh_token\":",
                "\"refresh_token\":\"duplicate\",\"refresh_token\":",
            ),
            text.replace("\"scopes\":", "\"access_token\":\"FORBIDDEN\",\"scopes\":"),
            text.replace("REFRESH_CANARY", ""),
            text.replace("REFRESH_CANARY", &"x".repeat(16 * 1024 + 1)),
            text.replace("REFRESH_CANARY", "bad\\nsecret"),
            text.replace("\"version\":1", "\"version\":2"),
        ] {
            assert!(matches!(
                decode("work", json.as_bytes()),
                Err(AuthError::StorageCorrupt)
            ));
        }
        assert!(decode("work", &vec![b' '; MAX_RECORD + 1]).is_err());
        assert!(decode("work", b"\xff").is_err());
        assert!(decode("work", b"").is_err());
        let mut bounded = BoundedBytes(Zeroizing::new(vec![0; MAX_RECORD]));
        assert!(std::io::Write::write(&mut bounded, b"x").is_err());
        let mut large = record();
        let grant = large.grant.as_mut().unwrap();
        grant.refresh_token = SecretString::new("\"".repeat(16 * 1024)).unwrap();
        grant.client.client_secret = Some(SecretString::new("\"".repeat(16 * 1024)).unwrap());
        assert!(matches!(
            encode("work", &large),
            Err(AuthError::StorageCorrupt)
        ));
    }

    struct Fake(i32);
    impl KeychainBackend for Fake {
        fn read(&self, account: &str) -> std::result::Result<Zeroizing<Vec<u8>>, i32> {
            assert_eq!(account, "work");
            if self.0 == 0 {
                Ok(Zeroizing::new(b"bad encoded record".to_vec()))
            } else {
                Err(self.0)
            }
        }
        fn write(&self, account: &str, _: &[u8]) -> std::result::Result<(), i32> {
            assert_eq!(account, "work");
            Err(self.0)
        }
    }

    #[test]
    fn backend_errors_are_sanitized() {
        assert!(load_with(&Fake(-25300), "work").unwrap().is_none());
        assert!(matches!(
            load_with(&Fake(0), "work"),
            Err(AuthError::StorageCorrupt)
        ));
        for (code, expected) in [
            (-128, AuthError::StorageDenied),
            (-25293, AuthError::StorageDenied),
            (-25308, AuthError::StorageLocked),
            (-25315, AuthError::StorageLocked),
            (-26275, AuthError::StorageCorrupt),
            (-25294, AuthError::StorageUnavailable),
            (-1, AuthError::StorageUnavailable),
        ] {
            assert_eq!(load_with(&Fake(code), "work").unwrap_err(), expected);
            assert_eq!(
                save_with(&Fake(code), "work", &record()).unwrap_err(),
                expected
            );
        }
        assert_eq!(
            save_with(&Fake(-25300), "work", &record()).unwrap_err(),
            AuthError::StorageUnavailable
        );
        assert_eq!(
            AuthProfile::TasksReadonly.service(),
            "hive/google-captures/v1"
        );
    }

    #[cfg(unix)]
    fn lock_path(temp: &tempfile::TempDir) -> PathBuf {
        temp.path().canonicalize().unwrap().join("hive/google-auth")
    }

    #[cfg(unix)]
    #[test]
    fn os_home_resolution_is_fail_closed_and_does_not_create_files() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let store = MacKeychainStore::with_os_home(
            |uid| {
                assert_eq!(uid, unsafe { libc::geteuid() });
                Ok(home.clone())
            },
            false,
        )
        .unwrap();
        assert_eq!(
            store.lock_dir,
            home.join("Library/Application Support/Hive/google-auth")
        );
        assert_eq!(std::fs::read_dir(&home).unwrap().count(), 0);
        assert!(!store.background);
        let background = MacKeychainStore::with_os_home(|_| Ok(home.clone()), true).unwrap();
        assert!(background.background);
        assert_eq!(store.lock_dir, background.lock_dir);
        assert_eq!(std::fs::read_dir(&home).unwrap().count(), 0);
        assert!(matches!(
            MacKeychainStore::with_os_home(|_| Err(AuthError::StorageUnavailable), false),
            Err(AuthError::StorageUnavailable)
        ));
        for invalid in ["", "relative", "/home/../other"] {
            assert!(matches!(
                MacKeychainStore::with_os_home(|_| Ok(invalid.into()), true),
                Err(AuthError::StorageUnavailable)
            ));
        }
        assert_eq!(std::fs::read_dir(&home).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn app_environments_share_os_user_lock_namespace() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let store = MacKeychainStore::with_os_home(|_| Ok(home.clone()), false).unwrap();
        let _guard = FileLock::acquire(&store.lock_dir, "work", LOCK_WAIT).unwrap();
        for (config, background) in [("app-a", false), ("app-b", true)] {
            // Only the child's environment varies. The same private resolver
            // seam used by production supplies the OS home, not HOME or XDG.
            assert!(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "storage::tests::child_lock_probe"])
                    .env("HOME", home.join(config))
                    .env("XDG_STATE_HOME", home.join(config).join("state"))
                    .env("HIVE_STORAGE_TEST_OS_HOME", &home)
                    .env("HIVE_STORAGE_TEST_LOCK_PATH", &store.lock_dir)
                    .env("HIVE_STORAGE_TEST_BUSY", "1")
                    .env(
                        "HIVE_STORAGE_TEST_BACKGROUND",
                        if background { "1" } else { "0" }
                    )
                    .env_remove("HIVE_STORAGE_TEST_HOLD")
                    .status()
                    .unwrap()
                    .success()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn paused_refresh_and_logout_share_one_fake_credential_lock() {
        use std::sync::{Mutex, mpsc};
        struct Memory(Mutex<Zeroizing<Vec<u8>>>);
        impl KeychainBackend for Memory {
            fn read(&self, account: &str) -> std::result::Result<Zeroizing<Vec<u8>>, i32> {
                assert_eq!(account, "work");
                Ok(self.0.lock().unwrap().clone())
            }
            fn write(&self, account: &str, bytes: &[u8]) -> std::result::Result<(), i32> {
                assert_eq!(account, "work");
                *self.0.lock().unwrap() = Zeroizing::new(bytes.to_vec());
                Ok(())
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let refresh = MacKeychainStore::with_os_home(|_| Ok(home.clone()), true).unwrap();
        let logout = MacKeychainStore::with_os_home(|_| Ok(home.clone()), false).unwrap();
        assert!(refresh.background);
        assert!(!logout.background);
        assert_eq!(refresh.lock_dir, logout.lock_dir);
        let backend = Memory(Mutex::new(encode("work", &record()).unwrap()));
        let (paused_tx, paused_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        std::thread::scope(|scope| {
            let backend = &backend;
            let worker = scope.spawn(move || {
                let _guard = FileLock::acquire(&refresh.lock_dir, "work", LOCK_WAIT).unwrap();
                let mut record = load_with(backend, "work").unwrap().unwrap();
                paused_tx.send(()).unwrap();
                resume_rx.recv().unwrap(); // Simulate a paused network refresh.
                record.generation += 1;
                record.grant.as_mut().unwrap().refresh_token =
                    SecretString::new("ROTATED_CANARY").unwrap();
                save_with(backend, "work", &record).unwrap();
            });
            paused_rx.recv().unwrap();
            assert!(matches!(
                FileLock::acquire(&logout.lock_dir, "work", std::time::Duration::ZERO),
                Err(AuthError::Busy)
            ));
            resume_tx.send(()).unwrap();
            worker.join().unwrap();
        });
        let _guard = FileLock::acquire(&logout.lock_dir, "work", LOCK_WAIT).unwrap();
        let mut tombstone = load_with(&backend, "work").unwrap().unwrap();
        assert_eq!(tombstone.generation, 8);
        tombstone.generation += 1;
        tombstone.grant = None;
        save_with(&backend, "work", &tombstone).unwrap();
        let final_record = load_with(&backend, "work").unwrap().unwrap();
        assert!(final_record.grant.is_none());
        assert_eq!(final_record.generation, 9);
    }

    #[cfg(unix)]
    #[test]
    fn lock_modes_serialization_release_and_persistent_inode() {
        use std::{os::unix::fs::MetadataExt, time::Duration};
        let temp = tempfile::tempdir().unwrap();
        let store = MacKeychainStore::with_lock_dir(lock_path(&temp));
        let guard = FileLock::acquire(&store.lock_dir, "work", LOCK_WAIT).unwrap();
        assert_eq!(
            std::fs::metadata(&store.lock_dir).unwrap().mode() & 0o777,
            0o700
        );
        let entry = std::fs::read_dir(&store.lock_dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let before = std::fs::metadata(&entry).unwrap();
        assert_eq!(before.mode() & 0o777, 0o600);
        assert_eq!(before.uid(), unsafe { libc::geteuid() });
        assert!(
            !entry
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .contains("work")
        );
        assert!(matches!(
            FileLock::acquire(&store.lock_dir, "work", Duration::from_millis(30)),
            Err(AuthError::Busy)
        ));
        let other = FileLock::acquire(&store.lock_dir, "personal", Duration::ZERO).unwrap();
        drop(other);
        drop(guard);
        let _again = FileLock::acquire(&store.lock_dir, "work", Duration::ZERO).unwrap();
        assert_eq!(std::fs::metadata(&entry).unwrap().ino(), before.ino());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_unsafe_files_and_directories() {
        use std::{
            os::unix::fs::{PermissionsExt, symlink},
            time::Duration,
        };
        let temp = tempfile::tempdir().unwrap();
        let path = lock_path(&temp);
        drop(FileLock::acquire(&path, "work", Duration::ZERO).unwrap());
        let file = std::fs::read_dir(&path)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(FileLock::acquire(&path, "work", Duration::ZERO).is_err());
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = temp.path().join("hardlink");
        std::fs::hard_link(&file, &link).unwrap();
        assert!(FileLock::acquire(&path, "work", Duration::ZERO).is_err());
        std::fs::remove_file(&link).unwrap();
        std::fs::remove_file(&file).unwrap();
        symlink(&link, &file).unwrap();
        assert!(FileLock::acquire(&path, "work", Duration::ZERO).is_err());
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        assert!(FileLock::acquire(&path, "work", Duration::ZERO).is_err());
        std::fs::remove_dir(&file).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(FileLock::acquire(&path, "work", Duration::ZERO).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let alias = temp.path().join("alias");
        symlink(path.parent().unwrap(), &alias).unwrap();
        assert!(FileLock::acquire(&alias.join("google-auth"), "work", Duration::ZERO).is_err());
        assert!(
            FileLock::acquire(std::path::Path::new("relative"), "work", Duration::ZERO).is_err()
        );
    }

    // Run this same test binary as a child, never fork a multithreaded test runner.
    #[cfg(unix)]
    #[test]
    fn child_lock_probe() {
        let Some(path) = std::env::var_os("HIVE_STORAGE_TEST_LOCK_PATH") else {
            return;
        };
        let store = std::env::var_os("HIVE_STORAGE_TEST_OS_HOME").map(|home| {
            let background = std::env::var("HIVE_STORAGE_TEST_BACKGROUND").as_deref() == Ok("1");
            MacKeychainStore::with_os_home(|_| Ok(home.into()), background).unwrap()
        });
        let path = store
            .as_ref()
            .map(|store| store.lock_dir.as_os_str())
            .unwrap_or(&path);
        let expected_busy = std::env::var_os("HIVE_STORAGE_TEST_BUSY").is_some();
        let result = FileLock::acquire(
            std::path::Path::new(path),
            "work",
            std::time::Duration::ZERO,
        );
        if expected_busy {
            assert!(matches!(result, Err(AuthError::Busy)));
        } else {
            assert!(result.is_ok());
            if std::env::var_os("HIVE_STORAGE_TEST_HOLD").is_some() {
                use std::io::Write;
                println!("LOCK_READY");
                std::io::stdout().flush().unwrap();
                std::thread::sleep(std::time::Duration::from_secs(30));
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn separate_process_serialization() {
        let temp = tempfile::tempdir().unwrap();
        let path = lock_path(&temp);
        let guard = FileLock::acquire(&path, "work", LOCK_WAIT).unwrap();
        let run = |busy: bool| {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args(["--exact", "storage::tests::child_lock_probe"])
                .env("HIVE_STORAGE_TEST_LOCK_PATH", &path)
                .env_remove("HIVE_STORAGE_TEST_OS_HOME")
                .env_remove("HIVE_STORAGE_TEST_HOLD")
                .env_remove("HIVE_STORAGE_TEST_BUSY");
            if busy {
                command.env("HIVE_STORAGE_TEST_BUSY", "1");
            }
            assert!(command.status().unwrap().success());
        };
        run(true);
        drop(guard);
        run(false);
    }

    #[cfg(unix)]
    #[test]
    fn process_death_releases_lock_without_removing_file() {
        use std::io::{BufRead, BufReader};
        let temp = tempfile::tempdir().unwrap();
        let path = lock_path(&temp);
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "storage::tests::child_lock_probe", "--nocapture"])
            .env("HIVE_STORAGE_TEST_LOCK_PATH", &path)
            .env_remove("HIVE_STORAGE_TEST_OS_HOME")
            .env("HIVE_STORAGE_TEST_HOLD", "1")
            .env_remove("HIVE_STORAGE_TEST_BUSY")
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let mut ready = false;
        let mut line = String::new();
        while output.read_line(&mut line).unwrap() != 0 {
            if line.contains("LOCK_READY") {
                ready = true;
                break;
            }
            line.clear();
        }
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(ready);
        let _guard = FileLock::acquire(&path, "work", std::time::Duration::ZERO).unwrap();
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 1);
    }
}

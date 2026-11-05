//! Immutable, process-local TasksWrite credentials supplied as existing V1 record bytes.
//! No environment, file, native storage, or cross-workflow locking is performed here.
use crate::{
    AuthError, AuthProfile, CredentialGuard, CredentialStore, Result, StoredConnection,
    storage::decode_for, validate_connection,
};
use std::sync::{Mutex, MutexGuard, TryLockError};

/// One live grant bound to an independently pinned account namespace and alias.
/// Callers must hold `lock` throughout authentication. The lock coordinates only
/// users of this store instance, not separate instances, processes, or workflows.
pub struct ImmutableTasksWriteStore {
    record: StoredConnection,
    lock: Mutex<()>,
}

impl ImmutableTasksWriteStore {
    /// Decode the bounded, strict native V1 format without exporting or persisting it.
    /// The expected namespace must be independently trusted, not taken from the record.
    pub fn from_record_bytes(
        expected_connection: &str,
        expected_account_namespace: &str,
        bytes: &[u8],
    ) -> Result<Self> {
        let record = decode_for(AuthProfile::TasksWrite, expected_connection, bytes)?;
        if record.grant.is_none() {
            return Err(AuthError::NotConnected);
        }
        // Comparing with the validated identity's canonical namespace also rejects
        // raw subjects and noncanonical pins (including uppercase hex).
        if record
            .identity
            .namespace()
            .map_err(|_| AuthError::StorageCorrupt)?
            != expected_account_namespace
        {
            return Err(AuthError::IdentityMismatch);
        }
        Ok(Self {
            record,
            lock: Mutex::new(()),
        })
    }

    fn check_connection(&self, connection: &str) -> Result<()> {
        validate_connection(connection)?;
        if connection != self.record.connection {
            return Err(AuthError::IdentityMismatch);
        }
        Ok(())
    }
}

impl std::fmt::Debug for ImmutableTasksWriteStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImmutableTasksWriteStore")
            .finish_non_exhaustive()
    }
}

struct Guard<'a> {
    _guard: MutexGuard<'a, ()>,
}
impl CredentialGuard for Guard<'_> {}

impl CredentialStore for ImmutableTasksWriteStore {
    fn profile(&self) -> AuthProfile {
        AuthProfile::TasksWrite
    }

    fn lock(&self, connection: &str) -> Result<Box<dyn CredentialGuard + '_>> {
        self.check_connection(connection)?;
        let guard = self.lock.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => AuthError::Busy,
            TryLockError::Poisoned(_) => AuthError::StorageUnavailable,
        })?;
        Ok(Box::new(Guard { _guard: guard }))
    }

    fn load(&self, connection: &str) -> Result<Option<StoredConnection>> {
        self.check_connection(connection)?;
        Ok(Some(self.record.clone()))
    }

    fn save(&self, _: &str, _: &StoredConnection) -> Result<()> {
        Err(AuthError::CredentialPersistenceRequired)
    }
}

#[cfg(test)]
mod tests;

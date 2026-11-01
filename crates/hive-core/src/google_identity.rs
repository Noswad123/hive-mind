//! Provider identity is not a user-selected connection alias or a credential.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const GOOGLE_ISSUER: &str = "https://accounts.google.com";
pub const VERIFIED_NAMESPACE_PREFIX: &str = "google-sub-v1:";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoogleIdentity {
    issuer: String,
    subject: String,
}

impl GoogleIdentity {
    pub fn new(subject: impl Into<String>) -> Result<Self> {
        let identity = Self {
            issuer: GOOGLE_ISSUER.into(),
            subject: subject.into(),
        };
        identity.validate()?;
        Ok(identity)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.issuer == GOOGLE_ISSUER,
            "unsupported Google identity issuer"
        );
        ensure!(
            !self.subject.trim().is_empty()
                && self.subject.len() <= 255
                && self.subject.is_ascii()
                && !self.subject.chars().any(char::is_control),
            "invalid Google subject identity"
        );
        Ok(())
    }

    pub fn issuer(&self) -> &str {
        &self.issuer
    }
    pub fn subject(&self) -> &str {
        &self.subject
    }

    pub fn namespace(&self) -> Result<String> {
        self.validate()?;
        let mut hash = Sha256::new();
        hash.update(b"hive/google-identity/v1\0");
        for value in [self.issuer.as_str(), self.subject.as_str()] {
            hash.update((value.len() as u64).to_be_bytes());
            hash.update(value.as_bytes());
        }
        Ok(format!("{VERIFIED_NAMESPACE_PREFIX}{:x}", hash.finalize()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_is_subject_based_and_strictly_validated() {
        let first = GoogleIdentity::new("one").unwrap();
        assert_eq!(
            first.namespace().unwrap(),
            first.clone().namespace().unwrap()
        );
        assert_ne!(
            first.namespace().unwrap(),
            GoogleIdentity::new("two").unwrap().namespace().unwrap()
        );
        assert!(
            first
                .namespace()
                .unwrap()
                .starts_with(VERIFIED_NAMESPACE_PREFIX)
        );
        for subject in ["", " ", "one\n", "\0", "非ASCII"] {
            assert!(GoogleIdentity::new(subject).is_err());
        }
        let fake: GoogleIdentity =
            serde_json::from_str(r#"{"issuer":"https://evil.invalid","subject":"one"}"#).unwrap();
        assert!(fake.namespace().is_err());
    }
}

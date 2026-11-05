use super::*;
use reqwest::{
    Url,
    blocking::Client,
    header::{AUTHORIZATION, HeaderValue},
};

// Neither JSON nor callers can construct an observation. Its origin is fixed by
// the transport constructor, never inferred from response claims.
pub(super) struct CurrentMain {
    repository_id: String,
    repository: String,
    commit: String,
    github: bool,
}
impl CurrentMain {
    pub(super) fn summary(&self) -> Value {
        let verified = self.github
            && numeric_id(&self.repository_id)
            && repository(&self.repository)
            && oid(&self.commit);
        json!({"schemaVersion":"1", "kind":"google-tasks-hosted-main-summary",
            "redacted":true, "pushClaimsAdmitted":true,
            "currentMainVerifiedDuringCollection":verified,
            "currentMainVerified":verified, "githubOrigin":self.github,
            "githubVerified":verified, "googleOrigin":false,
            "pushVerified":false, "githubPushAuthenticated":false,
            "publicationReady":false, "hostedExecutionEnabled":false,
            "dispatchAllowed":false, "portableEvidenceAuthenticated":false})
    }
    #[cfg(test)]
    pub(super) fn matches(&self, binding: &Binding, commit: &str) -> bool {
        self.repository_id == binding.expected_repository_id
            && self.repository == binding.expected_repository
            && self.commit == commit
    }
}

pub(super) struct GitHub {
    client: Client,
    base: Url,
    github: bool,
}
impl GitHub {
    pub(super) fn production() -> Result<Self> {
        Self::build(Url::parse("https://api.github.com/")?, true)
    }
    fn build(base: Url, github: bool) -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .https_only(github)
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(10))
                .build()?,
            base,
            github,
        })
    }
    #[cfg(test)]
    pub(super) fn loopback(base: &str) -> Result<Self> {
        let url = Url::parse(base)?;
        ensure!(
            url.scheme() == "http"
                && url.host_str() == Some("127.0.0.1")
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "invalid_test_origin"
        );
        Self::build(url, false)
    }
    fn get(&self, path: &str, token: &str, deadline: Instant) -> Result<Value> {
        ensure!(
            !token.is_empty() && token.len() <= 8192 && token.bytes().all(|b| b.is_ascii_graphic()),
            "invalid_token"
        );
        let mut bearer = HeaderValue::from_str(&format!("Bearer {token}"))?;
        bearer.set_sensitive(true);
        let response = self
            .client
            .get(self.base.join(path)?)
            .header(AUTHORIZATION, bearer)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "hive-tasks/1")
            .timeout(remaining(deadline)?.min(Duration::from_secs(10)))
            .send()
            .map_err(|_| anyhow!("github_transport_failed"))?;
        ensure!(response.status().as_u16() == 200, "github_status_failed");
        let mut bytes = Vec::new();
        response
            .take(64 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| anyhow!("github_body_failed"))?;
        ensure!(bytes.len() <= 64 * 1024, "github_body_limit");
        unique(&bytes).map_err(|_| anyhow!("github_json_failed"))
    }
    pub(super) fn verify(
        &self,
        b: &Binding,
        commit: &str,
        token: &str,
        deadline: Instant,
    ) -> Result<CurrentMain> {
        ensure!(
            repository(&b.expected_repository)
                && numeric_id(&b.expected_repository_id)
                && oid(commit),
            "invalid_github_binding"
        );
        let path = format!("repos/{}", b.expected_repository);
        let repo = self.get(&path, token, deadline)?;
        ensure!(
            repo["id"]
                .as_u64()
                .is_some_and(|id| id.to_string() == b.expected_repository_id)
                && repo["full_name"] == b.expected_repository
                && repo["private"] == true,
            "github_identity_mismatch"
        );
        let main = self.get(&format!("{path}/git/ref/heads/main"), token, deadline)?;
        ensure!(
            main["ref"] == "refs/heads/main"
                && main["object"]["type"] == "commit"
                && main["object"]["sha"] == commit,
            "github_main_mismatch"
        );
        remaining(deadline)?;
        Ok(CurrentMain {
            repository_id: b.expected_repository_id.clone(),
            repository: b.expected_repository.clone(),
            commit: commit.into(),
            github: self.github,
        })
    }
}

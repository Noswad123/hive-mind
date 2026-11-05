use anyhow::{Context, Result, ensure};
use hive_capture::{GoogleTask, handoff::validate_open_task};
use reqwest::{
    Method, Url,
    blocking::Client,
    header::{AUTHORIZATION, ETAG, HeaderValue, IF_MATCH},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{io::Read, time::Duration};

pub(crate) const RESOURCE_LIMIT: usize = 64 * 1024;

pub(crate) fn strong_etag(s: &str) -> bool {
    (3..=1024).contains(&s.len())
        && s.starts_with('"')
        && s.ends_with('"')
        && s.as_bytes()[1..s.len() - 1]
            .iter()
            .all(|b| matches!(b, 0x21 | 0x23..=0x7e) && *b != b',')
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Resource {
    pub raw: Value,
    pub etag: String,
}
impl Resource {
    pub fn validate(&self, id: &str) -> Result<GoogleTask> {
        ensure!(
            serde_json::to_vec(&self.raw)?.len() <= RESOURCE_LIMIT,
            "task resource limit"
        );
        for field in ["id", "title", "status"] {
            ensure!(
                self.raw.get(field).is_some_and(Value::is_string),
                "mandatory task string missing"
            );
        }
        let task: GoogleTask =
            serde_json::from_value(self.raw.clone()).context("unsupported task shape")?;
        validate_open_task(&task)?;
        ensure!(
            task.id == id && !task.title.trim().is_empty(),
            "task identity/title mismatch"
        );
        ensure!(
            strong_etag(&self.etag) && task.etag.as_deref() == Some(&self.etag),
            "strong matching task ETag required"
        );
        if let Some(updated) = &task.updated {
            ensure!(
                updated.len() <= 64
                    && time::OffsetDateTime::parse(
                        updated,
                        &time::format_description::well_known::Rfc3339
                    )
                    .is_ok(),
                "invalid updated metadata"
            );
        }
        Ok(task)
    }
    pub fn nondue(&self) -> Value {
        let mut value = self.raw.clone();
        if let Some(map) = value.as_object_mut() {
            for key in ["due", "etag"] {
                map.remove(key);
            }
            // Only a validated timestamp is volatile. Preserve raw null/presence
            // differences rather than silently treating them as updated metadata.
            if map.get("updated").is_some_and(Value::is_string) {
                map.remove("updated");
            }
        }
        value
    }
    pub fn satisfied(&self, baseline: &Self, id: &str, due: &str) -> Result<()> {
        let task = self.validate(id)?;
        ensure!(
            task.due
                .as_deref()
                .is_some_and(|d| crate::policy::due_matches(d, due))
                && self.nondue() == baseline.nondue(),
            "mutation semantic drift"
        );
        Ok(())
    }
    pub fn mutation(self, baseline: &Self, id: &str, due: &str) -> Result<Self> {
        self.satisfied(baseline, id, due)?;
        ensure!(self.etag != baseline.etag, "unchanged mutation ETag");
        Ok(self)
    }
}

pub(crate) struct Response {
    pub status: u16,
    etag: Option<String>,
    bytes: Vec<u8>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ResponseEvidence {
    pub status: u16,
    pub body_sha256: String,
    pub body_bytes: usize,
}
impl Response {
    pub fn evidence(&self) -> ResponseEvidence {
        ResponseEvidence {
            status: self.status,
            body_sha256: format!("{:x}", Sha256::digest(&self.bytes)),
            body_bytes: self.bytes.len(),
        }
    }
    pub fn resource(self, id: &str) -> Result<Resource> {
        ensure!(self.status == 200, "exact task request did not return 200");
        let raw: Value = serde_json::from_slice(&self.bytes).context("malformed task response")?;
        let etag = raw
            .get("etag")
            .and_then(Value::as_str)
            .context("missing task ETag")?
            .to_owned();
        ensure!(
            self.etag.as_ref().is_none_or(|h| h == &etag),
            "HTTP/resource ETag mismatch"
        );
        let resource = Resource { raw, etag };
        resource.validate(id)?;
        Ok(resource)
    }
    pub fn conflict(&self) -> bool {
        self.status == 412
            && serde_json::from_slice::<Value>(&self.bytes)
                .ok()
                .is_some_and(|v| v["error"]["code"] == 412)
    }
}

pub(crate) struct Http {
    client: Client,
    base: Url,
    pub google: bool,
    timeout: Duration,
}
impl Http {
    pub fn google() -> Result<Self> {
        Self::build(
            Url::parse("https://tasks.googleapis.com/tasks/v1/lists/")?,
            true,
        )
    }
    fn build(base: Url, google: bool) -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .https_only(google)
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .timeout(Duration::from_secs(10))
                .connect_timeout(Duration::from_secs(10))
                .build()?,
            base,
            google,
            timeout: Duration::from_secs(10),
        })
    }
    #[cfg(test)]
    pub fn loopback(base: &str) -> Self {
        let url = Url::parse(base).unwrap();
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        Self::build(url, false).unwrap()
    }
    #[cfg(test)]
    pub fn loopback_timeout(base: &str, timeout: Duration) -> Self {
        let mut http = Self::loopback(base);
        http.timeout = timeout;
        http
    }
    // Crate-private transport only; the sole caller of PATCH consumes a post-CAS permit.
    pub fn request(
        &self,
        token: &str,
        list: &str,
        task: &str,
        patch: Option<(&str, &str)>,
        remaining: Duration,
    ) -> Result<Response> {
        ensure!(!remaining.is_zero(), "network budget expired");
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("invalid private base"))?
            .pop_if_empty()
            .extend([list, "tasks", task]);
        let mut bearer = HeaderValue::from_str(&format!("Bearer {token}"))?;
        bearer.set_sensitive(true);
        let mut request = self
            .client
            .request(
                if patch.is_some() {
                    Method::PATCH
                } else {
                    Method::GET
                },
                url,
            )
            .header(AUTHORIZATION, bearer);
        if let Some((etag, due)) = patch {
            ensure!(strong_etag(etag), "strong If-Match required");
            request = request.header(IF_MATCH, etag).json(&json!({"due":due}));
        }
        let response = request
            .timeout(remaining.min(self.timeout))
            .send()
            .map_err(|_| {
                anyhow::anyhow!("Tasks transport unavailable; outcome unknown; no retry")
            })?;
        let status = response.status().as_u16();
        let tags: Vec<_> = response.headers().get_all(ETAG).iter().collect();
        ensure!(tags.len() <= 1, "multiple HTTP ETags");
        let etag = tags
            .first()
            .map(|h| h.to_str().map(str::to_owned))
            .transpose()?;
        let mut bytes = Vec::new();
        response
            .take((RESOURCE_LIMIT + 1) as u64)
            .read_to_end(&mut bytes)
            .context("Tasks body unavailable")?;
        ensure!(bytes.len() <= RESOURCE_LIMIT, "task resource limit");
        Ok(Response {
            status,
            etag,
            bytes,
        })
    }
}

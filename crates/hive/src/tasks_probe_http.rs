use super::journal::RECORD_LIMIT;
use anyhow::{Result, anyhow, ensure};
use reqwest::{
    Method, Url,
    blocking::Client,
    header::{AUTHORIZATION, ETAG, HeaderValue, IF_MATCH},
};
use serde_json::{Value, json};
use std::{
    io::Read,
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
pub(super) enum Origin {
    Google,
    #[cfg(test)]
    Loopback,
}

impl Origin {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Google => "Google",
            #[cfg(test)]
            Self::Loopback => "loopback test only; no Google validation",
        }
    }
    pub(super) fn google(self) -> bool {
        matches!(self, Self::Google)
    }
}

pub(super) struct Http {
    client: Client,
    base: Url,
    pub(super) origin: Origin,
    timeout: Duration,
}

pub(super) struct Response {
    pub(super) status: u16,
    pub(super) etag: Option<String>,
    pub(super) bytes: Vec<u8>,
    problem: Option<&'static str>,
}

pub(super) fn strong_etag(value: &str) -> bool {
    value.len() >= 3
        && value.len() <= 1024
        && value.starts_with('"')
        && value.ends_with('"')
        && value.as_bytes()[1..value.len() - 1]
            .iter()
            .all(|b| matches!(b, 0x21 | 0x23..=0x7e) && *b != b',')
}

impl Http {
    pub(super) fn google() -> Result<Self> {
        Self::build(
            Url::parse("https://tasks.googleapis.com/tasks/v1/lists/").unwrap(),
            Origin::Google,
            Duration::from_secs(10),
        )
    }

    fn build(base: Url, origin: Origin, timeout: Duration) -> Result<Self> {
        let client = Client::builder()
            .https_only(origin.google())
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(timeout)
            .connect_timeout(timeout)
            .build()
            .map_err(|_| anyhow!("inconclusive: HTTP setup unavailable"))?;
        Ok(Self {
            client,
            base,
            origin,
            timeout,
        })
    }

    #[cfg(test)]
    pub(super) fn loopback(base: &str, timeout: Duration) -> Self {
        let url = Url::parse(base).unwrap();
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        Self::build(url, Origin::Loopback, timeout).unwrap()
    }

    pub(super) fn request(
        &self,
        token: &str,
        list: &str,
        task: &str,
        patch: Option<(&str, &str)>,
        remaining: Duration,
    ) -> Result<Response> {
        let deadline = Instant::now() + remaining;
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|_| anyhow!("invalid private base"))?
            .pop_if_empty()
            .extend([list, "tasks", task]);
        let mut bearer = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| anyhow!("inconclusive: invalid bearer"))?;
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
            ensure!(strong_etag(etag), "unsupported: strong ETag required");
            request = request
                .header(IF_MATCH, etag)
                .json(&json!({"due":format!("{due}T00:00:00.000Z")}));
        }
        let timeout = deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| {
                anyhow!("inconclusive: admission/network budget expired before Tasks request")
            })?
            .min(self.timeout)
            .min(Duration::from_secs(10));
        let response = request.timeout(timeout).send().map_err(|_| {
            anyhow!("inconclusive: Tasks transport unavailable; outcome unknown; no retry")
        })?;
        let status = response.status().as_u16();
        let etags: Vec<_> = response.headers().get_all(ETAG).iter().collect();
        let mut problem = (etags.len() > 1).then_some("unsupported: multiple response ETags");
        let etag = etags.first().and_then(|v| match v.to_str() {
            Ok(s) => Some(s.to_owned()),
            Err(_) => {
                problem = Some("unsupported: invalid response ETag");
                None
            }
        });
        let mut bytes = Vec::new();
        if response
            .take((RECORD_LIMIT + 1) as u64)
            .read_to_end(&mut bytes)
            .is_err()
        {
            problem = Some("inconclusive: Tasks body unavailable; no retry");
        }
        if bytes.len() > RECORD_LIMIT {
            problem = Some("unsupported: Tasks body size limit");
        }
        Ok(Response {
            status,
            etag,
            bytes,
            problem,
        })
    }
}

impl Response {
    pub(super) fn validate_envelope(&self) -> Result<()> {
        if let Some(problem) = self.problem {
            anyhow::bail!(problem);
        }
        Ok(())
    }
    pub(super) fn json(&self) -> Result<Value> {
        self.validate_envelope()?;
        serde_json::from_slice(&self.bytes)
            .map_err(|_| anyhow!("unsupported: malformed task response"))
    }
}

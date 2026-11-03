//! Dormant HTTP boundary. No credential provider, dispatch authority, or durable ACK
//! consumer is wired here. A future provider must obtain Calendar-write AND OpenID
//! grants; a Tasks read-only grant and a claimed token identity are not authority.
use std::{
    collections::BTreeMap,
    fmt,
    io::Read,
    time::{Duration, Instant},
};

use reqwest::{Method, Url, blocking::Client};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::{
    Binding, CalendarDate, Ledger, OperationKind, OperationPhase, ProspectiveEvent, ProviderAck,
    ledger, operations,
};

pub(crate) mod ack;
mod coordinator;
mod dispatch;

const USERINFO: &str = "https://openidconnect.googleapis.com/v1/userinfo";
const CALENDAR: &str = "https://www.googleapis.com/calendar/v3/";
const USERINFO_LIMIT: usize = 64 * 1024;
const EVENT_LIMIT: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransportError {
    Authorization,
    PrincipalMismatch,
    Missing,
    Collision,
    Conflict,
    Unavailable,
    Invalid,
    Uncertain,
}
type Result<T> = std::result::Result<T, TransportError>;

struct SecretToken(Zeroizing<String>);

impl SecretToken {
    fn new(value: String) -> Result<Self> {
        let value = Zeroizing::new(value);
        // RFC 6750 b64token, not a JWT parser or an identity/scope assertion.
        let unpadded = value.trim_end_matches('=');
        if value.is_empty()
            || value.len() > 8192
            || unpadded.is_empty()
            || !unpadded
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._~+/".contains(&b))
        {
            return Err(TransportError::Invalid);
        }
        Ok(Self(value))
    }
}

impl fmt::Debug for SecretToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretToken([REDACTED])")
    }
}

// Neither this metadata nor a local Git CAS proves dispatch exclusivity or history.
struct PreparedFrozenIntent {
    context_digest: Option<String>,
    ledger_basis: Option<(String, String, String)>,
    binding: Binding,
    logical_id: String,
    operation_id: String,
    digest: String,
    kind: OperationKind,
    phase: OperationPhase,
    event_id: String,
    event: ProspectiveEvent,
    desired_hash: String,
    prior: Option<ProviderAck>,
}

impl PreparedFrozenIntent {
    fn prepare(ledger: &Ledger, binding: &Binding, logical: &str, operation: &str) -> Result<Self> {
        let bytes = serde_json::to_vec(ledger).map_err(|_| TransportError::Invalid)?;
        let strict = ledger::decode(&bytes, binding).map_err(|_| TransportError::Invalid)?;
        if matches!(binding.calendar_id.as_str(), "." | "..") {
            return Err(TransportError::Invalid);
        }
        let entry = strict
            .entries
            .iter()
            .find(|e| e.logical_id == logical)
            .ok_or(TransportError::Invalid)?;
        let pending = entry.pending.as_ref().ok_or(TransportError::Invalid)?;
        if pending.operation_id != operation || pending.kind == OperationKind::Delete {
            return Err(TransportError::Invalid);
        }
        let event = operations::frozen_event(pending).map_err(|_| TransportError::Invalid)?;
        if pending.kind == OperationKind::Create
            && pending.target_event_id
                != operations::reserved_event_id(binding, logical)
                    .map_err(|_| TransportError::Invalid)?
        {
            return Err(TransportError::Invalid);
        }
        if let Some(prior) = &entry.provider_ack {
            strong_etag(&prior.etag)?;
        }
        Ok(Self {
            context_digest: None,
            ledger_basis: None,
            binding: binding.clone(),
            logical_id: logical.into(),
            operation_id: operation.into(),
            digest: operations::intent_digest(binding, logical, pending)
                .map_err(|_| TransportError::Invalid)?,
            kind: pending.kind,
            phase: pending.phase,
            event_id: pending.target_event_id.clone(),
            desired_hash: ledger::payload_hash(&event),
            event,
            prior: entry.provider_ack.clone(),
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ImmutableEndpointOrigin {
    Google,
    #[cfg(test)]
    Loopback(u16),
}

// Move-only capability. Intentionally NO production constructor. Only a future
// exclusive worker may mint one, separately from Prepared and local ledger CAS.
struct DispatchPermit {
    context_digest: Option<String>,
    ledger_head: Option<String>,
    owner_nonce: Option<String>,
    authority: Option<String>,
    binding: Binding,
    logical_id: String,
    operation_id: String,
    digest: String,
    kind: OperationKind,
    origin: ImmutableEndpointOrigin,
}

impl DispatchPermit {
    #[cfg(test)]
    fn test_permit(intent: &PreparedFrozenIntent, transport: &HttpTransport) -> Self {
        assert!(
            matches!(transport.origin, ImmutableEndpointOrigin::Loopback(_)),
            "test permits are loopback-only"
        );
        Self {
            context_digest: None,
            ledger_head: None,
            owner_nonce: None,
            authority: None,
            binding: intent.binding.clone(),
            logical_id: intent.logical_id.clone(),
            operation_id: intent.operation_id.clone(),
            digest: intent.digest.clone(),
            kind: intent.kind,
            origin: transport.origin,
        }
    }

    fn validate(
        self,
        intent: &PreparedFrozenIntent,
        transport: &HttpTransport,
        kind: OperationKind,
    ) -> Result<()> {
        if self.binding != intent.binding
            || self.logical_id != intent.logical_id
            || self.operation_id != intent.operation_id
            || self.digest != intent.digest
            || self.kind != kind
            || intent.kind != kind
            || self.origin != transport.origin
            || intent.phase != OperationPhase::Prepared
            || self.context_digest != intent.context_digest
            || (self.origin == ImmutableEndpointOrigin::Google
                && (self.context_digest.as_deref().is_none_or(str::is_empty)
                    || intent
                        .ledger_basis
                        .as_ref()
                        .is_none_or(|(head, _, _)| self.ledger_head.as_ref() != Some(head))
                    || self.ledger_head.as_deref().is_none_or(str::is_empty)
                    || self.owner_nonce.as_deref().is_none_or(str::is_empty)
                    || self.authority.as_deref().is_none_or(str::is_empty)))
        {
            return Err(TransportError::Invalid);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SuccessCause {
    MutationResponse,
    ReconciledObservation,
}

// Sealed response evidence, deliberately not serializable or convertible to the
// operator-observation format. GET observation is not proof of dispatch.
struct ValidatedSuccess {
    context_digest: Option<String>,
    binding: Binding,
    logical_id: String,
    operation_id: String,
    intent_digest: String,
    event_id: String,
    etag: String,
    payload_hash: String,
    cause: SuccessCause,
    origin: ImmutableEndpointOrigin,
}

impl ValidatedSuccess {
    // This is only an origin eligibility check, NEVER permission to persist an ACK.
    fn production_origin_eligible(&self) -> bool {
        self.origin == ImmutableEndpointOrigin::Google
    }
}

struct HttpTransport {
    client: Client,
    userinfo: Url,
    calendar: Url,
    origin: ImmutableEndpointOrigin,
    operation_timeout: Duration,
}

impl HttpTransport {
    fn production() -> Result<Self> {
        Ok(Self {
            client: Self::client(true)?,
            userinfo: Url::parse(USERINFO).map_err(|_| TransportError::Invalid)?,
            calendar: Url::parse(CALENDAR).map_err(|_| TransportError::Invalid)?,
            origin: ImmutableEndpointOrigin::Google,
            operation_timeout: Duration::from_secs(30),
        })
    }

    fn client(https_only: bool) -> Result<Client> {
        Client::builder()
            .https_only(https_only)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| TransportError::Unavailable)
    }

    #[cfg(test)]
    fn loopback(base: &str, timeout: Duration) -> Result<Self> {
        let literal = base;
        let base = Url::parse(base).map_err(|_| TransportError::Invalid)?;
        if base.scheme() != "http"
            || base.host_str() != Some("127.0.0.1")
            || base.port().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || base.path() != "/"
            || base.port() == Some(0)
            || literal != format!("http://127.0.0.1:{}/", base.port().unwrap_or(0))
            || timeout.is_zero()
            || timeout > Duration::from_secs(30)
        {
            return Err(TransportError::Invalid);
        }
        // Share only the test runtime/client, never endpoint or operation state.
        // No default credentials or cookie store; fixture replies close connections.
        static CLIENT: std::sync::OnceLock<Result<Client>> = std::sync::OnceLock::new();
        let client = CLIENT.get_or_init(|| Self::client(false)).clone()?;
        Ok(Self {
            client,
            userinfo: base.join("userinfo").unwrap(),
            calendar: base.join("calendar/v3/").unwrap(),
            origin: ImmutableEndpointOrigin::Loopback(base.port().unwrap()),
            operation_timeout: timeout,
        })
    }

    fn event_url(&self, intent: &PreparedFrozenIntent, collection: bool) -> Result<Url> {
        let mut url = self.calendar.clone();
        url.path_segments_mut()
            .map_err(|_| TransportError::Invalid)?
            .pop_if_empty()
            .extend(["calendars", intent.binding.calendar_id.as_str(), "events"]);
        if !collection {
            url.path_segments_mut()
                .map_err(|_| TransportError::Invalid)?
                .push(&intent.event_id);
        }
        Ok(url)
    }

    fn principal(
        &self,
        token: &SecretToken,
        intent: &PreparedFrozenIntent,
        deadline: Instant,
    ) -> Result<()> {
        #[derive(Deserialize)]
        struct UserInfo {
            sub: String,
        }
        let bytes = self.request(
            Method::GET,
            self.userinfo.clone(),
            token,
            None,
            None,
            USERINFO_LIMIT,
            deadline,
            false,
        )?;
        let info: UserInfo =
            ledger::decode_unique_json(&bytes).map_err(|_| TransportError::Invalid)?;
        let namespace = hive_core::GoogleIdentity::new(info.sub)
            .and_then(|i| i.namespace())
            .map_err(|_| TransportError::Invalid)?;
        if namespace != intent.binding.account_id {
            return Err(TransportError::PrincipalMismatch);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn request(
        &self,
        method: Method,
        url: Url,
        token: &SecretToken,
        body: Option<&Value>,
        etag: Option<&str>,
        limit: usize,
        deadline: Instant,
        mutation: bool,
    ) -> Result<Vec<u8>> {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or(TransportError::Unavailable)?;
        let mut request = self
            .client
            .request(method, url)
            .bearer_auth(token.0.as_str())
            .timeout(remaining);
        if let Some(body) = body {
            request = request.json(body);
        }
        if let Some(etag) = etag {
            request = request.header(reqwest::header::IF_MATCH, etag);
        }
        let io_error = if mutation {
            TransportError::Uncertain
        } else {
            TransportError::Unavailable
        };
        // From send onward, transport failure cannot prove a mutation was not applied.
        let mut response = request.send().map_err(|_| io_error)?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(match status {
                401 | 403 => TransportError::Authorization,
                404 | 410 => TransportError::Missing,
                409 => TransportError::Collision,
                412 => TransportError::Conflict,
                // A server/gateway failure does not prove a write was not applied.
                500..=599 if mutation => TransportError::Uncertain,
                429 | 500..=599 => TransportError::Unavailable,
                _ => TransportError::Invalid,
            });
        }
        let malformed = if mutation {
            TransportError::Uncertain
        } else {
            TransportError::Invalid
        };
        if response.content_length().is_some_and(|n| n > limit as u64) {
            return Err(malformed);
        }
        let mut bytes = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            if Instant::now() >= deadline {
                return Err(io_error);
            }
            let count = response.read(&mut buffer).map_err(|_| io_error)?;
            if count == 0 {
                break;
            }
            if bytes.len() + count > limit {
                return Err(malformed);
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
        if Instant::now() >= deadline {
            return Err(io_error);
        }
        Ok(bytes)
    }

    fn reconcile(
        &self,
        token: &SecretToken,
        intent: &PreparedFrozenIntent,
    ) -> Result<ValidatedSuccess> {
        let deadline = Instant::now() + self.operation_timeout;
        self.principal(token, intent, deadline)?;
        let bytes = self.request(
            Method::GET,
            self.event_url(intent, false)?,
            token,
            None,
            None,
            EVENT_LIMIT,
            deadline,
            false,
        )?;
        self.success(
            intent,
            &bytes,
            SuccessCause::ReconciledObservation,
            deadline,
        )
    }

    fn create(
        &self,
        token: &SecretToken,
        intent: &PreparedFrozenIntent,
        permit: DispatchPermit,
    ) -> Result<ValidatedSuccess> {
        permit.validate(intent, self, OperationKind::Create)?;
        let deadline = Instant::now() + self.operation_timeout;
        self.principal(token, intent, deadline)?;
        let mut body = managed_body(intent, ownership(intent));
        body["id"] = json!(intent.event_id);
        let bytes = self.request(
            Method::POST,
            self.event_url(intent, true)?,
            token,
            Some(&body),
            None,
            EVENT_LIMIT,
            deadline,
            true,
        )?;
        self.success(intent, &bytes, SuccessCause::MutationResponse, deadline)
            .map_err(|_| TransportError::Uncertain)
    }

    fn update(
        &self,
        token: &SecretToken,
        intent: &PreparedFrozenIntent,
        permit: DispatchPermit,
    ) -> Result<ValidatedSuccess> {
        permit.validate(intent, self, OperationKind::Update)?;
        let prior = intent.prior.as_ref().ok_or(TransportError::Invalid)?;
        strong_etag(&prior.etag)?;
        let deadline = Instant::now() + self.operation_timeout;
        self.principal(token, intent, deadline)?;
        let url = self.event_url(intent, false)?;
        let bytes = self.request(
            Method::GET,
            url.clone(),
            token,
            None,
            None,
            EVENT_LIMIT,
            deadline,
            false,
        )?;
        let current = validate_event(intent, &bytes, &prior.last_payload_hash)?;
        if current.etag != prior.etag {
            return Err(TransportError::Conflict);
        }
        // PATCH only the managed subset. Preserve every unrelated private tag;
        // shared properties and all other provider fields are omitted entirely.
        let body = managed_body(intent, current.private);
        let bytes = self.request(
            Method::PATCH,
            url,
            token,
            Some(&body),
            Some(&prior.etag),
            EVENT_LIMIT,
            deadline,
            true,
        )?;
        self.success(intent, &bytes, SuccessCause::MutationResponse, deadline)
            .map_err(|_| TransportError::Uncertain)
    }

    fn success(
        &self,
        intent: &PreparedFrozenIntent,
        bytes: &[u8],
        cause: SuccessCause,
        deadline: Instant,
    ) -> Result<ValidatedSuccess> {
        let event = validate_event(intent, bytes, &intent.desired_hash)?;
        if Instant::now() >= deadline {
            return Err(TransportError::Unavailable);
        }
        Ok(ValidatedSuccess {
            context_digest: intent.context_digest.clone(),
            binding: intent.binding.clone(),
            logical_id: intent.logical_id.clone(),
            operation_id: intent.operation_id.clone(),
            intent_digest: intent.digest.clone(),
            event_id: intent.event_id.clone(),
            etag: event.etag,
            payload_hash: intent.desired_hash.clone(),
            cause,
            origin: self.origin,
        })
    }
}

fn strong_etag(value: &str) -> Result<()> {
    if !(3..=1024).contains(&value.len())
        || !value.starts_with('"')
        || !value.ends_with('"')
        || !value[1..value.len() - 1]
            .bytes()
            .all(|b| (0x21..=0x7e).contains(&b) && b != b'"')
    {
        return Err(TransportError::Invalid);
    }
    Ok(())
}

fn ownership(intent: &PreparedFrozenIntent) -> BTreeMap<String, String> {
    // A collision-resistant convention, not cryptographic proof of remote ownership.
    let mut hash = Sha256::new();
    hash.update(b"hive-calendar-owner:v1\0");
    for value in [
        &intent.binding.provider,
        &intent.binding.account_id,
        &intent.binding.calendar_id,
        &intent.binding.source_repo_id,
    ] {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value.as_bytes());
    }
    BTreeMap::from([
        ("hiveSchema".into(), "1".into()),
        ("hiveOwner".into(), format!("{:x}", hash.finalize())),
        ("hiveLogicalId".into(), intent.logical_id.clone()),
    ])
}

fn managed_body(intent: &PreparedFrozenIntent, private: BTreeMap<String, String>) -> Value {
    json!({"summary": intent.event.summary, "start": {"date": intent.event.start.date},
        "end": {"date": intent.event.end.date}, "transparency": intent.event.transparency,
        "extendedProperties": {"private": private}})
}

struct ValidatedEvent {
    etag: String,
    private: BTreeMap<String, String>,
}

fn validate_event(
    intent: &PreparedFrozenIntent,
    bytes: &[u8],
    expected_hash: &str,
) -> Result<ValidatedEvent> {
    #[derive(Deserialize)]
    struct Event {
        id: String,
        status: String,
        etag: String,
        summary: String,
        start: Value,
        end: Value,
        transparency: String,
        #[serde(rename = "extendedProperties")]
        extended: Extended,
        #[serde(flatten)]
        other: BTreeMap<String, Value>,
    }
    #[derive(Deserialize)]
    struct Extended {
        private: BTreeMap<String, String>,
    }
    fn date(value: &Value) -> Result<String> {
        let object = value.as_object().ok_or(TransportError::Invalid)?;
        if object.contains_key("dateTime") || object.get("timeZone").is_some_and(|v| !v.is_string())
        {
            return Err(TransportError::Invalid);
        }
        let date = object
            .get("date")
            .and_then(Value::as_str)
            .ok_or(TransportError::Invalid)?;
        let b = date.as_bytes();
        if b.len() != 10
            || b[4] != b'-'
            || b[7] != b'-'
            || !b
                .iter()
                .enumerate()
                .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
        {
            return Err(TransportError::Invalid);
        }
        let year = date[..4].parse().map_err(|_| TransportError::Invalid)?;
        if !(1..=9999).contains(&year) {
            return Err(TransportError::Invalid);
        }
        let month = date[5..7]
            .parse::<u8>()
            .ok()
            .and_then(|m| time::Month::try_from(m).ok())
            .ok_or(TransportError::Invalid)?;
        let day = date[8..].parse().map_err(|_| TransportError::Invalid)?;
        time::Date::from_calendar_date(year, month, day).map_err(|_| TransportError::Invalid)?;
        Ok(date.into())
    }
    let event: Event = ledger::decode_unique_json(bytes).map_err(|_| TransportError::Invalid)?;
    if event.id != intent.event_id
        || !matches!(event.status.as_str(), "confirmed" | "tentative")
        || ["recurrence", "recurringEventId", "originalStartTime"]
            .iter()
            .any(|k| event.other.contains_key(*k))
        || event
            .other
            .get("eventType")
            .is_some_and(|v| v.as_str() != Some("default"))
        || ownership(intent)
            .iter()
            .any(|(k, v)| event.extended.private.get(k) != Some(v))
    {
        return Err(TransportError::Invalid);
    }
    strong_etag(&event.etag)?;
    let start = date(&event.start)?;
    let end = date(&event.end)?;
    if end <= start {
        return Err(TransportError::Invalid);
    }
    let transparency = match event.transparency.as_str() {
        "transparent" => "transparent",
        "opaque" => "opaque",
        _ => return Err(TransportError::Invalid),
    };
    let payload = ProspectiveEvent {
        summary: event.summary,
        start: CalendarDate { date: start },
        end: CalendarDate { date: end },
        availability: "",
        transparency,
    };
    if ledger::payload_hash(&payload) != expected_hash {
        return Err(TransportError::Invalid);
    }
    Ok(ValidatedEvent {
        etag: event.etag,
        private: event.extended.private,
    })
}

#[cfg(test)]
mod tests;

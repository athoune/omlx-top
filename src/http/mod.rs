//! Blocking loopback HTTP client.
//!
//! SPECS.md section 4.1: `ureq` is built without TLS, gzip or cookies. The tool
//! only ever talks plain HTTP to `127.0.0.1`. Timeouts are 500 ms for the
//! connect and 1 s for the whole request (SPECS.md section 6.2).
//!
//! Status codes are deliberately *not* turned into errors: a 503 carries a
//! meaningful `{"status": "loading"}` body, and 401/403 must be classified,
//! not thrown away. Only a transport failure (connect, I/O, protocol) is an
//! error, which is what lets the sampler distinguish "unreachable" from
//! "degraded".

pub mod endpoints;

use std::fmt;
use std::time::Duration;

use serde::de::DeserializeOwned;
use ureq::Agent;

const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(1);

/// A transport-level failure: no HTTP answer was obtained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpError {
    pub message: String,
}

impl HttpError {
    pub fn transport(error: &ureq::Error) -> Self {
        HttpError {
            message: error.to_string(),
        }
    }
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for HttpError {}

/// One HTTP answer. `body` is `None` when the payload was empty or was not
/// valid JSON; `http_status` is still meaningful in that case.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpReply<T> {
    pub http_status: u16,
    pub body: Option<T>,
}

/// Classification of an HTTP status, per SPECS.md section 9.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusClass {
    Ok,
    Unauthorized,
    Forbidden,
    Loading,
    Other,
}

/// Either the transport answered, or it did not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    Answered(StatusClass),
    TransportError,
}

/// Classify an HTTP status code.
pub fn classify_status(status: u16) -> StatusClass {
    match status {
        200 => StatusClass::Ok,
        401 => StatusClass::Unauthorized,
        403 => StatusClass::Forbidden,
        503 => StatusClass::Loading,
        _ => StatusClass::Other,
    }
}

/// Classify an answered request, taking the body status into account.
///
/// A 200 whose body says `loading` is not healthy even though the transport
/// succeeded; keeping that distinction here means callers never have to
/// re-derive it. `/health` answers `"healthy"`, which remains [`StatusClass::Ok`].
pub fn classify_answered(http_status: u16, body_status: Option<&str>) -> StatusClass {
    match (http_status, body_status) {
        (200, Some("loading")) => StatusClass::Loading,
        (200, _) => StatusClass::Ok,
        _ => classify_status(http_status),
    }
}

/// Pure classification of a request outcome.
pub fn classify_outcome(outcome: &Result<u16, HttpError>) -> Reach {
    match outcome {
        Ok(status) => Reach::Answered(classify_status(*status)),
        Err(_) => Reach::TransportError,
    }
}

/// The `status` value written to the JSON `server` block.
///
/// The body's status wins when present. A 401/403 has no trustworthy body
/// status, so it always reports `"unauthorized"`. Any other answered request
/// without a body status reports `null`.
pub fn status_label(http_status: Option<u16>, body_status: Option<&str>) -> Option<String> {
    if matches!(http_status, Some(401) | Some(403)) {
        return Some("unauthorized".to_string());
    }
    body_status
        .filter(|status| !status.is_empty())
        .map(str::to_string)
}

/// A blocking client bound to one base URL.
#[derive(Clone)]
pub struct Client {
    agent: Agent,
    base_url: String,
    api_key: Option<String>,
}

impl Client {
    /// Build a client. Construction is infallible; failures surface on the
    /// first request as [`HttpError`].
    pub fn new(base_url: impl Into<String>, api_key: Option<String>) -> Self {
        let config = Agent::config_builder()
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_global(Some(TOTAL_TIMEOUT))
            .http_status_as_error(false)
            .build();

        Client {
            agent: config.into(),
            base_url: base_url.into(),
            api_key,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// GET a JSON document. `Err` means the transport failed; an invalid or
    /// empty body is reported as `Ok(HttpReply { body: None, .. })`.
    pub(crate) fn get<T: DeserializeOwned>(&self, path: &str) -> Result<HttpReply<T>, HttpError> {
        let url = format!("{}{}", self.base_url.trim_end_matches('/'), path);
        let mut request = self.agent.get(&url);
        if let Some(key) = self.api_key.as_deref() {
            request = request.header("Authorization", format!("Bearer {key}"));
        }

        let mut response = request
            .call()
            .map_err(|error| HttpError::transport(&error))?;
        let http_status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok());

        Ok(HttpReply { http_status, body })
    }

    /// Whether `/api/status` answered at the transport level on this URL.
    ///
    /// Any HTTP status counts: the port is bound and a server is there.
    pub fn probe(&self) -> bool {
        self.get::<serde_json::Value>("/api/status").is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transport_error() -> HttpError {
        HttpError {
            message: "connection refused".to_string(),
        }
    }

    #[test]
    fn transport_error_is_classified_as_unreachable() {
        let outcome: Result<u16, HttpError> = Err(transport_error());
        assert_eq!(classify_outcome(&outcome), Reach::TransportError);
    }

    #[test]
    fn http_statuses_are_classified() {
        assert_eq!(classify_status(200), StatusClass::Ok);
        assert_eq!(classify_status(401), StatusClass::Unauthorized);
        assert_eq!(classify_status(403), StatusClass::Forbidden);
        assert_eq!(classify_status(503), StatusClass::Loading);
        assert_eq!(classify_status(500), StatusClass::Other);
        assert_eq!(classify_status(404), StatusClass::Other);

        assert_eq!(classify_outcome(&Ok(200)), Reach::Answered(StatusClass::Ok));
        assert_eq!(
            classify_outcome(&Ok(503)),
            Reach::Answered(StatusClass::Loading)
        );

        // A 200 carrying `{"status": "loading"}` is loading, not healthy.
        assert_eq!(classify_answered(200, Some("ok")), StatusClass::Ok);
        assert_eq!(
            classify_answered(200, Some("loading")),
            StatusClass::Loading
        );
        assert_eq!(classify_answered(200, None), StatusClass::Ok);
        assert_eq!(
            classify_answered(401, Some("anything")),
            StatusClass::Unauthorized
        );
        assert_eq!(
            classify_answered(503, Some("loading")),
            StatusClass::Loading
        );
    }

    #[test]
    fn status_label_rules() {
        assert_eq!(status_label(Some(200), Some("ok")), Some("ok".to_string()));
        assert_eq!(
            status_label(Some(200), Some("loading")),
            Some("loading".to_string())
        );
        assert_eq!(
            status_label(Some(401), Some("ignored")),
            Some("unauthorized".to_string())
        );
        assert_eq!(
            status_label(Some(403), None),
            Some("unauthorized".to_string())
        );
        assert_eq!(status_label(Some(200), None), None);
        assert_eq!(status_label(None, None), None);
        assert_eq!(status_label(Some(500), None), None);
        assert_eq!(
            status_label(Some(503), Some("loading")),
            Some("loading".to_string())
        );
    }
}

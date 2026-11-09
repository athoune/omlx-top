//! URL and port resolution.
//!
//! The precedence is the one in SPECS.md section 6.3.3:
//!
//! 1. an explicit `--url`;
//! 2. the candidate process's own listening TCP socket;
//! 3. `OMLX_PORT` from the candidate's environment;
//! 4. `~/.omlx/settings.json` -> `server.port`;
//! 5. port 8000, then 8008, each probed with one `/api/status` call;
//! 6. 8000 as the documented default when no probe answers.
//!
//! Step 5 is injected as a closure so precedence can be tested without a live
//! server. The HTTP client itself belongs to a later batch.

use std::fmt;
use std::path::PathBuf;

use serde::Deserialize;

/// What discovery learned about a single candidate process.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PortEvidence {
    /// Port of the candidate's own listening TCP socket, when readable.
    pub socket_port: Option<u16>,
    /// `OMLX_PORT` from the candidate's environment, when readable.
    pub env_port: Option<u16>,
}

/// Which rule produced the resolved port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortSource {
    ExplicitUrl,
    ListeningSocket,
    ProcessEnv,
    SettingsFile,
    Probed,
    Default,
}

/// A resolved base URL and the rule that selected it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPort {
    pub port: u16,
    pub source: PortSource,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    InvalidUrl(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::InvalidUrl(url) => write!(f, "invalid --url value: {url}"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Resolve a base URL using the full precedence chain.
///
/// `probe` is called at most twice, first with 8000 and then with 8008, and
/// returns whether `/api/status` answered on that port.
pub fn resolve_port(
    cli_url: Option<&str>,
    evidence: &PortEvidence,
    settings_port: Option<u16>,
    mut probe: impl FnMut(u16) -> bool,
) -> Result<ResolvedPort, ConfigError> {
    if let Some(url) = cli_url {
        let port = parse_url_port(url)?;
        return Ok(ResolvedPort {
            port,
            source: PortSource::ExplicitUrl,
            url: url.to_string(),
        });
    }

    if let Some(port) = evidence.socket_port.filter(|port| *port != 0) {
        return Ok(resolved(port, PortSource::ListeningSocket));
    }

    if let Some(port) = evidence.env_port.filter(|port| *port != 0) {
        return Ok(resolved(port, PortSource::ProcessEnv));
    }

    if let Some(port) = settings_port.filter(|port| *port != 0) {
        return Ok(resolved(port, PortSource::SettingsFile));
    }

    if probe(8000) {
        return Ok(resolved(8000, PortSource::Probed));
    }
    if probe(8008) {
        return Ok(resolved(8008, PortSource::Probed));
    }

    // SPECS.md section 8 documents http://127.0.0.1:8000 as the final default.
    Ok(resolved(8000, PortSource::Default))
}

fn resolved(port: u16, source: PortSource) -> ResolvedPort {
    ResolvedPort {
        port,
        source,
        url: canonical_url(port),
    }
}

/// Canonical loopback URL for a port. The tool never talks to remote hosts.
pub fn canonical_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

/// Extract the port from a base URL. A URL without an explicit port means 80.
///
/// The parser is deliberately small: this tool only speaks plain HTTP to
/// loopback, so it does not need a general-purpose URL implementation.
pub fn parse_url_port(url: &str) -> Result<u16, ConfigError> {
    let url = url.trim();
    if url.is_empty() {
        return Err(ConfigError::InvalidUrl(url.to_string()));
    }

    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .filter(|authority| !authority.is_empty())
        .ok_or_else(|| ConfigError::InvalidUrl(url.to_string()))?;

    // Strip an optional userinfo section before looking for the port.
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, rest)| rest);

    let port_text = if let Some(after_bracket) = host_port.strip_prefix('[') {
        let end = after_bracket
            .find(']')
            .ok_or_else(|| ConfigError::InvalidUrl(url.to_string()))?;
        let after = &after_bracket[end + 1..];
        if after.is_empty() {
            return Ok(80);
        }
        after
            .strip_prefix(':')
            .ok_or_else(|| ConfigError::InvalidUrl(url.to_string()))?
    } else if let Some((_, port)) = host_port.rsplit_once(':') {
        port
    } else {
        return Ok(80);
    };

    let port: u16 = port_text
        .parse()
        .map_err(|_| ConfigError::InvalidUrl(url.to_string()))?;
    if port == 0 {
        return Err(ConfigError::InvalidUrl(url.to_string()));
    }
    Ok(port)
}

/// Path of the oMLX settings file, if `$HOME` is set.
pub fn settings_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".omlx").join("settings.json"))
}

/// Read `server.port` from `~/.omlx/settings.json`.
///
/// A missing, unreadable or malformed file is a normal degradation: the
/// resolver simply continues to the next rule.
pub fn load_settings_port() -> Option<u16> {
    let path = settings_path()?;
    let contents = std::fs::read_to_string(path).ok()?;
    settings_port_from_str(&contents)
}

/// Parse `server.port` out of a settings document. Pure so tests need no home
/// directory.
pub fn settings_port_from_str(contents: &str) -> Option<u16> {
    #[derive(Deserialize)]
    struct Settings {
        server: Option<Server>,
    }

    #[derive(Deserialize)]
    struct Server {
        port: Option<u16>,
    }

    let parsed: Settings = serde_json::from_str(contents).ok()?;
    parsed
        .server
        .and_then(|server| server.port)
        .filter(|port| *port != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn evidence(socket: Option<u16>, env: Option<u16>) -> PortEvidence {
        PortEvidence {
            socket_port: socket,
            env_port: env,
        }
    }

    #[test]
    fn explicit_url_wins_and_is_not_overridden() {
        let mut probed = Vec::new();
        let resolved = resolve_port(
            Some("http://127.0.0.1:9123"),
            &evidence(Some(8008), Some(7000)),
            Some(6000),
            |port| {
                probed.push(port);
                true
            },
        )
        .expect("valid URL");

        assert_eq!(resolved.port, 9123);
        assert_eq!(resolved.source, PortSource::ExplicitUrl);
        assert_eq!(resolved.url, "http://127.0.0.1:9123");
        assert!(probed.is_empty());
    }

    #[test]
    fn listening_socket_wins_over_env_and_settings() {
        let resolved = resolve_port(None, &evidence(Some(8008), Some(7000)), Some(6000), |_| {
            panic!("probe must not run")
        })
        .expect("no explicit URL cannot fail");

        assert_eq!(resolved.port, 8008);
        assert_eq!(resolved.source, PortSource::ListeningSocket);
    }

    #[test]
    fn env_wins_over_settings() {
        let resolved =
            resolve_port(None, &evidence(None, Some(7000)), Some(6000), |_| false).expect("ok");
        assert_eq!(resolved.port, 7000);
        assert_eq!(resolved.source, PortSource::ProcessEnv);
    }

    #[test]
    fn settings_wins_over_probe() {
        let resolved = resolve_port(None, &evidence(None, None), Some(6000), |_| true).expect("ok");
        assert_eq!(resolved.port, 6000);
        assert_eq!(resolved.source, PortSource::SettingsFile);
    }

    #[test]
    fn probes_8000_then_8008() {
        let probes = RefCell::new(Vec::new());
        let resolved = resolve_port(None, &evidence(None, None), None, |port| {
            probes.borrow_mut().push(port);
            port == 8008
        })
        .expect("ok");

        assert_eq!(resolved.port, 8008);
        assert_eq!(resolved.source, PortSource::Probed);
        assert_eq!(*probes.borrow(), vec![8000, 8008]);
    }

    #[test]
    fn first_probe_success_stops_the_chain() {
        let probes = RefCell::new(Vec::new());
        let resolved = resolve_port(None, &evidence(None, None), None, |port| {
            probes.borrow_mut().push(port);
            port == 8000
        })
        .expect("ok");

        assert_eq!(resolved.port, 8000);
        assert_eq!(resolved.source, PortSource::Probed);
        assert_eq!(*probes.borrow(), vec![8000]);
    }

    #[test]
    fn final_default_is_8000() {
        let resolved = resolve_port(None, &evidence(None, None), None, |_| false).expect("ok");
        assert_eq!(resolved.port, 8000);
        assert_eq!(resolved.source, PortSource::Default);
        assert_eq!(resolved.url, "http://127.0.0.1:8000");
    }

    #[test]
    fn url_ports() {
        assert_eq!(parse_url_port("http://127.0.0.1:8008"), Ok(8008));
        assert_eq!(parse_url_port("http://localhost"), Ok(80));
        assert_eq!(parse_url_port("https://localhost:443/"), Ok(443));
        assert_eq!(parse_url_port("http://[::1]:8008/v1"), Ok(8008));
        assert_eq!(parse_url_port("http://[::1]"), Ok(80));
        assert!(parse_url_port("").is_err());
        assert!(parse_url_port("http://127.0.0.1:0").is_err());
        assert!(parse_url_port("http://127.0.0.1:not-a-port").is_err());
    }

    #[test]
    fn settings_json_port() {
        let contents = r#"{
            "version": "1.0",
            "server": { "host": "127.0.0.1", "port": 8008 },
            "model": { "model_dirs": [] }
        }"#;
        assert_eq!(settings_port_from_str(contents), Some(8008));
        assert_eq!(settings_port_from_str("{}"), None);
        assert_eq!(settings_port_from_str("not json"), None);
        assert_eq!(settings_port_from_str(r#"{"server":{"port":0}}"#), None);
    }
}

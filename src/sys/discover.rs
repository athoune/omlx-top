//! Instance discovery: which oMLX processes exist, how they were started, and
//! which one the monitor should target.
//!
//! Discovery finding nothing is a normal outcome, not an error.

use crate::config::{self, ConfigError, PortEvidence};
use crate::model::{Instance, Mode};

use super::{proc, socket, SysError};

/// The GUI executable path suffix used to recognise an app-bundle ancestor.
const APP_GUI_SUFFIX: &str = "/oMLX.app/Contents/MacOS/oMLX";
const MAX_ANCESTOR_HOPS: usize = 4;

/// One entry of a parent chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ancestor {
    Node {
        pid: i32,
        path: Option<String>,
        comm: Option<String>,
    },
    /// The parent chain could not be read (for example EPERM).
    Unreadable,
}

/// Result of walking a candidate's ancestry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    pub mode: Mode,
    pub gui_pid: Option<i32>,
    pub parent_comm: Option<String>,
}

impl Classification {
    fn unknown() -> Self {
        Classification {
            mode: Mode::Unknown,
            gui_pid: None,
            parent_comm: None,
        }
    }
}

/// A process that looks like an oMLX server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub pid: i32,
    pub name: Option<String>,
    pub path: Option<String>,
    pub mode: Mode,
    pub gui_pid: Option<i32>,
    pub parent_comm: Option<String>,
    /// Ports of loopback/wildcard TCP sockets in LISTEN state.
    pub ports: Vec<u16>,
    /// `OMLX_PORT` from the process environment, when readable.
    pub env_port: Option<u16>,
    /// `OMLX_SUPERVISED` from the process environment, when readable.
    pub env_supervised: Option<String>,
    /// Owning uid; candidates owned by others are reported, never dropped.
    pub owner_uid: Option<u32>,
}

/// Candidate signature from SPECS.md section 6.3.1, in the documented order.
///
/// `argv0` may be supplied only when the caller already decided to read
/// `KERN_PROCARGS2` for an interpreter path. Environment variables are never
/// part of this decision.
pub fn signature_matches(name: Option<&str>, path: Option<&str>, argv0: Option<&str>) -> bool {
    // Rule 1: free, and correct if a future build names the binary itself.
    if name == Some("omlx-server") {
        return true;
    }

    let Some(path) = path else {
        return false;
    };

    // Rule 2: app-bundle server.
    if path.contains("/oMLX.app/Contents/Resources/Python/") {
        return true;
    }

    // Rule 3: Homebrew install under any prefix.
    if path.contains("/Cellar/omlx/") || path.contains("/opt/omlx/") {
        return true;
    }

    // Rule 4: interpreter path plus a rewritten argv[0]. This is the only
    // rule that consults the argument block.
    if !is_interpreter_path(path) {
        return false;
    }
    let Some(argv0) = argv0 else {
        return false;
    };
    matches!(basename(argv0), "omlx" | "omlx-server")
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn is_interpreter_path(path: &str) -> bool {
    let name = basename(path);
    // SPECS.md section 6.3.1: case-insensitive basename prefix, or a
    // framework build whose executable is `Python.app/Contents/MacOS/Python`.
    name.to_ascii_lowercase().starts_with("python") || path.contains("/Python.framework/")
}

/// Scan the process table and classify every oMLX server candidate.
pub fn discover() -> Result<Vec<Candidate>, SysError> {
    let pids = proc::list_pids()?;
    let mut candidates = Vec::new();

    for pid in pids {
        let name = proc::process_name(pid);
        let path = proc::pid_path(pid);

        // Rules 1-3 need no argument block.
        let cheap_match = signature_matches(name.as_deref(), path.as_deref(), None);
        let interpreter = path.as_deref().is_some_and(is_interpreter_path);

        // Rule 4 (and only rule 4) reads KERN_PROCARGS2 for detection, and
        // only for an interpreter path. After acceptance the same block also
        // supplies OMLX_PORT / OMLX_SUPERVISED; it never decides acceptance by
        // environment.
        let args = if cheap_match || interpreter {
            proc::read_procargs(pid).ok()
        } else {
            None
        };

        let accepted = cheap_match
            || signature_matches(
                name.as_deref(),
                path.as_deref(),
                args.as_ref().and_then(proc::ProcArgs::argv0),
            );

        if !accepted {
            continue;
        }

        let ancestry = read_ancestors(pid, MAX_ANCESTOR_HOPS);
        let classification = classify_ancestors(&ancestry);

        // A permission error on a root-run service leaves the port unknown but
        // the candidate stays in the list.
        let ports = socket::list_listening_ports(pid).unwrap_or_default();
        let env_port = args.as_ref().and_then(proc::ProcArgs::env_port);
        let env_supervised = args
            .as_ref()
            .and_then(proc::ProcArgs::env_supervised)
            .map(str::to_string);
        let owner_uid = proc::owner_uid(pid).ok();

        candidates.push(Candidate {
            pid,
            name,
            path,
            mode: classification.mode,
            gui_pid: classification.gui_pid,
            parent_comm: classification.parent_comm,
            ports,
            env_port,
            env_supervised,
            owner_uid,
        });
    }

    candidates.sort_by_key(|candidate| candidate.pid);
    Ok(candidates)
}

fn read_ancestors(pid: i32, max_hops: usize) -> Vec<Ancestor> {
    let mut ancestry = Vec::with_capacity(max_hops);
    let mut current = pid;

    for _ in 0..max_hops {
        let parent = match proc::parent_pid(current) {
            Ok(parent) if parent > 0 => parent,
            // EPERM, ESRCH or a malformed answer: stop and let the classifier
            // report an unreadable ancestry instead of inventing a mode.
            _ => {
                ancestry.push(Ancestor::Unreadable);
                break;
            }
        };

        let path = proc::pid_path(parent);
        let comm = proc::process_name(parent);
        ancestry.push(Ancestor::Node {
            pid: parent,
            path,
            comm,
        });

        if parent == 1 {
            break;
        }
        current = parent;
    }

    ancestry
}

/// Classify a candidate from its parent chain (SPECS.md section 6.3.2).
///
/// The chain starts with the direct parent. An app-bundle ancestor anywhere in
/// the first four hops wins; otherwise the direct parent decides between
/// launchd and a manual launch.
pub fn classify_ancestors(ancestry: &[Ancestor]) -> Classification {
    let first = match ancestry.first() {
        Some(first) => first,
        None => return Classification::unknown(),
    };
    if matches!(first, Ancestor::Unreadable) {
        return Classification::unknown();
    }

    for ancestor in ancestry {
        match ancestor {
            Ancestor::Unreadable => return Classification::unknown(),
            Ancestor::Node { pid, path, .. } => {
                if path
                    .as_deref()
                    .is_some_and(|path| path.ends_with(APP_GUI_SUFFIX))
                {
                    return Classification {
                        mode: Mode::App,
                        gui_pid: Some(*pid),
                        parent_comm: ancestor_comm(first).map(str::to_string),
                    };
                }
            }
        }
    }

    match first {
        // A direct child of launchd is the Homebrew service mode.
        Ancestor::Node { pid: 1, .. } => Classification {
            mode: Mode::Service,
            gui_pid: None,
            parent_comm: Some("launchd".to_string()),
        },
        Ancestor::Node { comm, .. } => Classification {
            mode: Mode::Manual,
            gui_pid: None,
            parent_comm: comm.clone(),
        },
        Ancestor::Unreadable => Classification::unknown(),
    }
}

fn ancestor_comm(ancestor: &Ancestor) -> Option<&str> {
    match ancestor {
        Ancestor::Node { comm, .. } => comm.as_deref(),
        Ancestor::Unreadable => None,
    }
}

/// Whether any discovered instance listens on `port`. Used by `--list` to
/// apply the exit-code rule when `--url` was requested.
pub fn has_instance_on_port(instances: &[Instance], port: u16) -> bool {
    instances.iter().any(|instance| instance.port == Some(port))
}

/// Turn candidates into the table shown by `--list`, applying the selection
/// rules from SPECS.md section 6.3.4.
pub fn build_instances(
    candidates: &[Candidate],
    cli_url: Option<&str>,
    settings_port: Option<u16>,
    probe: &mut dyn FnMut(u16) -> bool,
) -> Result<Vec<Instance>, ConfigError> {
    let cli_port = cli_url.map(config::parse_url_port).transpose()?;
    let preferred_port = cli_port.or(settings_port);

    let mut instances = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let socket_port = pick_socket_port(&candidate.ports, preferred_port);
        let evidence = PortEvidence {
            socket_port,
            env_port: candidate.env_port,
        };
        let resolved = config::resolve_port(None, &evidence, settings_port, &mut *probe)?;

        instances.push(Instance {
            mode: candidate.mode,
            pid: candidate.pid,
            gui_pid: candidate.gui_pid,
            port: Some(resolved.port),
            url: Some(resolved.url),
            selected: false,
        });
    }

    // Deterministic ordering: lowest port first, then pid.
    instances.sort_by_key(|instance| (instance.port.unwrap_or(u16::MAX), instance.pid));

    let selected = if let Some(port) = cli_port {
        instances
            .iter()
            .position(|instance| instance.port == Some(port))
    } else if let Some(port) = settings_port {
        instances
            .iter()
            .position(|instance| instance.port == Some(port))
            .or_else(|| first_index(&instances))
    } else {
        first_index(&instances)
    };

    if let Some(index) = selected {
        instances[index].selected = true;
        if let Some(url) = cli_url {
            instances[index].url = Some(url.to_string());
        }
    }

    Ok(instances)
}

fn pick_socket_port(ports: &[u16], preferred: Option<u16>) -> Option<u16> {
    if ports.is_empty() {
        return None;
    }
    preferred
        .filter(|port| ports.contains(port))
        .or_else(|| ports.iter().copied().min())
}

fn first_index(instances: &[Instance]) -> Option<usize> {
    (!instances.is_empty()).then_some(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(pid: i32, path: &str, comm: &str) -> Ancestor {
        Ancestor::Node {
            pid,
            path: Some(path.to_string()),
            comm: Some(comm.to_string()),
        }
    }

    #[test]
    fn launchd_parent_is_service() {
        let chain = vec![node(1, "/sbin/launchd", "launchd")];
        let classification = classify_ancestors(&chain);
        assert_eq!(classification.mode, Mode::Service);
        assert_eq!(classification.gui_pid, None);
    }

    #[test]
    fn app_bundle_ancestor_is_app_and_names_the_gui() {
        let chain = vec![node(
            38_225,
            "/Applications/oMLX.app/Contents/MacOS/oMLX",
            "oMLX",
        )];
        let classification = classify_ancestors(&chain);
        assert_eq!(classification.mode, Mode::App);
        assert_eq!(classification.gui_pid, Some(38_225));
    }

    #[test]
    fn app_bundle_found_beyond_the_direct_parent() {
        let chain = vec![
            node(500, "/usr/bin/env", "env"),
            node(38_225, "/Applications/oMLX.app/Contents/MacOS/oMLX", "oMLX"),
        ];
        let classification = classify_ancestors(&chain);
        assert_eq!(classification.mode, Mode::App);
        assert_eq!(classification.gui_pid, Some(38_225));
    }

    #[test]
    fn shell_parent_is_manual() {
        let chain = vec![
            node(500, "/bin/zsh", "zsh"),
            node(1, "/sbin/launchd", "launchd"),
        ];
        let classification = classify_ancestors(&chain);
        assert_eq!(classification.mode, Mode::Manual);
        assert_eq!(classification.parent_comm.as_deref(), Some("zsh"));
    }

    #[test]
    fn unreadable_ancestry_is_unknown() {
        let chain = vec![Ancestor::Unreadable];
        let classification = classify_ancestors(&chain);
        assert_eq!(classification.mode, Mode::Unknown);
        assert_eq!(classification.parent_comm, None);
    }

    #[test]
    fn empty_ancestry_is_unknown() {
        assert_eq!(classify_ancestors(&[]).mode, Mode::Unknown);
    }

    #[test]
    fn candidate_signature_matrix() {
        let app_path =
            "/Applications/oMLX.app/Contents/Resources/Python/cpython-3.11/bin/python3.11";
        let cellar_path = "/opt/homebrew/Cellar/omlx/0.7.3/libexec/bin/python3.11";
        let opt_path = "/opt/omlx/bin/python3";
        let framework_path = "/opt/homebrew/Cellar/python@3.11/3.11.18/Frameworks/\
Python.framework/Versions/3.11/Resources/Python.app/Contents/MacOS/Python";

        // Rule 1: kernel p_comm is already the server name.
        assert!(signature_matches(Some("omlx-server"), None, None));
        // Rule 2: app-bundle server path.
        assert!(signature_matches(None, Some(app_path), None));
        // Rule 3: Homebrew Cellar and /opt/omlx layouts.
        assert!(signature_matches(None, Some(cellar_path), None));
        assert!(signature_matches(None, Some(opt_path), None));
        // Rule 4: interpreter path plus the rewritten argv[0].
        assert!(signature_matches(
            Some("python3.11"),
            Some("/usr/bin/python3"),
            Some("omlx-server")
        ));
        assert!(signature_matches(
            None,
            Some("/usr/bin/python3.12"),
            Some("/opt/omlx/bin/omlx")
        ));

        // Rule 4 accepts every interpreter spelling in SPECS.md, including
        // the capitalised framework name and the Python.framework path.
        for name in ["Python", "python", "python3", "python3.11", "python3.13"] {
            let path = format!("/usr/bin/{name}");
            assert!(
                signature_matches(None, Some(path.as_str()), Some("omlx-server")),
                "{name} should be treated as an interpreter"
            );
        }
        assert!(signature_matches(
            Some("Python"),
            Some(framework_path),
            Some("omlx-server")
        ));

        // Rule 4 never accepts an unrelated argv[0], a missing argv[0], or a
        // non-interpreter executable.
        assert!(!signature_matches(
            None,
            Some("/usr/bin/python3"),
            Some("/usr/bin/other")
        ));
        assert!(!signature_matches(None, Some("/usr/bin/python3"), None));
        assert!(!signature_matches(
            Some("Python"),
            Some(framework_path),
            Some("serve")
        ));
        assert!(!signature_matches(None, Some(framework_path), None));
        assert!(!signature_matches(
            None,
            Some("/usr/bin/ruby"),
            Some("omlx-server")
        ));
    }

    #[test]
    fn framework_python_requires_omlx_argv0() {
        let framework_path = "/opt/homebrew/Cellar/python@3.11/3.11.18/Frameworks/\
Python.framework/Versions/3.11/Resources/Python.app/Contents/MacOS/Python";

        assert!(signature_matches(
            Some("Python"),
            Some(framework_path),
            Some("omlx-server")
        ));
        assert!(!signature_matches(
            Some("Python"),
            Some(framework_path),
            Some("/usr/bin/other")
        ));
        assert!(!signature_matches(
            None,
            Some("/usr/bin/ruby"),
            Some("omlx-server")
        ));
    }

    #[test]
    fn environment_alone_never_accepts_a_candidate() {
        let args = proc::ProcArgs {
            exec_path: "/usr/bin/python3".to_string(),
            argv: vec!["/usr/bin/python3".to_string()],
            env: vec![
                "OMLX_PORT=8008".to_string(),
                "OMLX_SUPERVISED=launchd".to_string(),
            ],
        };

        // The environment is readable after acceptance, but it does not make
        // the process a candidate on its own.
        assert_eq!(args.env_port(), Some(8008));
        assert_eq!(args.env_supervised(), Some("launchd"));
        assert!(!signature_matches(
            Some("python3.11"),
            Some("/usr/bin/python3"),
            args.argv0()
        ));
    }

    #[test]
    fn url_match_decision_is_pure() {
        let instance = |port: u16| Instance {
            mode: Mode::App,
            pid: 100 + i32::from(port),
            gui_pid: None,
            port: Some(port),
            url: Some(format!("http://127.0.0.1:{port}")),
            selected: false,
        };
        let instances = vec![instance(8008)];

        assert!(has_instance_on_port(&instances, 8008));
        assert!(!has_instance_on_port(&instances, 9999));
    }

    #[test]
    fn empty_candidate_list_yields_no_instances() {
        let mut probe = |_| false;
        let instances = build_instances(&[], None, None, &mut probe).expect("no explicit URL");
        assert!(instances.is_empty());
    }

    #[test]
    fn selection_prefers_settings_port_then_lowest_port() {
        let candidate = |pid: i32, port: u16| Candidate {
            pid,
            name: Some("omlx-server".to_string()),
            path: None,
            mode: Mode::Service,
            gui_pid: None,
            parent_comm: Some("launchd".to_string()),
            ports: vec![port],
            env_port: None,
            env_supervised: None,
            owner_uid: None,
        };

        let candidates = vec![candidate(20, 8000), candidate(10, 8008)];
        let mut probe = |_| false;

        let instances =
            build_instances(&candidates, None, Some(8008), &mut probe).expect("no explicit URL");
        assert_eq!(instances[0].port, Some(8000));
        assert!(!instances[0].selected);
        assert_eq!(instances[1].port, Some(8008));
        assert!(instances[1].selected);

        let instances =
            build_instances(&candidates, None, None, &mut probe).expect("no explicit URL");
        assert_eq!(instances[0].port, Some(8000));
        assert!(instances[0].selected);
    }

    #[test]
    fn explicit_url_selects_matching_port() {
        let candidate = |pid: i32, port: u16| Candidate {
            pid,
            name: Some("omlx-server".to_string()),
            path: None,
            mode: Mode::App,
            gui_pid: Some(pid - 1),
            parent_comm: None,
            ports: vec![port],
            env_port: None,
            env_supervised: None,
            owner_uid: None,
        };
        let candidates = vec![candidate(20, 8000), candidate(10, 8008)];
        let mut probe = |_| false;

        let instances =
            build_instances(&candidates, Some("http://127.0.0.1:8008"), None, &mut probe)
                .expect("valid URL");
        assert_eq!(instances[0].port, Some(8000));
        assert!(!instances[0].selected);
        assert_eq!(instances[1].port, Some(8008));
        assert!(instances[1].selected);
        assert_eq!(instances[1].url.as_deref(), Some("http://127.0.0.1:8008"));
    }
}

//! Errors of every lifecycle operation.
//!
//! [`BrowserError`] is what every fallible public function returns. Its
//! configuration and profile details are typed ([`ConfigError`],
//! [`ProfileError`]) so callers can match on them instead of parsing strings.
//! No message ever contains a secret: proxy URLs, environment variable values
//! and preference values are never formatted into an error.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bidi_lite::error::BidiError;

use crate::config::LaunchMode;

/// Errors of every lifecycle operation: discovery, launch, attach, session,
/// stop and pool management.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BrowserError {
    /// No Firefox executable was found. `searched` lists every path that was
    /// tried, in order, so the caller can tell the user where to install it or
    /// what to put in `FIREFOX_PATH`.
    #[error(
        "Firefox executable not found; set FIREFOX_PATH or BrowserConfigBuilder::firefox_path (searched: {})",
        PathList(searched)
    )]
    ExecutableNotFound {
        /// Every candidate path, in search order.
        searched: Vec<PathBuf>,
    },
    /// The operating system refused to start the executable.
    #[error("failed to spawn {}: {source}", path.display())]
    SpawnFailed {
        /// The executable that was spawned.
        path: PathBuf,
        /// The underlying OS error.
        #[source]
        source: std::io::Error,
    },
    /// Firefox exited before its ports were ready.
    #[error("Firefox exited before it was ready ({}): {reason}", ExitCode(*code))]
    EarlyExit {
        /// The exit code, when the process exited normally.
        code: Option<i32>,
        /// Best guess at why, derived from the exit code.
        reason: EarlyExitReason,
    },
    /// A port did not become ready within `startup_timeout`.
    #[error("Firefox did not get port {port} ready within {timeout:?}")]
    StartupTimeout {
        /// The configured startup timeout.
        timeout: Duration,
        /// The first port that was not ready (BiDi or RDP).
        port: u16,
    },
    /// A fixed port requested for a new browser is already taken.
    #[error("port {port} is already in use")]
    PortConflict {
        /// The port that is taken.
        port: u16,
    },
    /// Nothing that speaks Firefox BiDi (or RDP, for the RDP port) answers.
    #[error("no Firefox endpoint answers at {host}:{port}")]
    RemoteUnavailable {
        /// Host that was probed.
        host: String,
        /// Port that was probed.
        port: u16,
    },
    /// The Firefox at `host:port` already has a BiDi session that is not ours.
    /// Firefox allows a single session and keeps it after its WebSocket dies,
    /// so only ending it from its owner or restarting that Firefox frees it.
    #[error(
        "Firefox at {host}:{port} already has a BiDi session (Firefox allows only one); end that session or restart that Firefox"
    )]
    SessionBusy {
        /// Host of the busy browser.
        host: String,
        /// BiDi port of the busy browser.
        port: u16,
    },
    /// [`Browser::rdp_client`](crate::browser::Browser::rdp_client) was called
    /// on a browser configured without RDP, which is opt-in for security.
    #[error(
        "RDP is disabled for this browser; enable it with BrowserConfigBuilder::rdp(true) or rdp_port(..)"
    )]
    RdpDisabled,
    /// The configuration is invalid.
    #[error("invalid configuration: {0}")]
    InvalidConfig(#[from] ConfigError),
    /// The browser was stopped (or its process died without `auto_relaunch`).
    #[error("the browser was stopped")]
    Stopped,
    /// The profile directory could not be prepared or read.
    #[error("profile error: {0}")]
    Profile(#[from] ProfileError),
    /// An error from the underlying `bidi-lite` client.
    #[error("BiDi error: {0}")]
    Bidi(#[from] BidiError),
    /// Any other I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Why Firefox exited before becoming ready, derived from its exit code
/// (observed with Firefox 157 in the spike).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EarlyExitReason {
    /// Exit code 0 before the ports were ready: Firefox found another
    /// instance owning the profile and handed the launch over to it.
    ProfileInUse,
    /// Exit code 69: Firefox could not bind its BiDi port.
    PortInUse,
    /// Any other exit (crash, signal, missing display, ...).
    Unknown,
}

impl EarlyExitReason {
    /// Classifies an exit code the way Firefox uses them during startup.
    pub fn from_exit_code(code: Option<i32>) -> Self {
        match code {
            Some(0) => Self::ProfileInUse,
            Some(69) => Self::PortInUse,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for EarlyExitReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ProfileInUse => {
                "the profile is already in use by another Firefox, which took over the launch"
            }
            Self::PortInUse => "a port it needed is already in use",
            Self::Unknown => "unknown reason (crash, signal, or no display without headless)",
        })
    }
}

/// Invalid configuration, reported by
/// [`BrowserConfig::validate`](crate::config::BrowserConfig::validate).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// `Auto` and `AttachOnly` need to know which port to look at.
    #[error("{mode:?} mode needs an explicit BiDi port; port 0 only works with LaunchNew")]
    PortRequired {
        /// The mode that needs a port.
        mode: LaunchMode,
    },
    /// Only `AttachOnly` may target another machine: a launch is always local.
    #[error("host {host} is not local; only AttachOnly may target a remote host")]
    RemoteHostRequiresAttachOnly {
        /// The configured host.
        host: String,
    },
    /// Launching on the user's real profile would write remote-control prefs
    /// into it, so it is refused; attaching is fine.
    #[error(
        "ProfileMode::UserDefault cannot be launched (it would write remote-control prefs into your real Firefox profile); use it only with AttachOnly"
    )]
    UserDefaultProfileUnsupported,
    /// BiDi and RDP were given the same fixed port.
    #[error("BiDi and RDP cannot share port {port}")]
    PortClash {
        /// The shared port.
        port: u16,
    },
    /// A per-port profile is named after the BiDi port, so it needs one.
    #[error(
        "ProfileMode::PersistentPerPort needs a fixed BiDi port (its directory is named after it)"
    )]
    PerPortProfileNeedsPort,
    /// An attached Firefox's RDP port cannot be discovered.
    #[error(
        "AttachOnly and Auto with RDP need an explicit rdp_port: an attached Firefox's RDP port cannot be discovered"
    )]
    RdpPortRequired,
    /// Firefox prefs cannot carry proxy credentials.
    #[error(
        "proxy URLs with credentials are not supported (Firefox prefs cannot carry them); answer proxy authentication through BiDi network.continueWithAuth"
    )]
    ProxyCredentialsUnsupported,
    /// The proxy URL could not be understood. The URL itself is not echoed.
    #[error("invalid proxy: {reason}")]
    InvalidProxy {
        /// What is wrong with it.
        reason: &'static str,
    },
    /// A window dimension was zero.
    #[error("window size must be at least 1x1")]
    InvalidWindowSize,
    /// A timeout was zero, which would make every operation fail at once.
    #[error("{which} must be greater than zero")]
    ZeroTimeout {
        /// Which timeout (`connect_timeout`, `startup_timeout`, `command_timeout`).
        which: &'static str,
    },
}

/// A profile directory could not be prepared or read.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProfileError {
    /// The profile directory could not be created.
    #[error("cannot create profile directory {}: {source}", path.display())]
    CreateDir {
        /// The directory.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
    /// `user.js` could not be read or written.
    #[error("cannot update {}: {source}", path.display())]
    UserJs {
        /// The `user.js` path.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
    /// Firefox's port file (`WebDriverBiDiServer.json`) is unreadable.
    #[error("invalid port file {}: {reason}", path.display())]
    PortFile {
        /// The port file.
        path: PathBuf,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// A running Firefox owns this profile; launching on it would hand the
    /// launch over to that instance instead of starting ours.
    #[error("profile {} is in use by a running Firefox", path.display())]
    Locked {
        /// The profile directory.
        path: PathBuf,
    },
    /// Snap Firefox only sees profiles under `$HOME`, and `HOME` is not set.
    #[error("snap Firefox needs profiles under $HOME, but HOME is not set")]
    SnapHomeUnavailable,
}

/// Formats a list of paths as `a, b, c` for error messages.
struct PathList<'a>(&'a [PathBuf]);

impl fmt::Display for PathList<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for path in self.0.iter().map(PathBuf::as_path).map(Path::display) {
            if !first {
                f.write_str(", ")?;
            }
            first = false;
            write!(f, "{path}")?;
        }
        Ok(())
    }
}

/// Formats an optional exit code (`None` means killed by a signal).
struct ExitCode(Option<i32>);

impl fmt::Display for ExitCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(code) => write!(f, "exit code {code}"),
            None => f.write_str("killed by a signal"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    fn io_err() -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied")
    }

    /// ERR-01: ExecutableNotFound names FIREFOX_PATH and every searched path.
    #[test]
    fn executable_not_found_names_env_var_and_paths() {
        // Given an error with two searched paths
        let err = BrowserError::ExecutableNotFound {
            searched: vec![PathBuf::from("/a/firefox"), PathBuf::from("/b/firefox")],
        };
        // When it is displayed
        let msg = err.to_string();
        // Then it names the override and both paths
        assert!(msg.contains("FIREFOX_PATH"), "{msg}");
        assert!(msg.contains("/a/firefox, /b/firefox"), "{msg}");
    }

    /// ERR-02: SpawnFailed names the path and keeps the io error as source.
    #[test]
    fn spawn_failed_names_path_and_keeps_source() {
        // Given a spawn failure
        let err = BrowserError::SpawnFailed {
            path: PathBuf::from("/x/firefox"),
            source: io_err(),
        };
        // Then the path is in the message and the io error is the source
        assert!(err.to_string().contains("/x/firefox"));
        let source = err.source().map(ToString::to_string);
        assert_eq!(source.as_deref(), Some("denied"));
    }

    /// ERR-03: EarlyExit explains its reason and exit code.
    #[test]
    fn early_exit_explains_reason_and_code() {
        // Given the three reasons
        let msg = |code| {
            BrowserError::EarlyExit {
                code,
                reason: EarlyExitReason::from_exit_code(code),
            }
            .to_string()
        };
        // Then each message explains itself
        let in_use = msg(Some(0));
        assert!(
            in_use.contains("profile") && in_use.contains("already in use"),
            "{in_use}"
        );
        let port = msg(Some(69));
        assert!(port.contains("port") && port.contains("in use"), "{port}");
        assert!(msg(Some(3)).contains("exit code 3"));
        assert!(msg(None).contains("killed by a signal"));
    }

    /// ERR-04: StartupTimeout names the timeout and the port that never became ready.
    #[test]
    fn startup_timeout_names_timeout_and_port() {
        let msg = BrowserError::StartupTimeout {
            timeout: Duration::from_secs(10),
            port: 6123,
        }
        .to_string();
        assert!(msg.contains("10s") && msg.contains("6123"), "{msg}");
    }

    /// ERR-05: PortConflict names the port.
    #[test]
    fn port_conflict_names_port() {
        assert!(
            BrowserError::PortConflict { port: 9333 }
                .to_string()
                .contains("9333")
        );
    }

    /// ERR-06: RemoteUnavailable names host and port.
    #[test]
    fn remote_unavailable_names_host_and_port() {
        let msg = BrowserError::RemoteUnavailable {
            host: "10.0.0.5".into(),
            port: 9222,
        }
        .to_string();
        assert!(msg.contains("10.0.0.5:9222"), "{msg}");
    }

    /// ERR-07: SessionBusy explains the one-session rule and the remedy.
    #[test]
    fn session_busy_explains_rule_and_remedy() {
        let msg = BrowserError::SessionBusy {
            host: "127.0.0.1".into(),
            port: 9222,
        }
        .to_string();
        assert!(msg.contains("127.0.0.1:9222"), "{msg}");
        assert!(msg.contains("already has a BiDi session"), "{msg}");
        assert!(
            msg.contains("end that session") && msg.contains("restart"),
            "{msg}"
        );
    }

    /// ERR-08: RdpDisabled tells how to enable RDP.
    #[test]
    fn rdp_disabled_tells_how_to_enable_it() {
        assert!(BrowserError::RdpDisabled.to_string().contains("rdp(true)"));
    }

    /// ERR-09: Every ConfigError variant has a distinct, actionable message.
    #[test]
    fn config_error_messages_are_distinct() {
        // Given every variant
        let all = [
            ConfigError::PortRequired {
                mode: LaunchMode::Auto,
            },
            ConfigError::RemoteHostRequiresAttachOnly { host: "h".into() },
            ConfigError::UserDefaultProfileUnsupported,
            ConfigError::PortClash { port: 1 },
            ConfigError::RdpPortRequired,
            ConfigError::PerPortProfileNeedsPort,
            ConfigError::ProxyCredentialsUnsupported,
            ConfigError::InvalidProxy { reason: "r" },
            ConfigError::InvalidWindowSize,
            ConfigError::ZeroTimeout {
                which: "connect_timeout",
            },
        ];
        // When displayed
        let msgs: Vec<String> = all.iter().map(ToString::to_string).collect();
        // Then none is empty and all differ
        let unique: std::collections::HashSet<&String> = msgs.iter().collect();
        assert_eq!(unique.len(), msgs.len(), "{msgs:?}");
        assert!(msgs.iter().all(|m| m.len() > 10), "{msgs:?}");
    }

    /// ERR-10: Every ProfileError variant names its path when it has one.
    #[test]
    fn profile_errors_name_their_path() {
        let p = || PathBuf::from("/prof/x");
        let all = [
            ProfileError::CreateDir {
                path: p(),
                source: io_err(),
            },
            ProfileError::UserJs {
                path: p(),
                source: io_err(),
            },
            ProfileError::PortFile {
                path: p(),
                reason: "bad json",
            },
            ProfileError::Locked { path: p() },
        ];
        for err in all {
            assert!(err.to_string().contains("/prof/x"), "{err}");
        }
        assert!(
            ProfileError::SnapHomeUnavailable
                .to_string()
                .contains("HOME")
        );
    }

    /// ERR-11: BidiError and io::Error convert with ? and keep their source.
    #[test]
    fn bidi_and_io_errors_convert() {
        fn bidi() -> Result<(), BrowserError> {
            Err(BidiError::Disconnected)?
        }
        fn io() -> Result<(), BrowserError> {
            Err(io_err())?
        }
        assert!(matches!(
            bidi(),
            Err(BrowserError::Bidi(BidiError::Disconnected))
        ));
        assert!(matches!(io(), Err(BrowserError::Io(_))));
        let wrapped = BrowserError::from(ConfigError::InvalidWindowSize);
        assert!(wrapped.source().is_some());
    }
}

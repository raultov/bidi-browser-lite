//! Spawning Firefox, waiting for its ports, and terminating it.
//!
//! The spawned PID is Firefox itself, also for snap and Ubuntu's wrapper
//! script (both `exec`, verified in the spike), so no process group is
//! needed. Standard streams are null: a piped stream would kill a Firefox
//! kept alive on drop with SIGPIPE once this process exits. Early exits are
//! classified by exit code instead (see [`EarlyExitReason`]).

use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, Command};
use tokio::time::Instant;
use tracing::{debug, warn};

use crate::error::{BrowserError, EarlyExitReason};
use crate::probe;
use crate::profile::Profile;

/// How often readiness is polled.
const POLL: Duration = Duration::from_millis(200);
/// Upper bound of one probe inside the readiness loop.
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);
/// How often `terminate` checks whether the process exited.
const EXIT_POLL: Duration = Duration::from_millis(50);
/// How long `terminate` waits after the final kill.
const KILL_WAIT: Duration = Duration::from_secs(5);

/// A Firefox child process owned by this library. Dropping it kills the
/// process unless it is kept alive.
#[doc(hidden)]
#[derive(Debug)]
pub struct FirefoxProcess {
    child: Child,
    pid: Option<u32>,
    keep_alive: bool,
}

impl FirefoxProcess {
    fn new(child: Child, keep_alive: bool) -> Self {
        let pid = child.id();
        Self {
            child,
            pid,
            keep_alive,
        }
    }

    /// The OS process id.
    pub fn id(&self) -> Option<u32> {
        self.pid
    }

    /// Whether the process has not exited yet.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Stops the process: on Unix SIGTERM, then a kill after `grace`; on
    /// Windows a kill (the graceful path there is BiDi `browser.close`,
    /// which `Browser::stop` sends first). Idempotent.
    pub async fn terminate(&mut self, grace: Duration) {
        if !self.is_running() {
            return;
        }
        if request_exit(self.pid) && self.wait_exit(grace).await {
            return;
        }
        debug!(pid = ?self.pid, "killing Firefox");
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(KILL_WAIT, self.child.wait()).await;
    }

    /// Waits up to `limit` for the process to exit.
    pub(crate) async fn wait_exit(&mut self, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        loop {
            if !self.is_running() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(EXIT_POLL).await;
        }
    }

    /// Leaves the process running when this handle is dropped.
    pub(crate) fn keep_alive(&mut self) {
        self.keep_alive = true;
    }
}

impl Drop for FirefoxProcess {
    fn drop(&mut self) {
        if !self.keep_alive && self.is_running() {
            debug!(pid = ?self.pid, "killing Firefox on drop");
            let _ = self.child.start_kill();
        }
    }
}

/// Asks the process to exit (SIGTERM). Returns whether a request was sent.
#[cfg(unix)]
fn request_exit(pid: Option<u32>) -> bool {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    let Some(pid) = pid.and_then(|pid| i32::try_from(pid).ok()) else {
        return false;
    };
    kill(Pid::from_raw(pid), Signal::SIGTERM).is_ok()
}

/// Windows has no SIGTERM; the caller goes straight to a kill.
#[cfg(not(unix))]
fn request_exit(_pid: Option<u32>) -> bool {
    false
}

/// What to start and how.
pub(crate) struct SpawnSpec<'a> {
    pub(crate) path: &'a Path,
    pub(crate) args: Vec<OsString>,
    pub(crate) env: &'a [(OsString, OsString)],
}

/// Which ports must answer before the launch counts as ready.
pub(crate) struct Readiness<'a> {
    pub(crate) host: &'a str,
    /// 0 = read the port Firefox chose from the profile's port file.
    pub(crate) bidi_port: u16,
    pub(crate) rdp_port: Option<u16>,
    pub(crate) profile: &'a Profile,
    pub(crate) timeout: Duration,
}

/// Starts Firefox and waits until its BiDi port (and RDP port, if any)
/// answer. Returns the process and the real BiDi port. A process that exits
/// first yields `EarlyExit`; one that is not ready in time is killed and
/// yields `StartupTimeout` naming the first port that was not ready.
pub(crate) async fn spawn(
    spec: SpawnSpec<'_>,
    ready: &Readiness<'_>,
) -> Result<(FirefoxProcess, u16), BrowserError> {
    let mut command = Command::new(spec.path);
    command
        .args(&spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (key, value) in spec.env {
        debug!("Firefox env var set: {}", key.to_string_lossy());
        command.env(key, value);
    }
    let child = command
        .spawn()
        .map_err(|source| BrowserError::SpawnFailed {
            path: spec.path.to_path_buf(),
            source,
        })?;
    let mut process = FirefoxProcess::new(child, false);
    debug!(pid = ?process.id(), path = %spec.path.display(), "Firefox spawned");
    let port = wait_ready(&mut process, ready).await?;
    Ok((process, port))
}

async fn wait_ready(
    process: &mut FirefoxProcess,
    ready: &Readiness<'_>,
) -> Result<u16, BrowserError> {
    let deadline = Instant::now() + ready.timeout;
    loop {
        if let Ok(Some(status)) = process.child.try_wait() {
            let code = status.code();
            return Err(BrowserError::EarlyExit {
                code,
                reason: EarlyExitReason::from_exit_code(code),
            });
        }
        let unready = match check_ports(ready).await {
            Ok(port) => return Ok(port),
            Err(port) => port,
        };
        if Instant::now() >= deadline {
            warn!(port = unready, "Firefox not ready in time; killing it");
            process.terminate(Duration::ZERO).await;
            return Err(BrowserError::StartupTimeout {
                timeout: ready.timeout,
                port: unready,
            });
        }
        tokio::time::sleep(POLL).await;
    }
}

/// `Ok(bidi_port)` when every port answers, else `Err(first unready port)`
/// (0 while Firefox has not written its port file yet).
async fn check_ports(ready: &Readiness<'_>) -> Result<u16, u16> {
    let bidi = match ready.bidi_port {
        0 => ready.profile.read_port_file().ok().flatten().ok_or(0u16)?,
        port => port,
    };
    if !probe::is_firefox_bidi(ready.host, bidi, PROBE_TIMEOUT).await {
        return Err(bidi);
    }
    if let Some(rdp) = ready.rdp_port
        && !probe::is_firefox_rdp(ready.host, rdp, PROBE_TIMEOUT).await
    {
        return Err(rdp);
    }
    Ok(bidi)
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;

    fn spawn_sh(script: &str) -> FirefoxProcess {
        let child = Command::new("sh").args(["-c", script]).spawn().unwrap();
        FirefoxProcess::new(child, false)
    }

    /// PRC-08: terminate stops a well-behaved process within the grace period.
    #[tokio::test]
    async fn terminate_stops_with_sigterm() {
        let mut process = spawn_sh("exec sleep 30");
        let start = Instant::now();
        process.terminate(Duration::from_secs(5)).await;
        assert!(!process.is_running());
        assert!(
            start.elapsed() < Duration::from_secs(4),
            "SIGTERM, not the kill"
        );
    }

    /// PRC-09: terminate escalates to kill when SIGTERM is ignored.
    #[tokio::test]
    async fn terminate_escalates_to_kill() {
        let mut process = spawn_sh("trap '' TERM; while true; do sleep 1; done");
        tokio::time::sleep(Duration::from_millis(100)).await;
        process.terminate(Duration::from_millis(300)).await;
        assert!(!process.is_running());
    }

    /// PRC-11: terminate is idempotent and a no-op on an exited process.
    #[tokio::test]
    async fn terminate_is_idempotent() {
        let mut process = spawn_sh("exit 0");
        tokio::time::sleep(Duration::from_millis(200)).await;
        process.terminate(Duration::from_secs(1)).await;
        process.terminate(Duration::from_secs(1)).await;
        assert!(!process.is_running());
    }

    /// PRC-14: Dropping a FirefoxProcess kills it unless kept alive.
    #[tokio::test]
    async fn drop_kills_unless_kept_alive() {
        let process = spawn_sh("exec sleep 30");
        let pid = process.id().unwrap();
        drop(process);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!crate::profile::pid_alive(pid) || is_zombie(pid));
        let mut kept = spawn_sh("exec sleep 30");
        kept.keep_alive();
        let pid = kept.id().unwrap();
        drop(kept);
        assert!(crate::profile::pid_alive(pid));
        assert!(request_exit(Some(pid)));
    }

    /// A killed child not yet reaped by tokio still has a PID entry.
    fn is_zombie(pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .map(|stat| stat.contains(") Z "))
            .unwrap_or(true)
    }
}

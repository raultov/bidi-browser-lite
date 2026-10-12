//! Spawning Firefox, waiting for its ports, and terminating it.
//!
//! The spawned PID is Firefox itself, also for snap and Ubuntu's wrapper
//! script (both `exec`, verified in the spike), so no process group is
//! needed. Standard streams are null: a piped stream would kill a Firefox
//! kept alive on drop with SIGPIPE once this process exits. Early exits are
//! classified by exit code instead (see [`EarlyExitReason`]).
//!
//! On Windows killing a process does not kill its children, and `firefox.exe`
//! may run a launcher process before the browser, so the spawned process is
//! put in a Job Object that kills every process of the tree when Firefox is
//! killed. (Processes started in the instant between the spawn and the
//! assignment would escape it; Firefox starts none that early in practice.)

use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, Command};
use tokio::time::Instant;
use tracing::{debug, warn};

use crate::error::{BrowserError, EarlyExitReason};
use crate::probe;
use crate::profile;

/// How often readiness is polled.
const POLL: Duration = Duration::from_millis(200);
/// Upper bound of one probe inside the readiness loop.
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);
/// How often `terminate` checks whether the process exited.
const EXIT_POLL: Duration = Duration::from_millis(50);
/// How long `terminate` waits after the final kill.
const KILL_WAIT: Duration = Duration::from_secs(5);
/// How long dropping a `FirefoxProcess` waits for the killed process to
/// exit (milliseconds in practice), so its profile is never removed under a
/// Firefox that is still writing to it.
const DROP_WAIT: Duration = Duration::from_secs(2);

/// A Firefox child process owned by this library. Dropping it kills the
/// process and waits (bounded) for it to exit, unless it is kept alive.
#[doc(hidden)]
#[derive(Debug)]
pub struct FirefoxProcess {
    child: Child,
    pid: Option<u32>,
    keep_alive: bool,
    tree: tree::Tree,
}

impl FirefoxProcess {
    fn new(child: Child, keep_alive: bool) -> Self {
        let pid = child.id();
        let tree = tree::Tree::attach(&child);
        Self {
            child,
            pid,
            keep_alive,
            tree,
        }
    }

    /// Kills the process and, on Windows, every process it started.
    fn kill(&mut self) {
        let _ = self.child.start_kill();
        self.tree.kill();
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
        self.kill();
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
        if self.keep_alive {
            self.tree.release();
            return;
        }
        if !self.is_running() {
            return;
        }
        debug!(pid = ?self.pid, "killing Firefox on drop");
        self.kill();
        // Synchronous on purpose: drop cannot await, it may run without a
        // runtime, and a killed process exits within milliseconds.
        let deadline = std::time::Instant::now() + DROP_WAIT;
        while self.is_running() && std::time::Instant::now() < deadline {
            std::thread::sleep(EXIT_POLL / 5);
        }
    }
}

/// The process tree of a launched Firefox, as one killable unit.
#[cfg(windows)]
mod tree {
    use tokio::process::Child;
    use win32job::{ExtendedLimitInfo, Job};

    /// A Job Object holding Firefox and everything it starts; closing it
    /// with the kill-on-close limit kills them all.
    pub(super) struct Tree(Option<Job>);

    impl std::fmt::Debug for Tree {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(if self.0.is_some() {
                "Tree(job)"
            } else {
                "Tree(none)"
            })
        }
    }

    impl Tree {
        /// Puts `child` in a new kill-on-close job; without one (the job
        /// could not be created or assigned) only the process is killed.
        pub(super) fn attach(child: &Child) -> Self {
            let job = child.raw_handle().and_then(|handle| {
                let mut info = ExtendedLimitInfo::new();
                info.limit_kill_on_job_close();
                let job = Job::create_with_limit_info(&info).ok()?;
                job.assign_process(handle as isize).ok()?;
                Some(job)
            });
            if job.is_none() {
                tracing::debug!("no Job Object for Firefox; only its main process will be killed");
            }
            Self(job)
        }

        /// Kills every process of the tree by closing the job.
        pub(super) fn kill(&mut self) {
            self.0 = None;
        }

        /// Lets the tree outlive this handle (a browser kept alive on drop).
        pub(super) fn release(&mut self) {
            if let Some(job) = self.0.take() {
                let _ = job.set_extended_limit_info(ExtendedLimitInfo::new().clear_limits());
            }
        }
    }
}

/// On Unix Firefox's children exit with it (it is the process the library
/// signals), so there is nothing extra to track.
#[cfg(not(windows))]
mod tree {
    use tokio::process::Child;

    #[derive(Debug)]
    pub(super) struct Tree;

    impl Tree {
        pub(super) fn attach(_child: &Child) -> Self {
            Self
        }

        pub(super) fn kill(&mut self) {}

        pub(super) fn release(&mut self) {}
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
    /// Where Firefox writes its port file.
    pub(crate) profile_dir: Option<&'a Path>,
    pub(crate) timeout: Duration,
}

/// Starts Firefox. The caller owns the process from here on, so a cancelled
/// launch drops it (killing it) before the profile it was declared after.
pub(crate) fn start(spec: SpawnSpec<'_>) -> Result<FirefoxProcess, BrowserError> {
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
    let process = FirefoxProcess::new(child, false);
    debug!(pid = ?process.id(), path = %spec.path.display(), "Firefox spawned");
    Ok(process)
}

/// Waits until the BiDi port (and the RDP port, if any) answer; returns the
/// real BiDi port. A process that exits first yields `EarlyExit`; one that is
/// not ready in time is killed and yields `StartupTimeout` naming the first
/// port that was not ready.
pub(crate) async fn wait_ready(
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
        0 => ready
            .profile_dir
            .and_then(|dir| profile::read_port_file(dir).ok().flatten())
            .ok_or(0u16)?,
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

    /// PRC-14: Dropping a FirefoxProcess kills it and has reaped it on return, unless kept alive.
    #[tokio::test]
    async fn drop_kills_unless_kept_alive() {
        let process = spawn_sh("exec sleep 30");
        let pid = process.id().unwrap();
        drop(process);
        // Not merely killed: already exited and reaped (no zombie either)
        assert!(
            !crate::profile::pid_alive(pid),
            "the drop must wait for the exit"
        );
        let mut kept = spawn_sh("exec sleep 30");
        kept.keep_alive();
        let pid = kept.id().unwrap();
        drop(kept);
        assert!(crate::profile::pid_alive(pid));
        assert!(request_exit(Some(pid)));
    }

    /// PRC-16: Without a runtime, dropping a FirefoxProcess still waits for the exit.
    #[test]
    fn drop_without_runtime_waits_for_exit() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let process = runtime.block_on(async { spawn_sh("exec sleep 30") });
        let pid = process.id().unwrap();
        drop(runtime);
        drop(process);
        assert!(
            !crate::profile::pid_alive(pid),
            "killed and reaped before returning"
        );
    }
}

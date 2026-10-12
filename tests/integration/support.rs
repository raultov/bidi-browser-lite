//! Shared helpers for the integration tests that spawn `fake_firefox_helper`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::process::{Child, Command};

/// Path of the fake Firefox binary built by Cargo for this test run.
pub fn helper_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fake_firefox_helper"))
}

/// Fixed test ports come from 20000-29999, below the kernel's ephemeral
/// range on Linux, macOS and Windows, so they never collide with the port-0
/// listeners the helpers and the allocator get from the kernel. A counter
/// keeps the tests of this binary apart and spaces their ports by `STRIDE`,
/// so an `Auto` launch searching upwards from a taken port never walks into
/// another test's port; the PID offset keeps concurrent test binaries apart.
const TEST_PORTS: std::ops::Range<u16> = 20000..30000;

const STRIDE: u32 = 50;

static NEXT_PORT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// A fixed port that is free right now and handed to no other test.
pub fn free_port() -> u16 {
    let span = u32::from(TEST_PORTS.end - TEST_PORTS.start);
    let offset = std::process::id() % (span / STRIDE) * STRIDE;
    loop {
        let n = NEXT_PORT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let port = TEST_PORTS.start + u16::try_from((offset + n * STRIDE) % span).unwrap();
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
}

/// `N` distinct fixed ports (see [`free_port`]).
pub fn free_ports<const N: usize>() -> [u16; N] {
    std::array::from_fn(|_| free_port())
}

/// Spawns the helper in `mode` with `args`, killed when dropped.
pub fn spawn_helper(mode: &str, args: &[&str], log: Option<&Path>) -> Child {
    let mut command = Command::new(helper_path());
    command
        .args(args)
        .env("FAKE_FIREFOX_MODE", mode)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(log) = log {
        command.env("FAKE_FIREFOX_ARGS_LOG", log);
    }
    command.spawn().unwrap()
}

/// Polls `check` every 50 ms until it is true or `limit` passes.
pub async fn eventually<F, Fut>(limit: Duration, mut check: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if check().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// A LaunchNew config that runs the fake helper in `mode`, plus a scratch
/// directory whose `pid` file receives the helper's PID.
pub fn helper_config(mode: &str) -> (bidi_browser_lite::BrowserConfigBuilder, tempfile::TempDir) {
    let scratch = tempfile::tempdir().unwrap();
    let builder = bidi_browser_lite::BrowserConfig::builder()
        .mode(bidi_browser_lite::LaunchMode::LaunchNew)
        .firefox_path(helper_path())
        .env_var("FAKE_FIREFOX_MODE", mode)
        .env_var("FAKE_FIREFOX_PID_FILE", scratch.path().join("pid"))
        .env_var("FAKE_FIREFOX_ARGS_LOG", scratch.path().join("args"))
        .connect_timeout(Duration::from_secs(5))
        .command_timeout(Duration::from_secs(5))
        .startup_timeout(Duration::from_secs(10));
    (builder, scratch)
}

/// The PID the helper wrote to its scratch directory.
pub fn helper_pid(scratch: &tempfile::TempDir) -> u32 {
    let path = scratch.path().join("pid");
    for _ in 0..50 {
        if let Ok(content) = std::fs::read_to_string(&path)
            && let Ok(pid) = content.trim().parse()
        {
            return pid;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    std::fs::read_to_string(&path)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// The `--profile` directory the helper was launched with.
#[cfg(unix)]
pub fn helper_profile(scratch: &tempfile::TempDir) -> PathBuf {
    let log = std::fs::read_to_string(scratch.path().join("args")).unwrap();
    let args: Vec<&str> = log.lines().last().unwrap().split('\0').collect();
    let i = args.iter().position(|a| *a == "--profile").unwrap();
    PathBuf::from(args[i + 1])
}

/// Whether `pid` is gone (exited, or a zombie waiting to be reaped).
#[cfg(unix)]
pub fn pid_gone(pid: u32) -> bool {
    let alive = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_ok();
    let zombie = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map(|stat| stat.contains(") Z "))
        .unwrap_or(false);
    !alive || zombie
}

/// Waits until `pid` is gone.
#[cfg(unix)]
pub async fn wait_gone(pid: u32) -> bool {
    eventually(Duration::from_secs(10), || async move { pid_gone(pid) }).await
}

/// `Browser::ensure` with a fresh allocator: tests running in parallel must
/// not share the process-wide reserved set (a port one test picked from the
/// kernel may be the RDP port another test just reserved).
pub async fn ensure(
    config: bidi_browser_lite::BrowserConfig,
) -> Result<bidi_browser_lite::Browser, bidi_browser_lite::BrowserError> {
    bidi_browser_lite::Browser::ensure_with_allocator(
        config,
        bidi_browser_lite::PortAllocator::new(),
    )
    .await
}

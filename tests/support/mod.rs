//! Shared helpers for the integration tests that spawn `fake_firefox_helper`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::process::{Child, Command};

/// Path of the fake Firefox binary built by Cargo for this test run.
pub fn helper_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fake_firefox_helper"))
}

/// A port that was free a moment ago (the kernel picks it).
pub fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.local_addr().unwrap().port()
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

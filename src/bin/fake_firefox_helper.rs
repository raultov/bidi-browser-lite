//! Test double standing in for Firefox (feature `test-support`).
//!
//! It parses the command line the way Firefox does for the flags this crate
//! passes (`--remote-debugging-port`, `--start-debugger-server ws:N`,
//! `--profile DIR`), serves `bidi_browser_lite::test_support` mocks on those
//! ports and reproduces the startup behaviours observed in the Firefox 157
//! spike. `FAKE_FIREFOX_MODE` picks the behaviour:
//!
//! | Mode | Behaviour |
//! | --- | --- |
//! | `serve` (default) | lock the profile, serve BiDi (port 0: write `WebDriverBiDiServer.json`) and RDP if asked; exit 0 on `browser.close` |
//! | `serve_session_busy` | as `serve`, with the session held by a foreign client |
//! | `rdp_silent` | as `serve`, but the RDP port never greets |
//! | `ignore_sigterm` | as `serve`, ignoring SIGTERM (Unix) |
//! | `stubborn` | as `ignore_sigterm`, and `browser.close` does not make it exit |
//! | `handoff` | exit 0 after 200 ms without binding (live-locked profile) |
//! | `port_in_use` | exit 69 after 200 ms without binding (BiDi port taken) |
//! | `exit_immediately` | exit 1 |
//! | `hang_no_port` | stay alive without binding anything |
//!
//! `FAKE_FIREFOX_ARGS_LOG` names a file that gets one line per run: the
//! arguments separated by NUL. `FAKE_FIREFOX_ENV_PROBE` names an environment
//! variable whose presence (never its value) is appended to that log.
//! `FAKE_FIREFOX_PID_FILE` names a file that gets the helper's PID.
//! `FAKE_FIREFOX_DROP_AFTER_MS`: once the first session exists, every BiDi
//! socket is dropped after that many milliseconds, leaving the session
//! orphaned (what a crashed client does to Firefox).

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use bidi_browser_lite::test_support::{MockBidi, MockBidiBehavior, MockRdp};

/// The flags the helper understands.
#[derive(Debug, Default)]
struct Args {
    bidi_port: Option<u16>,
    rdp_port: Option<u16>,
    profile: Option<PathBuf>,
}

fn parse_args(args: &[String]) -> Args {
    let mut parsed = Args::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if let Some(port) = arg.strip_prefix("--remote-debugging-port=") {
            parsed.bidi_port = port.parse().ok();
        } else if arg == "--remote-debugging-port" {
            parsed.bidi_port = iter.next().and_then(|p| p.parse().ok());
        } else if arg == "--start-debugger-server" {
            let value = iter.next().map(String::as_str).unwrap_or_default();
            parsed.rdp_port = value.strip_prefix("ws:").and_then(|p| p.parse().ok());
        } else if arg == "--profile" {
            parsed.profile = iter.next().map(PathBuf::from);
        }
    }
    parsed
}

fn log_invocation(args: &[String]) {
    let Some(log) = std::env::var_os("FAKE_FIREFOX_ARGS_LOG") else {
        return;
    };
    let mut line = args.join("\0");
    if let Some(name) = std::env::var_os("FAKE_FIREFOX_ENV_PROBE") {
        let presence = match std::env::var_os(&name) {
            Some(_) => "present",
            None => "absent",
        };
        line.push_str(&format!("\0env:{}={presence}", name.to_string_lossy()));
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
    {
        let _ = writeln!(file, "{line}");
    }
}

fn write_pid_file() {
    if let Some(path) = std::env::var_os("FAKE_FIREFOX_PID_FILE") {
        let _ = std::fs::write(path, std::process::id().to_string());
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    log_invocation(&args);
    write_pid_file();
    let mode = std::env::var("FAKE_FIREFOX_MODE").unwrap_or_else(|_| "serve".to_owned());
    match mode.as_str() {
        "exit_immediately" => ExitCode::from(1),
        "handoff" => exit_after_delay(0),
        "port_in_use" => exit_after_delay(69),
        "hang_no_port" => loop {
            std::thread::sleep(Duration::from_secs(3600));
        },
        _ => run_serve(&mode, &parse_args(&args)),
    }
}

fn exit_after_delay(code: u8) -> ExitCode {
    std::thread::sleep(Duration::from_millis(200));
    ExitCode::from(code)
}

fn run_serve(mode: &str, args: &Args) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => return ExitCode::from(2),
    };
    match runtime.block_on(serve(mode, args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("fake_firefox_helper: {err}");
            ExitCode::from(3)
        }
    }
}

async fn serve(mode: &str, args: &Args) -> std::io::Result<()> {
    prepare(mode, args)?;
    let requested = args.bidi_port.unwrap_or(0);
    let bidi = Arc::new(MockBidi::start_on(requested, bidi_behavior(mode)).await?);
    let _rdp = start_rdp(mode, args.rdp_port).await?;
    let port_file = args
        .profile
        .as_deref()
        .map(|p| p.join("WebDriverBiDiServer.json"));
    if let (0, Some(path)) = (requested, &port_file) {
        write_port_file(path, bidi.port())?;
    }
    tokio::spawn(orphan_session_later(Arc::clone(&bidi)));
    bidi.wait_browser_closed().await;
    if mode == "stubborn" {
        std::future::pending::<()>().await;
    }
    if let Some(path) = port_file {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

/// Signal handling and the profile lock, before any port is bound.
fn prepare(mode: &str, args: &Args) -> std::io::Result<()> {
    if matches!(mode, "ignore_sigterm" | "stubborn") {
        ignore_sigterm()?;
    }
    match &args.profile {
        Some(profile) => lock_profile(profile),
        None => Ok(()),
    }
}

fn bidi_behavior(mode: &str) -> MockBidiBehavior {
    match mode {
        "serve_session_busy" => MockBidiBehavior::SessionBusy,
        _ => MockBidiBehavior::Ready,
    }
}

async fn start_rdp(mode: &str, port: Option<u16>) -> std::io::Result<Option<MockRdp>> {
    match (port, mode) {
        (Some(port), "rdp_silent") => MockRdp::start_silent_on(port).await.map(Some),
        (Some(port), _) => MockRdp::start_on(port).await.map(Some),
        (None, _) => Ok(None),
    }
}

/// Drops every BiDi socket once, `FAKE_FIREFOX_DROP_AFTER_MS` after the
/// first session appears, so that session becomes an orphan.
async fn orphan_session_later(bidi: Arc<MockBidi>) {
    let Some(delay) = std::env::var("FAKE_FIREFOX_DROP_AFTER_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    else {
        return;
    };
    while !bidi.session_active() {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(delay)).await;
    bidi.drop_connections();
}

fn write_port_file(path: &Path, port: u16) -> std::io::Result<()> {
    let body = format!("{{\n  \"ws_host\": \"127.0.0.1\",\n  \"ws_port\": {port}\n}}");
    std::fs::write(path, body)
}

/// Locks the profile the way Firefox does: a `lock` symlink to
/// `IP:+PID` on Unix, a `parent.lock` file on Windows.
#[cfg(unix)]
fn lock_profile(profile: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(profile)?;
    let lock = profile.join("lock");
    let _ = std::fs::remove_file(&lock);
    std::os::unix::fs::symlink(format!("127.0.0.1:+{}", std::process::id()), lock)?;
    std::fs::write(profile.join(".parentlock"), "")
}

#[cfg(not(unix))]
fn lock_profile(profile: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(profile)?;
    std::fs::write(profile.join("parent.lock"), std::process::id().to_string())
}

/// Swallows SIGTERM so the caller has to escalate to a kill.
#[cfg(unix)]
fn ignore_sigterm() -> std::io::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut terms = signal(SignalKind::terminate())?;
    tokio::spawn(async move { while terms.recv().await.is_some() {} });
    Ok(())
}

#[cfg(not(unix))]
fn ignore_sigterm() -> std::io::Result<()> {
    Ok(())
}

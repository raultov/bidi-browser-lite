//! FKH: the fake Firefox helper behaves like the Firefox 157 observed in the
//! spike, so the process and browser tests can rely on it.

#[cfg(test)]
mod support;

use std::time::Duration;

use bidi_browser_lite::probe;
use bidi_lite::browser::BrowserClient;
use support::{eventually, free_port, spawn_helper};

const HOST: &str = "127.0.0.1";
const T: Duration = Duration::from_secs(10);
const PROBE: Duration = Duration::from_millis(500);

async fn bidi_ready(port: u16) -> bool {
    eventually(T, || probe::is_firefox_bidi(HOST, port, PROBE)).await
}

/// FKH-01: serve binds the BiDi port given on the command line and answers session.status.
#[tokio::test]
async fn serve_binds_given_bidi_port() {
    let port = free_port();
    let _child = spawn_helper("serve", &[&format!("--remote-debugging-port={port}")], None);
    assert!(bidi_ready(port).await);
}

/// FKH-02: serve with port 0 writes WebDriverBiDiServer.json with the bound port.
#[tokio::test]
async fn port_zero_writes_port_file() {
    // Given a profile and port 0
    let profile = tempfile::tempdir().unwrap();
    let dir = profile.path().to_str().unwrap();
    let _child = spawn_helper(
        "serve",
        &["--remote-debugging-port=0", "--profile", dir],
        None,
    );
    // When the port file appears
    let file = profile.path().join("WebDriverBiDiServer.json");
    assert!(eventually(T, || async { file.exists() }).await);
    // Then it names a port that answers BiDi
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let port = u16::try_from(json["ws_port"].as_u64().unwrap()).unwrap();
    assert_eq!(json["ws_host"], "127.0.0.1");
    assert!(bidi_ready(port).await);
}

/// FKH-03: serve creates the profile lock naming its own pid.
#[tokio::test]
async fn serve_locks_profile_with_its_pid() {
    let profile = tempfile::tempdir().unwrap();
    let port = free_port();
    let child = spawn_helper(
        "serve",
        &[
            &format!("--remote-debugging-port={port}"),
            "--profile",
            profile.path().to_str().unwrap(),
        ],
        None,
    );
    assert!(bidi_ready(port).await);
    let pid = child.id().unwrap();
    if cfg!(unix) {
        let target = std::fs::read_link(profile.path().join("lock")).unwrap();
        assert_eq!(target.to_string_lossy(), format!("127.0.0.1:+{pid}"));
    } else {
        let owner = std::fs::read_to_string(profile.path().join("parent.lock")).unwrap();
        assert_eq!(owner, pid.to_string());
    }
}

/// FKH-04: handoff exits 0 and port_in_use exits 69 without binding.
#[tokio::test]
async fn handoff_and_port_in_use_exit_codes() {
    for (mode, code) in [("handoff", 0), ("port_in_use", 69)] {
        let port = free_port();
        let mut child = spawn_helper(mode, &[&format!("--remote-debugging-port={port}")], None);
        let status = tokio::time::timeout(T, child.wait())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status.code(), Some(code), "{mode}");
        assert!(
            !probe::is_port_open(HOST, port, PROBE).await,
            "{mode} must not bind"
        );
    }
}

/// FKH-05: serve opens RDP only when --start-debugger-server is given.
#[tokio::test]
async fn rdp_only_when_asked() {
    let (bidi, rdp) = (free_port(), free_port());
    let _plain = spawn_helper("serve", &[&format!("--remote-debugging-port={bidi}")], None);
    assert!(bidi_ready(bidi).await);
    assert!(!probe::is_port_open(HOST, rdp, PROBE).await);
    let bidi2 = free_port();
    let _with = spawn_helper(
        "serve",
        &[
            &format!("--remote-debugging-port={bidi2}"),
            "--start-debugger-server",
            &format!("ws:{rdp}"),
        ],
        None,
    );
    assert!(eventually(T, || probe::is_firefox_rdp(HOST, rdp, PROBE)).await);
}

/// FKH-06: browser.close makes the helper exit 0.
#[tokio::test]
async fn browser_close_exits_zero() {
    let port = free_port();
    let mut child = spawn_helper("serve", &[&format!("--remote-debugging-port={port}")], None);
    assert!(bidi_ready(port).await);
    let client = BrowserClient::connect(&format!("{HOST}:{port}"), T)
        .await
        .unwrap();
    client.close_browser().await.unwrap();
    let status = tokio::time::timeout(T, child.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.code(), Some(0));
}

/// FKH-07: argv is logged exactly to FAKE_FIREFOX_ARGS_LOG.
#[tokio::test]
async fn argv_is_logged() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("args.log");
    let mut child = spawn_helper("exit_immediately", &["-a", "b c", "--x=1"], Some(&log));
    let status = tokio::time::timeout(T, child.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.code(), Some(1));
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "-a\0b c\0--x=1\n");
}

/// FKH-08: ignore_sigterm survives SIGTERM.
#[cfg(unix)]
#[tokio::test]
async fn ignore_sigterm_survives_sigterm() {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    let port = free_port();
    let mut child = spawn_helper(
        "ignore_sigterm",
        &[&format!("--remote-debugging-port={port}")],
        None,
    );
    assert!(bidi_ready(port).await);
    let pid = Pid::from_raw(i32::try_from(child.id().unwrap()).unwrap());
    kill(pid, Signal::SIGTERM).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        child.try_wait().unwrap().is_none(),
        "SIGTERM must be ignored"
    );
}

/// FKH-09: rdp_silent accepts RDP connections but never greets.
#[tokio::test]
async fn rdp_silent_never_greets() {
    let (bidi, rdp) = (free_port(), free_port());
    let _child = spawn_helper(
        "rdp_silent",
        &[
            &format!("--remote-debugging-port={bidi}"),
            "--start-debugger-server",
            &format!("ws:{rdp}"),
        ],
        None,
    );
    assert!(bidi_ready(bidi).await);
    assert!(probe::is_port_open(HOST, rdp, PROBE).await);
    assert!(!probe::is_firefox_rdp(HOST, rdp, PROBE).await);
}

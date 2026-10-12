//! BRW: launch, session ownership, stop, restart, relaunch and drop of a
//! managed Firefox (the fake helper).

use std::sync::Arc;
use std::time::Duration;

use crate::support::{free_port, helper_config, helper_pid, helper_profile};
use bidi_browser_lite::{Browser, BrowserClient, LaunchMode, PortAllocator, ProfileMode, probe};

const HOST: &str = "127.0.0.1";
const PROBE: Duration = Duration::from_millis(500);

/// BRW-03: LaunchNew on a fixed port launches a managed instance.
#[tokio::test]
async fn launch_new_fixed_port_is_managed() {
    let port = free_port();
    let (builder, scratch) = helper_config("serve");
    let browser = crate::support::ensure(builder.port(port).build())
        .await
        .unwrap();
    assert!(browser.is_managed().await && browser.is_alive().await);
    assert_eq!(browser.pid().await, Some(helper_pid(&scratch)));
    let dir = browser.profile_dir().await.unwrap();
    assert!(dir.join("user.js").exists());
    browser.stop().await.unwrap();
}

/// BRW-04: LaunchNew on port 0 launches and debug_address reports the real port.
#[tokio::test]
async fn launch_new_port_zero_reports_real_port() {
    let (builder, _scratch) = helper_config("serve");
    let browser = crate::support::ensure(builder.build()).await.unwrap();
    let (host, port) = browser.debug_address().await;
    assert_eq!(host, HOST);
    assert!(port != 0 && probe::is_firefox_bidi(HOST, port, PROBE).await);
    browser.stop().await.unwrap();
}

/// BRW-11: A managed Firefox with an orphan session is restarted even with auto_relaunch off.
#[tokio::test]
async fn managed_orphan_session_is_recovered_by_restart() {
    // Given a managed Firefox that orphans its session 300 ms after it starts
    let (builder, scratch) = helper_config("serve");
    let config = builder.env_var("FAKE_FIREFOX_DROP_AFTER_MS", "300").build();
    let browser = crate::support::ensure(config).await.unwrap();
    let first = helper_pid(&scratch);
    let client = browser.client().await.unwrap();
    assert!(
        crate::support::eventually(Duration::from_secs(5), || async { !client.is_connected() })
            .await
    );
    // When a client is requested
    browser.list_tabs().await.unwrap();
    // Then the process was restarted and a new session opened
    assert_ne!(browser.pid().await, Some(first));
    browser.stop().await.unwrap();
}

/// BRW-14: stop on a managed instance sends browser.close and waits for a clean exit.
#[cfg(unix)]
#[tokio::test]
async fn stop_managed_closes_browser_cleanly() {
    // Given a managed Firefox on a persistent profile
    let dir = tempfile::tempdir().unwrap();
    let (builder, scratch) = helper_config("serve");
    let config = builder
        .profile(ProfileMode::Persistent(dir.path().to_path_buf()))
        .build();
    let browser = crate::support::ensure(config).await.unwrap();
    assert!(dir.path().join("WebDriverBiDiServer.json").exists());
    // When it is stopped
    browser.stop().await.unwrap();
    // Then it exited through browser.close (only a clean exit removes the port file)
    assert!(crate::support::pid_gone(helper_pid(&scratch)));
    assert!(
        !dir.path().join("WebDriverBiDiServer.json").exists(),
        "exited by browser.close, not a signal"
    );
    assert!(
        dir.path().join("user.js").exists(),
        "persistent profile kept"
    );
    assert_eq!(browser.pid().await, None);
}

/// BRW-35: stop removes an ephemeral profile.
#[tokio::test]
async fn stop_removes_ephemeral_profile() {
    let (builder, _scratch) = helper_config("serve");
    let browser = crate::support::ensure(builder.build()).await.unwrap();
    let dir = browser.profile_dir().await.unwrap();
    browser.stop().await.unwrap();
    assert!(!dir.exists());
}

/// BRW-15: stop escalates to kill when the process ignores browser.close and SIGTERM.
#[cfg(unix)]
#[tokio::test]
async fn stop_escalates_to_kill() {
    let (builder, scratch) = helper_config("stubborn");
    let browser = crate::support::ensure(builder.build()).await.unwrap();
    browser.stop().await.unwrap();
    assert!(crate::support::wait_gone(helper_pid(&scratch)).await);
}

/// BRW-18: restart on a managed instance yields a new pid, the same fixed port and a new session.
#[tokio::test]
async fn restart_managed_keeps_fixed_port() {
    let port = free_port();
    let (builder, _scratch) = helper_config("serve");
    let browser = crate::support::ensure(builder.port(port).build())
        .await
        .unwrap();
    let before = browser.pid().await;
    browser.restart().await.unwrap();
    assert_ne!(browser.pid().await, before);
    assert_eq!(browser.debug_address().await.1, port);
    browser.list_tabs().await.unwrap();
    browser.stop().await.unwrap();
}

/// BRW-20: A dead managed process with auto_relaunch off yields Stopped.
#[cfg(unix)]
#[tokio::test]
async fn dead_process_without_relaunch_is_stopped() {
    let (builder, scratch) = helper_config("serve");
    let browser = crate::support::ensure(builder.build()).await.unwrap();
    kill9(helper_pid(&scratch));
    assert!(
        crate::support::eventually(Duration::from_secs(5), || async {
            !browser.is_alive().await
        })
        .await
    );
    assert!(matches!(
        browser.client().await,
        Err(bidi_browser_lite::BrowserError::Stopped)
    ));
}

/// BRW-21: A dead managed process with auto_relaunch on is relaunched on the next client call.
#[cfg(unix)]
#[tokio::test]
async fn dead_process_with_relaunch_is_relaunched() {
    let (builder, scratch) = helper_config("serve");
    let browser = crate::support::ensure(builder.auto_relaunch(true).build())
        .await
        .unwrap();
    let first = helper_pid(&scratch);
    kill9(first);
    assert!(
        crate::support::eventually(Duration::from_secs(5), || async {
            !browser.is_alive().await
        })
        .await
    );
    browser.list_tabs().await.unwrap();
    assert_ne!(browser.pid().await, Some(first));
    browser.stop().await.unwrap();
}

/// BRW-22: Dropping a managed Browser kills the process and removes the ephemeral profile.
#[cfg(unix)]
#[tokio::test]
async fn drop_managed_kills_and_cleans() {
    let (builder, scratch) = helper_config("serve");
    let browser = crate::support::ensure(builder.build()).await.unwrap();
    let dir = browser.profile_dir().await.unwrap();
    drop(browser);
    assert!(crate::support::wait_gone(helper_pid(&scratch)).await);
    assert!(!dir.exists());
}

/// BRW-23: keep_alive_on_drop leaves the process running and ends the session best effort.
#[tokio::test]
async fn keep_alive_on_drop_leaves_process_running() {
    let (builder, _scratch) = helper_config("serve");
    let browser = crate::support::ensure(builder.keep_alive_on_drop(true).build())
        .await
        .unwrap();
    let (_, port) = browser.debug_address().await;
    let dir = browser.profile_dir().await.unwrap();
    drop(browser);
    // The session is freed in the background and the process keeps serving
    let free = crate::support::eventually(Duration::from_secs(5), || async {
        probe::bidi_session_available(HOST, port, PROBE)
            .await
            .unwrap_or(false)
    });
    assert!(free.await);
    assert!(dir.exists(), "a live browser keeps its profile");
    // Cleanup: close it the BiDi way
    let client = BrowserClient::connect(&format!("{HOST}:{port}"), Duration::from_secs(5))
        .await
        .unwrap();
    client.close_browser().await.unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

/// BRW-29: A cancelled ensure leaves no process, reservation or ephemeral directory.
#[cfg(unix)]
#[tokio::test]
async fn cancelled_ensure_leaves_nothing_behind() {
    // Given a launch that will hang and an RDP port reservation
    let allocator = PortAllocator::new();
    let port = free_port();
    let (builder, scratch) = helper_config("hang_no_port");
    let config = builder.port(port).rdp(true).build();
    // When ensure is cancelled half-way
    let ensure = Browser::ensure_with_allocator(config, Arc::clone(&allocator));
    assert!(
        tokio::time::timeout(Duration::from_millis(800), ensure)
            .await
            .is_err()
    );
    // Then the child, the reservation and the profile are gone
    assert!(crate::support::wait_gone(helper_pid(&scratch)).await);
    assert!(!allocator.is_reserved(port));
    assert!(!helper_profile(&scratch).exists());
}

/// BRW-30: Auto with PersistentPerPort next to a live managed instance launches on another port with its own profile.
#[cfg(unix)]
#[tokio::test]
async fn auto_per_port_skips_live_instance() {
    let root = tempfile::tempdir().unwrap();
    let port = free_port();
    let per_port = || ProfileMode::PersistentPerPort {
        root: root.path().to_path_buf(),
        prefix: "ff-".into(),
    };
    let (a, _sa) = helper_config("serve");
    let first = crate::support::ensure(
        a.mode(LaunchMode::Auto)
            .port(port)
            .profile(per_port())
            .build(),
    )
    .await
    .unwrap();
    let (b, _sb) = helper_config("serve");
    let second = crate::support::ensure(
        b.mode(LaunchMode::Auto)
            .port(port)
            .profile(per_port())
            .build(),
    )
    .await
    .unwrap();
    assert!(
        second.is_managed().await,
        "must not attach to the other manager's Firefox"
    );
    assert_ne!(second.debug_address().await.1, port);
    assert_ne!(second.profile_dir().await, first.profile_dir().await);
    second.stop().await.unwrap();
    first.stop().await.unwrap();
    assert!(
        root.path().join(format!("ff-{port}")).exists(),
        "persistent dirs survive stop"
    );
}

/// BRW-33: Auto on a port held by a non-BiDi server launches on a nearby reserved port.
#[tokio::test]
async fn auto_on_foreign_port_launches_nearby() {
    let foreign = std::net::TcpListener::bind((HOST, 0)).unwrap();
    let port = foreign.local_addr().unwrap().port();
    let (builder, _scratch) = helper_config("serve");
    let config = builder
        .mode(LaunchMode::Auto)
        .port(port)
        .connect_timeout(PROBE)
        .build();
    let browser = crate::support::ensure(config).await.unwrap();
    assert!(browser.is_managed().await);
    assert_ne!(browser.debug_address().await.1, port);
    browser.stop().await.unwrap();
    drop(foreign);
}

#[cfg(unix)]
fn kill9(pid: u32) {
    use nix::sys::signal::{Signal, kill};
    kill(nix::unistd::Pid::from_raw(pid as i32), Signal::SIGKILL).unwrap();
}

//! BRW: launch, session ownership, stop, restart, relaunch and drop of a
//! managed Firefox (the fake helper).

use std::time::Duration;

use crate::support::{free_port, helper_config, helper_pid};
use bidi_browser_lite::{BrowserClient, LaunchMode, probe};

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

/// BRW-35: stop removes an ephemeral profile.
#[tokio::test]
async fn stop_removes_ephemeral_profile() {
    let (builder, _scratch) = helper_config("serve");
    let browser = crate::support::ensure(builder.build()).await.unwrap();
    let dir = browser.profile_dir().await.unwrap();
    browser.stop().await.unwrap();
    assert!(!dir.exists());
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

//! RFX: the lifecycle against a real Firefox. `#[ignore]`d: run with
//! `make test-real-firefox` (the CI `firefox` job runs them headless with
//! `BIDI_BROWSER_LITE_HEADLESS=1`). Never part of `make check`.

use std::time::Duration;

use bidi_browser_lite::{
    Browser, BrowserConfig, BrowserConfigBuilder, BrowserError, LaunchMode, ProfileError,
    ProfileMode, ReadinessState, discovery, probe,
};
use serde_json::json;

const HOST: &str = "127.0.0.1";

fn launch() -> BrowserConfigBuilder {
    let headless = std::env::var_os("BIDI_BROWSER_LITE_HEADLESS").is_some();
    BrowserConfig::builder()
        .mode(LaunchMode::LaunchNew)
        .headless(headless)
        .startup_timeout(Duration::from_secs(30))
        .command_timeout(Duration::from_secs(30))
}

/// A scratch directory Firefox can see: snap Firefox cannot see the host
/// `/tmp`, so it goes under `~/snap/firefox/common` when that exists.
fn profile_parent() -> std::io::Result<tempfile::TempDir> {
    let snap = std::env::var_os("HOME")
        .map(|home| std::path::PathBuf::from(home).join("snap/firefox/common/bidi-browser-lite"))
        .filter(|dir| dir.parent().is_some_and(std::path::Path::is_dir));
    match snap {
        Some(dir) => {
            std::fs::create_dir_all(&dir)?;
            tempfile::tempdir_in(dir)
        }
        None => tempfile::tempdir(),
    }
}

/// RFX-01: discover_default finds the installed Firefox.
#[test]
#[ignore = "requires a real Firefox"]
fn discovers_installed_firefox() {
    let path = discovery::discover_default().unwrap();
    assert!(path.exists(), "{path:?}");
}

/// RFX-02: LaunchNew port 0 -> new_tab -> navigate -> stop leaves no process and no profile dir.
#[tokio::test]
#[ignore = "requires a real Firefox"]
async fn launch_navigate_stop_leaves_nothing() {
    let browser = Browser::ensure(launch().build()).await.unwrap();
    let (_, port) = browser.debug_address().await;
    let dir = browser.profile_dir().await.unwrap();
    let tab = browser.new_tab("about:blank").await.unwrap();
    tab.navigate("about:blank", ReadinessState::Complete)
        .await
        .unwrap();
    let params =
        json!({"expression": "1 + 1", "target": tab.script_target(), "awaitPromise": false});
    let reply = tab
        .send_raw_command("script.evaluate", params)
        .await
        .unwrap();
    assert_eq!(reply.result.unwrap()["result"]["value"], 2);
    browser.stop().await.unwrap();
    assert!(!probe::is_port_open(HOST, port, Duration::from_millis(500)).await);
    assert!(!dir.exists(), "ephemeral profile removed");
}

/// RFX-03: RDP enabled: rdp_client greets and listTabs returns the tab.
#[tokio::test]
#[ignore = "requires a real Firefox"]
async fn rdp_client_lists_tabs() {
    let browser = Browser::ensure(launch().rdp(true).build()).await.unwrap();
    let rdp = browser.rdp_client().await.unwrap();
    let tabs = rdp.request("root", "listTabs", json!({})).await.unwrap();
    assert!(
        tabs.body["tabs"].as_array().is_some_and(|t| !t.is_empty()),
        "{:?}",
        tabs.body
    );
    browser.stop().await.unwrap();
}

/// RFX-04: Two managed instances run side by side on distinct ports and profiles.
#[tokio::test]
#[ignore = "requires a real Firefox"]
async fn two_instances_side_by_side() {
    let (a, b) = tokio::join!(
        Browser::ensure(launch().build()),
        Browser::ensure(launch().build())
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_ne!(a.debug_address().await.1, b.debug_address().await.1);
    assert_ne!(a.profile_dir().await, b.profile_dir().await);
    a.list_tabs().await.unwrap();
    b.list_tabs().await.unwrap();
    a.stop().await.unwrap();
    b.stop().await.unwrap();
}

/// RFX-05: A session orphaned on a real Firefox makes an attaching Browser fail with SessionBusy,
/// and the managed owner recovers by restarting Firefox.
#[test]
#[ignore = "requires a real Firefox"]
fn orphaned_session_busy_then_recovered() {
    // Given a managed Firefox whose session connection dies with its runtime
    let first = tokio::runtime::Runtime::new().unwrap();
    let browser = first.block_on(Browser::ensure(launch().build())).unwrap();
    let (_, port) = first.block_on(browser.debug_address());
    drop(first);
    let second = tokio::runtime::Runtime::new().unwrap();
    second.block_on(async {
        // When another Browser attaches, the orphan makes it busy
        let attach = BrowserConfig::builder()
            .mode(LaunchMode::AttachOnly)
            .port(port)
            .build();
        let busy = Browser::ensure(attach).await;
        assert!(
            matches!(busy, Err(BrowserError::SessionBusy { .. })),
            "{busy:?}"
        );
        // And the managed owner recovers by restarting its Firefox
        browser.list_tabs().await.unwrap();
        browser.stop().await.unwrap();
    });
}

/// RFX-06: restart yields a working new session.
#[tokio::test]
#[ignore = "requires a real Firefox"]
async fn restart_yields_working_session() {
    let browser = Browser::ensure(launch().build()).await.unwrap();
    let before = browser.pid().await;
    browser.restart().await.unwrap();
    assert_ne!(browser.pid().await, before);
    browser.new_tab("about:blank").await.unwrap();
    browser.stop().await.unwrap();
}

/// RFX-07: A persistent profile in use by a running Firefox is refused before launching.
#[tokio::test]
#[ignore = "requires a real Firefox"]
async fn profile_in_use_is_refused() {
    // Given a Firefox running on a persistent profile
    let dir = profile_parent().unwrap();
    let profile = || ProfileMode::Persistent(dir.path().to_path_buf());
    let first = Browser::ensure(launch().profile(profile()).build())
        .await
        .unwrap();
    // When a second launch targets the same profile
    let second = Browser::ensure(launch().profile(profile()).build()).await;
    // Then it is refused instead of being handed to the running Firefox
    assert!(
        matches!(
            second,
            Err(BrowserError::Profile(ProfileError::Locked { .. }))
        ),
        "{second:?}"
    );
    first.stop().await.unwrap();
    // And once that Firefox is gone the profile can be launched again
    let again = Browser::ensure(launch().profile(profile()).build())
        .await
        .unwrap();
    again.stop().await.unwrap();
}

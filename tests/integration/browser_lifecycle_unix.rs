//! Lifecycle tests that need Unix process control (SIGKILL from outside,
//! reaping checks): kept in their own module so its imports exist only
//! where the tests do.

use std::sync::Arc;
use std::time::Duration;

use crate::support::{free_port, helper_config, helper_pid, helper_profile};
use bidi_browser_lite::{Browser, LaunchMode, PortAllocator, ProfileMode};

/// BRW-14: stop on a managed instance sends browser.close and waits for a clean exit.
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

/// BRW-15: stop escalates to kill when the process ignores browser.close and SIGTERM.
#[tokio::test]
async fn stop_escalates_to_kill() {
    let (builder, scratch) = helper_config("stubborn");
    let browser = crate::support::ensure(builder.build()).await.unwrap();
    browser.stop().await.unwrap();
    assert!(crate::support::wait_gone(helper_pid(&scratch)).await);
}

/// BRW-20: A dead managed process with auto_relaunch off yields Stopped.
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
#[tokio::test]
async fn drop_managed_kills_and_cleans() {
    let (builder, scratch) = helper_config("serve");
    let browser = crate::support::ensure(builder.build()).await.unwrap();
    let dir = browser.profile_dir().await.unwrap();
    drop(browser);
    assert!(crate::support::wait_gone(helper_pid(&scratch)).await);
    assert!(!dir.exists());
}

/// BRW-29: A cancelled ensure leaves no process, reservation or ephemeral directory.
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
#[tokio::test]
async fn auto_per_port_skips_live_instance() {
    let root = tempfile::tempdir().unwrap();
    let port = free_port();
    let (first, _first_logs) = launch_auto_per_port(root.path(), port).await;
    let (second, _second_logs) = launch_auto_per_port(root.path(), port).await;
    assert!(
        second.is_managed().await,
        "must not attach to the other manager's Firefox"
    );
    let first_alive = first.is_alive().await;
    assert_ne!(
        second.debug_address().await.1,
        port,
        "first on {:?}, alive: {first_alive}",
        first.debug_address().await
    );
    assert_ne!(second.profile_dir().await, first.profile_dir().await);
    second.stop().await.unwrap();
    first.stop().await.unwrap();
    assert!(
        root.path().join(format!("ff-{port}")).exists(),
        "persistent dirs survive stop"
    );
}

/// An `Auto` launch of the fake helper on `port` with per-port profiles
/// under `root`, plus the scratch dir holding the helper's logs.
async fn launch_auto_per_port(root: &std::path::Path, port: u16) -> (Browser, tempfile::TempDir) {
    let (builder, scratch) = helper_config("serve");
    let config = builder
        .mode(LaunchMode::Auto)
        .port(port)
        .profile(ProfileMode::PersistentPerPort {
            root: root.to_path_buf(),
            prefix: "ff-".into(),
        })
        .build();
    (crate::support::ensure(config).await.unwrap(), scratch)
}

fn kill9(pid: u32) {
    use nix::sys::signal::{Signal, kill};
    kill(nix::unistd::Pid::from_raw(pid as i32), Signal::SIGKILL).unwrap();
}

/// PRC-15: A StartupTimeout leaves no child process behind.
#[tokio::test]
async fn startup_timeout_leaves_no_process() {
    let (builder, scratch) = helper_config("hang_no_port");
    let config = builder
        .port(free_port())
        .startup_timeout(Duration::from_millis(800))
        .build();
    assert!(crate::support::ensure(config).await.is_err());
    assert!(crate::support::wait_gone(helper_pid(&scratch)).await);
}

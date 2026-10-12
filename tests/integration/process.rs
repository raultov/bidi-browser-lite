//! PRC: spawning Firefox (the fake helper) and waiting for its ports,
//! through the public `Browser::ensure`.

use std::time::Duration;

use crate::support::{free_port, helper_config};
use bidi_browser_lite::{BrowserError, EarlyExitReason, probe};

const HOST: &str = "127.0.0.1";
const PROBE: Duration = Duration::from_millis(500);

/// PRC-01: spawn with a fixed BiDi port waits until the helper answers BiDi.
#[tokio::test]
async fn fixed_port_launch_waits_for_bidi() {
    let port = free_port();
    let (builder, _scratch) = helper_config("serve");
    let browser = crate::support::ensure(builder.port(port).build())
        .await
        .unwrap();
    assert_eq!(browser.debug_address().await.1, port);
    assert!(probe::is_firefox_bidi(HOST, port, PROBE).await);
    browser.stop().await.unwrap();
}

/// PRC-02: spawn with RDP waits for both ports.
#[tokio::test]
async fn rdp_launch_waits_for_both_ports() {
    let (builder, _scratch) = helper_config("serve");
    let browser = crate::support::ensure(builder.rdp(true).build())
        .await
        .unwrap();
    let (_, rdp) = browser.rdp_address().await.unwrap();
    assert!(probe::is_firefox_rdp(HOST, rdp, PROBE).await);
    browser.rdp_client().await.unwrap();
    browser.stop().await.unwrap();
}

/// PRC-03: spawn with BiDi port 0 reads the port file.
#[tokio::test]
async fn port_zero_reads_port_file() {
    let (builder, _scratch) = helper_config("serve");
    let browser = crate::support::ensure(builder.build()).await.unwrap();
    let (_, port) = browser.debug_address().await;
    assert_ne!(port, 0);
    assert!(probe::is_firefox_bidi(HOST, port, PROBE).await);
    browser.stop().await.unwrap();
}

/// PRC-04: A helper that exits immediately yields EarlyExit with its exit code.
#[tokio::test]
async fn immediate_exit_is_early_exit() {
    let (builder, _scratch) = helper_config("exit_immediately");
    let result = crate::support::ensure(builder.build()).await;
    assert!(
        matches!(
            result,
            Err(BrowserError::EarlyExit {
                code: Some(1),
                reason: EarlyExitReason::Unknown
            })
        ),
        "{result:?}"
    );
}

/// PRC-05: Exit 0 before ready yields ProfileInUse; exit 69 yields PortInUse.
#[tokio::test]
async fn early_exit_codes_are_classified() {
    for (mode, expected) in [
        ("handoff", EarlyExitReason::ProfileInUse),
        ("port_in_use", EarlyExitReason::PortInUse),
    ] {
        let (builder, _scratch) = helper_config(mode);
        let result = crate::support::ensure(builder.build()).await;
        assert!(
            matches!(result, Err(BrowserError::EarlyExit { reason, .. }) if reason == expected),
            "{mode}: {result:?}"
        );
    }
}

/// PRC-06: A helper that never opens its ports yields StartupTimeout naming the BiDi port.
#[tokio::test]
async fn hang_yields_startup_timeout_on_bidi_port() {
    let port = free_port();
    let (builder, _scratch) = helper_config("hang_no_port");
    let config = builder
        .port(port)
        .startup_timeout(Duration::from_millis(800))
        .build();
    let result = crate::support::ensure(config).await;
    assert!(
        matches!(result, Err(BrowserError::StartupTimeout { port: p, .. }) if p == port),
        "{result:?}"
    );
}

/// PRC-07: A helper whose RDP port never greets yields StartupTimeout naming the RDP port.
#[tokio::test]
async fn silent_rdp_yields_startup_timeout_on_rdp_port() {
    let [bidi, rdp] = crate::support::free_ports();
    let (builder, _scratch) = helper_config("rdp_silent");
    let config = builder
        .port(bidi)
        .rdp_port(rdp)
        .startup_timeout(Duration::from_millis(1500))
        .build();
    let result = crate::support::ensure(config).await;
    assert!(
        matches!(result, Err(BrowserError::StartupTimeout { port, .. }) if port == rdp),
        "{result:?}"
    );
}

/// PRC-12: A bad executable path yields SpawnFailed with the path.
#[tokio::test]
async fn bad_path_is_spawn_failed() {
    let (builder, _scratch) = helper_config("serve");
    let config = builder.firefox_path("/nonexistent/firefox").build();
    let result = crate::support::ensure(config).await;
    assert!(
        matches!(&result, Err(BrowserError::SpawnFailed { path, .. }) if path.ends_with("firefox")),
        "{result:?}"
    );
}

/// PRC-13: Configured env vars reach the child, and the arguments are Firefox's.
#[tokio::test]
async fn env_vars_and_args_reach_child() {
    let (builder, scratch) = helper_config("serve");
    let config = builder
        .env_var("FAKE_FIREFOX_ENV_PROBE", "MY_TOKEN")
        .env_var("MY_TOKEN", "s3cr3t")
        .headless(true)
        .build();
    let browser = crate::support::ensure(config).await.unwrap();
    let log = std::fs::read_to_string(scratch.path().join("args")).unwrap();
    assert!(log.contains("env:MY_TOKEN=present"), "{log:?}");
    for arg in [
        "--remote-debugging-port=0",
        "--no-remote",
        "--profile",
        "--headless",
    ] {
        assert!(
            log.split(['\0', '\n']).any(|a| a == arg),
            "{arg} missing in {log:?}"
        );
    }
    browser.stop().await.unwrap();
}

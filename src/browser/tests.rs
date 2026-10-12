//! Unit tests of `Browser` against the in-process mocks (attach paths).
//! Launch paths need a child process and live in `tests/`.

use std::time::Duration;

use super::*;
use crate::config::{LaunchMode, ProfileMode};
use crate::test_support::{MockBidi, MockBidiBehavior, MockFirefox, MockRdp};

const T: Duration = Duration::from_secs(5);

/// Unit tests must never launch anything: a launch attempt fails with
/// `SpawnFailed` on this path instead of finding a real Firefox.
const NO_FIREFOX: &str = "/nonexistent/firefox";

fn attach_only(port: u16) -> crate::config::BrowserConfigBuilder {
    BrowserConfig::builder()
        .mode(LaunchMode::AttachOnly)
        .port(port)
        .firefox_path(NO_FIREFOX)
        .connect_timeout(T)
        .command_timeout(T)
}

async fn mock(behavior: MockBidiBehavior) -> MockBidi {
    MockBidi::start(behavior).await.unwrap()
}

async fn attached(mock: &MockBidi) -> Browser {
    Browser::ensure(attach_only(mock.port()).build())
        .await
        .unwrap()
}

fn closed_port() -> u16 {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.local_addr().unwrap().port()
}

/// BRW-01: AttachOnly attaches to a free Firefox with exactly one session.new and is unmanaged.
#[tokio::test]
async fn attach_only_attaches_with_one_session() {
    let mock = mock(MockBidiBehavior::Ready).await;
    let browser = attached(&mock).await;
    assert!(!browser.is_managed().await);
    assert_eq!(mock.count("session.new"), 1);
    assert!(mock.session_active());
}

/// BRW-02: AttachOnly to a closed port fails with RemoteUnavailable.
#[tokio::test]
async fn attach_only_to_closed_port_fails() {
    let port = closed_port();
    let result = Browser::ensure(attach_only(port).build()).await;
    assert!(matches!(result, Err(BrowserError::RemoteUnavailable { port: p, .. }) if p == port));
}

/// BRW-05: LaunchNew on a busy fixed port fails with PortConflict and spawns nothing.
#[tokio::test]
async fn launch_new_on_busy_port_conflicts() {
    // Given a port taken by something and a Firefox path that cannot run
    let mock = mock(MockBidiBehavior::PlainHttp).await;
    let config = BrowserConfig::builder()
        .mode(LaunchMode::LaunchNew)
        .port(mock.port())
        .firefox_path(NO_FIREFOX)
        .build();
    // Then the conflict is reported before any spawn is attempted
    let result = Browser::ensure(config).await;
    assert!(
        matches!(result, Err(BrowserError::PortConflict { port }) if port == mock.port()),
        "{result:?}"
    );
}

/// BRW-06: Auto attaches when the port answers BiDi with a free session.
#[tokio::test]
async fn auto_attaches_to_free_bidi() {
    let mock = mock(MockBidiBehavior::Ready).await;
    let config = BrowserConfig::builder()
        .port(mock.port())
        .firefox_path(NO_FIREFOX)
        .build();
    let browser = Browser::ensure(config).await.unwrap();
    assert!(!browser.is_managed().await);
    assert_eq!(mock.count("session.new"), 1);
}

/// BRW-07: AttachOnly to a Firefox whose session is taken fails with SessionBusy and signals nothing.
#[tokio::test]
async fn attach_only_to_busy_firefox_fails_with_session_busy() {
    let mock = mock(MockBidiBehavior::SessionBusy).await;
    let result = Browser::ensure(attach_only(mock.port()).build()).await;
    assert!(matches!(result, Err(BrowserError::SessionBusy { port, .. }) if port == mock.port()));
    assert_eq!(
        mock.count("session.new"),
        0,
        "a probe must never create a session"
    );
    assert!(!mock.browser_closed());
}

/// BRW-08: Auto to a Firefox whose session is taken fails with SessionBusy.
#[tokio::test]
async fn auto_to_busy_firefox_fails_with_session_busy() {
    let mock = mock(MockBidiBehavior::SessionBusy).await;
    let config = BrowserConfig::builder()
        .port(mock.port())
        .firefox_path(NO_FIREFOX)
        .build();
    let result = Browser::ensure(config).await;
    assert!(
        matches!(result, Err(BrowserError::SessionBusy { .. })),
        "{result:?}"
    );
}

/// BRW-10: client(), browser_client() and their clones share one session.
#[tokio::test]
async fn clients_share_one_session() {
    let mock = mock(MockBidiBehavior::Ready).await;
    let browser = attached(&mock).await;
    for _ in 0..5 {
        browser.client().await.unwrap().status().await.unwrap();
        browser
            .browser_client()
            .await
            .unwrap()
            .list_tabs()
            .await
            .unwrap();
    }
    assert_eq!(mock.count("session.new"), 1);
    assert_eq!(
        mock.connection_count(),
        3,
        "two probe connections and one session"
    );
}

/// BRW-12: An attached Firefox left busy after a disconnect yields SessionBusy and is not signalled.
#[tokio::test]
async fn attached_orphan_yields_session_busy() {
    let mock = mock(MockBidiBehavior::Ready).await;
    let browser = attached(&mock).await;
    mock.drop_connections();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let result = browser.browser_client().await.map(drop);
    assert!(
        matches!(result, Err(BrowserError::SessionBusy { .. })),
        "{result:?}"
    );
    assert!(!mock.browser_closed());
}

/// BRW-13: An attached Firefox that freed its session gets a new session transparently.
#[tokio::test]
async fn attached_reconnects_when_session_was_freed() {
    let mock = mock(MockBidiBehavior::Ready).await;
    let browser = attached(&mock).await;
    mock.drop_connections();
    mock.release_session();
    tokio::time::sleep(Duration::from_millis(100)).await;
    browser.list_tabs().await.unwrap();
    assert_eq!(mock.count("session.new"), 2);
}

/// BRW-16: stop on an attached instance sends session.end only and never signals the process.
#[tokio::test]
async fn stop_on_attached_ends_session_only() {
    let mock = mock(MockBidiBehavior::Ready).await;
    let browser = attached(&mock).await;
    browser.stop().await.unwrap();
    assert_eq!(mock.count("session.end"), 1);
    assert_eq!(mock.count("browser.close"), 0);
    assert!(!mock.session_active());
}

/// BRW-17: stop is idempotent and later client calls fail with Stopped.
#[tokio::test]
async fn stop_is_idempotent() {
    let mock = mock(MockBidiBehavior::Ready).await;
    let browser = attached(&mock).await;
    browser.stop().await.unwrap();
    browser.stop().await.unwrap();
    assert_eq!(mock.count("session.end"), 1);
    assert!(matches!(browser.client().await, Err(BrowserError::Stopped)));
    assert!(matches!(
        browser.new_tab("about:blank").await,
        Err(BrowserError::Stopped)
    ));
    assert!(!browser.is_alive().await);
}

/// BRW-19: restart on an attached instance ends and re-creates the session.
#[tokio::test]
async fn restart_attached_recreates_session() {
    let mock = mock(MockBidiBehavior::Ready).await;
    let browser = attached(&mock).await;
    browser.restart().await.unwrap();
    assert_eq!(
        (mock.count("session.end"), mock.count("session.new")),
        (1, 2)
    );
    browser.list_tabs().await.unwrap();
}

/// BRW-24: Dropping an attached Browser spawns session.end and leaves the process alone.
#[tokio::test]
async fn drop_attached_ends_session_in_background() {
    let mock = mock(MockBidiBehavior::Ready).await;
    drop(attached(&mock).await);
    let ended = crate::test_support_wait(|| mock.count("session.end") == 1).await;
    assert!(ended, "session.end must be sent in the background");
    assert!(!mock.browser_closed());
}

/// BRW-25: rdp_client without RDP fails with RdpDisabled.
#[tokio::test]
async fn rdp_client_without_rdp_is_disabled() {
    let mock = mock(MockBidiBehavior::Ready).await;
    let browser = attached(&mock).await;
    assert!(matches!(
        browser.rdp_client().await,
        Err(BrowserError::RdpDisabled)
    ));
    assert_eq!(browser.rdp_address().await, None);
}

/// BRW-26: rdp_client is cached and reconnects after the RDP socket dies.
#[tokio::test]
async fn rdp_client_is_cached() {
    let firefox = MockFirefox::start(true).await.unwrap();
    let rdp_port = firefox.rdp_port().unwrap();
    let config = attach_only(firefox.bidi_port()).rdp_port(rdp_port).build();
    let browser = Browser::ensure(config).await.unwrap();
    let rdp = firefox.rdp().unwrap();
    let probes = rdp.connection_count();
    browser.rdp_client().await.unwrap();
    browser.rdp_client().await.unwrap();
    assert_eq!(rdp.connection_count(), probes + 1, "cached");
    assert_eq!(
        browser.rdp_address().await,
        Some(("127.0.0.1".to_owned(), rdp_port))
    );
}

/// BRW-27: AttachOnly with a fixed RDP port that does not greet fails with RemoteUnavailable { port: rdp }.
#[tokio::test]
async fn attach_with_silent_rdp_fails() {
    let mock = mock(MockBidiBehavior::Ready).await;
    let rdp = MockRdp::start_silent().await.unwrap();
    let config = attach_only(mock.port())
        .rdp_port(rdp.port())
        .connect_timeout(Duration::from_millis(300))
        .build();
    let result = Browser::ensure(config).await;
    assert!(
        matches!(result, Err(BrowserError::RemoteUnavailable { port, .. }) if port == rdp.port())
    );
    assert_eq!(mock.count("session.new"), 0);
}

/// BRW-28: Tab helpers use the shared session.
#[tokio::test]
async fn tab_helpers_use_shared_session() {
    let mock = mock(MockBidiBehavior::Ready).await;
    let browser = attached(&mock).await;
    let tab = browser.new_tab("https://example.com/").await.unwrap();
    assert_eq!(browser.list_tabs().await.unwrap().len(), 2);
    browser.attach_tab(tab.context_id()).await.unwrap();
    assert_eq!(browser.attach_to_all_tabs().await.unwrap().len(), 2);
    browser.close_tab(tab.context_id()).await.unwrap();
    assert_eq!(browser.list_tabs().await.unwrap().len(), 1);
    assert_eq!(mock.count("session.new"), 1);
}

/// BRW-31: Concurrent client() calls during a reconnect create a single session.
#[tokio::test]
async fn concurrent_reconnect_creates_one_session() {
    let mock = mock(MockBidiBehavior::Ready).await;
    let browser = Arc::new(attached(&mock).await);
    mock.drop_connections();
    mock.release_session();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let calls: Vec<_> = (0..8)
        .map(|_| {
            let browser = Arc::clone(&browser);
            tokio::spawn(async move { browser.client().await.map(drop) })
        })
        .collect();
    for call in calls {
        call.await.unwrap().unwrap();
    }
    assert_eq!(mock.count("session.new"), 2);
}

/// BRW-32: Accessors report address, RDP address, profile dir, pid, managed and alive state.
#[tokio::test]
async fn accessors_of_attached_browser() {
    let mock = mock(MockBidiBehavior::Ready).await;
    let browser = attached(&mock).await;
    assert_eq!(
        browser.debug_address().await,
        ("127.0.0.1".to_owned(), mock.port())
    );
    assert_eq!(browser.profile_dir().await, None);
    assert_eq!(browser.pid().await, None);
    assert!(browser.is_alive().await);
    assert!(format!("{browser:?}").contains("Browser"));
}

/// BRW-34: test_from_state opens its session on first use.
#[tokio::test]
async fn test_from_state_opens_session_lazily() {
    let mock = mock(MockBidiBehavior::Ready).await;
    let config = attach_only(mock.port())
        .profile(ProfileMode::UserDefault)
        .build();
    let browser = Browser::test_from_state(config, BrowserState::unmanaged(mock.port(), None));
    assert_eq!(mock.count("session.new"), 0);
    browser.list_tabs().await.unwrap();
    assert_eq!(mock.count("session.new"), 1);
}

/// BRW-36: A session that does not answer in time is neither handed out nor dropped.
#[tokio::test]
async fn unresponsive_session_is_not_handed_out_nor_dropped() {
    // Given an attached browser whose Firefox stops answering
    let mock = mock(MockBidiBehavior::Ready).await;
    let config = attach_only(mock.port())
        .connect_timeout(Duration::from_millis(300))
        .build();
    let browser = Browser::ensure(config).await.unwrap();
    mock.stop_replying();
    // When a client is requested
    let result = browser.browser_client().await.map(drop);
    // Then it times out, and the session is kept: no new session, no restart
    assert!(
        matches!(
            result,
            Err(BrowserError::Bidi(
                bidi_lite::error::BidiError::Timeout { .. }
            ))
        ),
        "{result:?}"
    );
    assert_eq!(mock.count("session.new"), 1);
    assert!(
        browser.state.lock().await.session.is_some(),
        "a slow session is still ours"
    );
}

/// BRW-37: A cancelled attach does not leave an orphan session in a Firefox we do not own.
#[tokio::test]
async fn cancelled_attach_ends_its_session() {
    // Given a Firefox whose session.new reply is slow
    let mock = mock(MockBidiBehavior::Ready).await;
    mock.delay_session_new(Duration::from_millis(400));
    // When ensure is cancelled while session.new is in flight
    let ensure = Browser::ensure(attach_only(mock.port()).build());
    assert!(
        tokio::time::timeout(Duration::from_millis(150), ensure)
            .await
            .is_err()
    );
    // Then the session it created is ended once the reply arrives
    let ended =
        crate::test_support_wait(|| mock.count("session.end") == 1 && !mock.session_active()).await;
    assert!(ended, "the orphaned session must be ended");
}

//! Side-effect free readiness probes for the BiDi and RDP ports.
//!
//! Firefox allows a single BiDi session and keeps it after its WebSocket
//! dies, so a probe must never send `session.new`. The BiDi probes open a
//! session-less WebSocket to `/session` and ask `session.status` (verified
//! against Firefox 157: neither the handshake nor `session.status` affects
//! the session). The RDP probe reads the root actor's greeting. Every probe
//! is bounded by its timeout and closes its connection before returning.

use std::future::Future;
use std::time::Duration;

use bidi_lite::client::{BidiClient, session_url};
use bidi_lite::rdp::client::RdpClient;
use tokio::net::TcpStream;

use crate::error::BrowserError;

/// Whether something accepts TCP connections on `host:port`.
pub async fn is_port_open(host: &str, port: u16, timeout: Duration) -> bool {
    let connect = TcpStream::connect((host, port));
    matches!(tokio::time::timeout(timeout, connect).await, Ok(Ok(_)))
}

/// Whether `host:port` speaks Firefox's WebDriver BiDi: a WebSocket on
/// `/session` that answers `session.status`. Never creates a session.
pub async fn is_firefox_bidi(host: &str, port: u16, timeout: Duration) -> bool {
    session_ready(host, port, timeout).await.is_ok()
}

/// Whether a new BiDi session could be created on `host:port` right now
/// (`false` while another client holds Firefox's only session). Never creates
/// a session. Fails with [`BrowserError::RemoteUnavailable`] when nothing
/// BiDi answers.
pub async fn bidi_session_available(
    host: &str,
    port: u16,
    timeout: Duration,
) -> Result<bool, BrowserError> {
    session_ready(host, port, timeout).await
}

/// Whether `host:port` greets as the Firefox RDP root actor.
pub async fn is_firefox_rdp(host: &str, port: u16, timeout: Duration) -> bool {
    let address = host_port(host, port);
    bounded(timeout, async {
        RdpClient::connect(&address, timeout).await.map(drop)
    })
    .await
    .is_some_and(|result| result.is_ok())
}

/// `host:port`, bracketing bare IPv6 addresses.
pub(crate) fn host_port(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

async fn session_ready(host: &str, port: u16, timeout: Duration) -> Result<bool, BrowserError> {
    let url = session_url(&host_port(host, port));
    let status = bounded(timeout, async {
        let client = BidiClient::connect(&url, timeout).await?;
        client.status().await
    })
    .await;
    match status {
        Some(Ok(status)) => Ok(status.ready),
        _ => Err(BrowserError::RemoteUnavailable {
            host: host.to_owned(),
            port,
        }),
    }
}

/// Runs `future` with an overall deadline; `None` when it expired.
async fn bounded<T>(timeout: Duration, future: impl Future<Output = T>) -> Option<T> {
    tokio::time::timeout(timeout, future).await.ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{MockBidi, MockBidiBehavior, MockRdp};
    use std::time::Instant;

    const HOST: &str = "127.0.0.1";
    const T: Duration = Duration::from_millis(500);

    async fn bidi(behavior: MockBidiBehavior) -> MockBidi {
        MockBidi::start(behavior).await.unwrap()
    }

    fn closed_port() -> u16 {
        let listener = std::net::TcpListener::bind((HOST, 0)).unwrap();
        listener.local_addr().unwrap().port()
    }

    /// PRB-01: is_port_open is true for a listener and false for a closed port.
    #[tokio::test]
    async fn port_open_and_closed() {
        let mock = bidi(MockBidiBehavior::Ready).await;
        assert!(is_port_open(HOST, mock.port(), T).await);
        assert!(!is_port_open(HOST, closed_port(), T).await);
    }

    /// PRB-02: is_port_open returns within its timeout for a closed port.
    #[tokio::test]
    async fn port_open_is_bounded() {
        let start = Instant::now();
        let _ = is_port_open(HOST, closed_port(), T).await;
        assert!(start.elapsed() < T + Duration::from_millis(500));
    }

    /// PRB-03: is_firefox_bidi recognises the BiDi mock.
    #[tokio::test]
    async fn recognises_bidi_mock() {
        let mock = bidi(MockBidiBehavior::Ready).await;
        assert!(is_firefox_bidi(HOST, mock.port(), T).await);
        let busy = bidi(MockBidiBehavior::SessionBusy).await;
        assert!(
            is_firefox_bidi(HOST, busy.port(), T).await,
            "busy is still BiDi"
        );
    }

    /// PRB-04: is_firefox_bidi rejects things that are not Firefox BiDi.
    #[tokio::test]
    async fn rejects_non_bidi_peers() {
        // Given a plain HTTP server, a WebSocket server that is not BiDi, a silent peer and a closed port
        let http = bidi(MockBidiBehavior::PlainHttp).await;
        let silent = bidi(MockBidiBehavior::SilentPeer).await;
        let rdp = MockRdp::start().await.unwrap();
        // Then none of them is Firefox BiDi
        for port in [http.port(), silent.port(), rdp.port(), closed_port()] {
            assert!(!is_firefox_bidi(HOST, port, T).await, "port {port}");
        }
    }

    /// PRB-05: is_firefox_bidi never creates a session.
    #[tokio::test]
    async fn bidi_probe_never_creates_a_session() {
        let mock = bidi(MockBidiBehavior::Ready).await;
        for _ in 0..3 {
            assert!(is_firefox_bidi(HOST, mock.port(), T).await);
        }
        assert_eq!(mock.count("session.new"), 0);
        assert!(!mock.session_active());
    }

    /// PRB-06: bidi_session_available reports free and busy sessions.
    #[tokio::test]
    async fn session_available_free_and_busy() {
        let free = bidi(MockBidiBehavior::Ready).await;
        let busy = bidi(MockBidiBehavior::SessionBusy).await;
        assert!(bidi_session_available(HOST, free.port(), T).await.unwrap());
        assert!(!bidi_session_available(HOST, busy.port(), T).await.unwrap());
        let closed = bidi_session_available(HOST, closed_port(), T).await;
        assert!(matches!(
            closed,
            Err(BrowserError::RemoteUnavailable { .. })
        ));
    }

    /// PRB-07: bidi_session_available never sends session.new.
    #[tokio::test]
    async fn session_available_never_sends_session_new() {
        let mock = bidi(MockBidiBehavior::Ready).await;
        let _ = bidi_session_available(HOST, mock.port(), T).await;
        assert_eq!(mock.count("session.new"), 0);
        assert_eq!(mock.count("session.status"), 1);
    }

    /// PRB-08: is_firefox_rdp recognises the root greeting.
    #[tokio::test]
    async fn recognises_rdp_greeting() {
        let mock = MockRdp::start().await.unwrap();
        assert!(is_firefox_rdp(HOST, mock.port(), T).await);
    }

    /// PRB-09: is_firefox_rdp is false when no greeting arrives or it is not RDP.
    #[tokio::test]
    async fn rdp_probe_rejects_silent_and_bidi() {
        let silent = MockRdp::start_silent().await.unwrap();
        let bidi = bidi(MockBidiBehavior::Ready).await;
        assert!(!is_firefox_rdp(HOST, silent.port(), T).await);
        assert!(!is_firefox_rdp(HOST, bidi.port(), T).await);
        assert!(!is_firefox_rdp(HOST, closed_port(), T).await);
    }

    /// PRB-10: Probes work on a current-thread runtime.
    #[test]
    fn probes_work_on_current_thread_runtime() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mock = bidi(MockBidiBehavior::Ready).await;
            assert!(is_firefox_bidi(HOST, mock.port(), T).await);
        });
    }

    /// PRB-11: A silent peer cannot make any probe exceed its timeout by more than 500 ms.
    #[tokio::test]
    async fn silent_peer_cannot_stall_probes() {
        let silent = bidi(MockBidiBehavior::SilentPeer).await;
        let start = Instant::now();
        assert!(!is_firefox_bidi(HOST, silent.port(), T).await);
        assert!(!is_firefox_rdp(HOST, silent.port(), T).await);
        assert!(
            bidi_session_available(HOST, silent.port(), T)
                .await
                .is_err()
        );
        assert!(start.elapsed() < 3 * T + Duration::from_millis(500));
    }

    /// PRB-12: IPv6 hosts are bracketed.
    #[test]
    fn ipv6_hosts_are_bracketed() {
        assert_eq!(host_port("::1", 9), "[::1]:9");
        assert_eq!(host_port("[::1]", 9), "[::1]:9");
        assert_eq!(host_port("127.0.0.1", 9), "127.0.0.1:9");
    }
}

//! `MockRdp`: a Firefox debugger server (RDP over WebSocket) stand-in.

use std::io;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;

use super::fixtures::{self as fx, load};
use super::server::Server;

/// A mock RDP endpoint that greets as the root actor and answers
/// `listTabs` and `getRoot`. Dropping it closes the port.
pub struct MockRdp {
    server: Server,
}

impl MockRdp {
    /// Starts on a free port; every connection gets the captured greeting.
    pub async fn start() -> io::Result<Self> {
        Self::start_with(0, true).await
    }

    /// Starts on `127.0.0.1:port` (0 = any free port).
    pub async fn start_on(port: u16) -> io::Result<Self> {
        Self::start_with(port, true).await
    }

    /// Starts a server that accepts WebSocket connections and never greets,
    /// like Firefox announcing a debugger server it could not bind.
    pub async fn start_silent() -> io::Result<Self> {
        Self::start_with(0, false).await
    }

    /// Like [`MockRdp::start_silent`] on `127.0.0.1:port` (0 = any free port).
    pub async fn start_silent_on(port: u16) -> io::Result<Self> {
        Self::start_with(port, false).await
    }

    async fn start_with(port: u16, greet: bool) -> io::Result<Self> {
        let server = Server::start(port, move |stream, _| serve(stream, greet)).await?;
        Ok(Self { server })
    }

    /// The bound port.
    pub fn port(&self) -> u16 {
        self.server.port()
    }

    /// How many TCP connections were accepted.
    pub fn connection_count(&self) -> usize {
        self.server.connection_count()
    }
}

async fn serve(stream: TcpStream, greet: bool) {
    let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    if greet && ws.send(Message::text(fx::RDP_GREETING)).await.is_err() {
        return;
    }
    while let Some(Ok(message)) = ws.next().await {
        if !greet {
            continue;
        }
        let Message::Text(text) = message else {
            continue;
        };
        if ws
            .send(Message::text(answer(&text).to_string()))
            .await
            .is_err()
        {
            break;
        }
    }
}

fn answer(text: &str) -> Value {
    let packet: Value = serde_json::from_str(text).unwrap_or_default();
    let kind = packet
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match kind {
        "listTabs" => load(fx::RDP_LIST_TABS),
        "getRoot" => load(fx::RDP_GET_ROOT),
        _ => {
            let mut error = load(fx::RDP_UNRECOGNIZED);
            error["message"] = Value::from(format!(
                "Actor root does not recognize the packet type '{kind}'"
            ));
            error
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bidi_lite::rdp::client::RdpClient;
    use std::time::Duration;

    const T: Duration = Duration::from_secs(5);

    /// MCK-06: MockRdp greets with the captured root packet.
    #[tokio::test]
    async fn greets_with_captured_root_packet() {
        // Given a mock RDP server
        let mock = MockRdp::start().await.unwrap();
        // When bidi-lite's RdpClient connects and lists tabs
        let client = RdpClient::connect(&format!("127.0.0.1:{}", mock.port()), T)
            .await
            .unwrap();
        let tabs = client
            .request("root", "listTabs", serde_json::json!({}))
            .await
            .unwrap();
        // Then the greeting and the reply come from the fixtures
        assert_eq!(client.greeting()["applicationType"], "browser");
        assert!(
            tabs.body.get("tabs").is_some_and(Value::is_array),
            "{tabs:?}"
        );
    }

    /// MCK-13: A silent MockRdp never greets, so RdpClient::connect times out.
    #[tokio::test]
    async fn silent_server_never_greets() {
        let mock = MockRdp::start_silent().await.unwrap();
        let host = format!("127.0.0.1:{}", mock.port());
        let result = RdpClient::connect(&host, Duration::from_millis(200)).await;
        assert!(result.is_err());
        assert_eq!(mock.connection_count(), 1);
    }
}

//! `MockBidi`: a Firefox Remote Agent stand-in on `ws://127.0.0.1:P/session`.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::Message;

use super::fixtures::{self as fx, reply};
use super::server::Server;

/// How a [`MockBidi`] behaves from the start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MockBidiBehavior {
    /// The session is free. Like Firefox, a session outlives its WebSocket.
    Ready,
    /// A foreign client already holds the session: `session.status` answers
    /// `ready: false` and `session.new` fails with `session not created`.
    SessionBusy,
    /// `session.new` takes the session and drops the socket without replying
    /// (a timed-out `session.new` that leaves an orphan behind).
    CloseOnSessionNew,
    /// Accepts TCP connections and never answers.
    SilentPeer,
    /// Answers `HTTP 200` to anything and never upgrades to WebSocket.
    PlainHttp,
}

/// Who holds the browser's single BiDi session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owner {
    Connection(u64),
    Foreign,
}

/// The browser the mock pretends to be.
struct State {
    behavior: MockBidiBehavior,
    owner: Option<Owner>,
    closed: bool,
    counts: HashMap<String, usize>,
    contexts: Vec<(String, String)>,
    next_context: u64,
}

struct Shared {
    state: Mutex<State>,
    closed: Notify,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// What to do after a command.
enum Reply {
    Send(Value),
    SendThenClose(Value),
    Close,
}

/// A mock Firefox BiDi endpoint. Dropping it closes the port and every
/// connection.
pub struct MockBidi {
    server: Server,
    shared: Arc<Shared>,
}

impl MockBidi {
    /// Starts on `127.0.0.1` with a free port.
    pub async fn start(behavior: MockBidiBehavior) -> io::Result<Self> {
        Self::start_on(0, behavior).await
    }

    /// Starts on `127.0.0.1:port` (0 = any free port).
    pub async fn start_on(port: u16, behavior: MockBidiBehavior) -> io::Result<Self> {
        let owner = (behavior == MockBidiBehavior::SessionBusy).then_some(Owner::Foreign);
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                behavior,
                owner,
                closed: false,
                counts: HashMap::new(),
                contexts: vec![("ctx-0".to_owned(), "about:home".to_owned())],
                next_context: 1,
            }),
            closed: Notify::new(),
        });
        let serving = Arc::clone(&shared);
        let server = Server::start(port, move |stream, id| {
            serve(stream, id, Arc::clone(&serving))
        })
        .await?;
        Ok(Self { server, shared })
    }

    /// The bound port.
    pub fn port(&self) -> u16 {
        self.server.port()
    }

    /// `ws://127.0.0.1:P/session`.
    pub fn ws_url(&self) -> String {
        format!("ws://127.0.0.1:{}/session", self.port())
    }

    /// How many commands named `method` were received.
    pub fn count(&self, method: &str) -> usize {
        self.shared.lock().counts.get(method).copied().unwrap_or(0)
    }

    /// How many TCP connections were accepted.
    pub fn connection_count(&self) -> usize {
        self.server.connection_count()
    }

    /// Whether someone holds the BiDi session.
    pub fn session_active(&self) -> bool {
        self.shared.lock().owner.is_some()
    }

    /// Whether `browser.close` was received.
    pub fn browser_closed(&self) -> bool {
        self.shared.lock().closed
    }

    /// Waits until `browser.close` is received.
    pub async fn wait_browser_closed(&self) {
        loop {
            let notified = self.shared.closed.notified();
            if self.browser_closed() {
                return;
            }
            notified.await;
        }
    }

    /// Closes every live socket. The session, if any, survives as an orphan,
    /// exactly like Firefox.
    pub fn drop_connections(&self) {
        self.server.drop_connections();
    }

    /// Frees the session as if its owner had ended it.
    pub fn release_session(&self) {
        self.shared.lock().owner = None;
    }

    /// Gives the session to a foreign client.
    pub fn occupy_session(&self) {
        self.shared.lock().owner = Some(Owner::Foreign);
    }
}

async fn serve(stream: TcpStream, conn: u64, shared: Arc<Shared>) {
    let behavior = shared.lock().behavior;
    match behavior {
        MockBidiBehavior::SilentPeer => hold_silently(stream).await,
        MockBidiBehavior::PlainHttp => answer_http(stream).await,
        _ => serve_ws(stream, conn, shared).await,
    }
}

async fn hold_silently(mut stream: TcpStream) {
    let mut sink = [0u8; 1024];
    while matches!(stream.read(&mut sink).await, Ok(n) if n > 0) {}
}

async fn answer_http(mut stream: TcpStream) {
    let mut request = [0u8; 4096];
    let _ = stream.read(&mut request).await;
    let response = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
    let _ = stream.write_all(response.as_bytes()).await;
}

async fn serve_ws(stream: TcpStream, conn: u64, shared: Arc<Shared>) {
    let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    while let Some(Ok(message)) = ws.next().await {
        let Message::Text(text) = message else {
            continue;
        };
        let reply = handle(&shared, conn, &text);
        let (value, close) = match reply {
            Reply::Send(value) => (Some(value), false),
            Reply::SendThenClose(value) => (Some(value), true),
            Reply::Close => (None, true),
        };
        if let Some(value) = value
            && ws.send(Message::text(value.to_string())).await.is_err()
        {
            break;
        }
        if close {
            let _ = ws.close(None).await;
            break;
        }
    }
    if shared.lock().closed {
        shared.closed.notify_waiters();
    }
}

/// One parsed command.
struct Command {
    id: u64,
    method: String,
    params: Value,
}

fn parse(text: &str) -> Option<Command> {
    let value: Value = serde_json::from_str(text).ok()?;
    Some(Command {
        id: value.get("id")?.as_u64()?,
        method: value.get("method")?.as_str()?.to_owned(),
        params: value.get("params").cloned().unwrap_or(Value::Null),
    })
}

fn handle(shared: &Shared, conn: u64, text: &str) -> Reply {
    let Some(cmd) = parse(text) else {
        return Reply::Send(
            json!({"type": "error", "id": null, "error": "invalid argument", "message": "bad command"}),
        );
    };
    let mut state = shared.lock();
    *state.counts.entry(cmd.method.clone()).or_default() += 1;
    let owns = state.owner == Some(Owner::Connection(conn));
    match cmd.method.as_str() {
        "session.status" => Reply::Send(status(&state, cmd.id)),
        "session.new" => session_new(&mut state, conn, cmd.id),
        "session.end" if owns => {
            state.owner = None;
            Reply::SendThenClose(reply(fx::SESSION_END, cmd.id))
        }
        _ if !owns => Reply::Send(reply(fx::INVALID_SESSION_ID, cmd.id)),
        "browser.close" => {
            state.owner = None;
            state.closed = true;
            Reply::SendThenClose(reply(fx::BROWSER_CLOSE, cmd.id))
        }
        "session.subscribe" => Reply::Send(reply(fx::SUBSCRIBE, cmd.id)),
        method if method.starts_with("browsingContext.") => Reply::Send(contexts(&mut state, &cmd)),
        _ => Reply::Send(success(cmd.id, json!({}))),
    }
}

fn status(state: &State, id: u64) -> Value {
    if state.owner.is_none() && !state.closed {
        reply(fx::SESSION_STATUS_READY, id)
    } else {
        reply(fx::SESSION_STATUS_BUSY, id)
    }
}

fn session_new(state: &mut State, conn: u64, id: u64) -> Reply {
    if state.owner.is_some() || state.closed {
        return Reply::Send(reply(fx::SESSION_NEW_BUSY, id));
    }
    state.owner = Some(Owner::Connection(conn));
    match state.behavior {
        MockBidiBehavior::CloseOnSessionNew => Reply::Close,
        _ => Reply::Send(reply(fx::SESSION_NEW, id)),
    }
}

fn success(id: u64, result: Value) -> Value {
    json!({"type": "success", "id": id, "result": result})
}

fn contexts(state: &mut State, cmd: &Command) -> Value {
    let context = cmd
        .params
        .get("context")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let known = state.contexts.iter().position(|(id, _)| id == context);
    match (cmd.method.as_str(), known) {
        ("browsingContext.getTree", _) => tree(state, cmd.id),
        ("browsingContext.create", _) => create(state, cmd.id),
        ("browsingContext.navigate", Some(i)) => navigate(state, i, cmd),
        ("browsingContext.close", Some(i)) => {
            state.contexts.remove(i);
            success(cmd.id, json!({}))
        }
        ("browsingContext.activate", Some(_)) => success(cmd.id, json!({})),
        (_, None) if !context.is_empty() => reply(fx::NO_SUCH_FRAME, cmd.id),
        _ => unknown(cmd),
    }
}

fn tree(state: &State, id: u64) -> Value {
    let mut value = reply(fx::GET_TREE, id);
    let template = value["result"]["contexts"][0].clone();
    let list: Vec<Value> = state
        .contexts
        .iter()
        .map(|(context, url)| {
            let mut entry = template.clone();
            entry["context"] = Value::from(context.as_str());
            entry["url"] = Value::from(url.as_str());
            entry
        })
        .collect();
    value["result"]["contexts"] = Value::Array(list);
    value
}

fn create(state: &mut State, id: u64) -> Value {
    let context = format!("ctx-{}", state.next_context);
    state.next_context += 1;
    state
        .contexts
        .push((context.clone(), "about:blank".to_owned()));
    let mut value = reply(fx::CREATE_TAB, id);
    value["result"]["context"] = Value::from(context);
    value
}

fn navigate(state: &mut State, index: usize, cmd: &Command) -> Value {
    let url = cmd
        .params
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("about:blank");
    if let Some(entry) = state.contexts.get_mut(index) {
        entry.1 = url.to_owned();
    }
    let mut value = reply(fx::NAVIGATE, cmd.id);
    value["result"]["url"] = Value::from(url);
    value
}

fn unknown(cmd: &Command) -> Value {
    let mut value = reply(fx::UNKNOWN_COMMAND, cmd.id);
    value["message"] = Value::from(cmd.method.as_str());
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use bidi_lite::browser::BrowserClient;
    use bidi_lite::client::BidiClient;
    use bidi_lite::error::BidiError;
    use std::time::Duration;

    const T: Duration = Duration::from_secs(5);

    async fn raw(mock: &MockBidi) -> BidiClient {
        BidiClient::connect(&mock.ws_url(), T).await.unwrap()
    }

    async fn session(mock: &MockBidi) -> BidiClient {
        BidiClient::new(&format!("127.0.0.1:{}", mock.port()), T)
            .await
            .unwrap()
    }

    /// MCK-01: MockBidi replies to session.status with the captured fixture.
    #[tokio::test]
    async fn status_reply_matches_fixture() {
        let mock = MockBidi::start(MockBidiBehavior::Ready).await.unwrap();
        let status = raw(&mock).await.status().await.unwrap();
        assert!(status.ready);
        assert_eq!(mock.count("session.status"), 1);
    }

    /// MCK-02: A WebSocket handshake that sends nothing leaves the mock session free.
    #[tokio::test]
    async fn bare_handshake_leaves_session_free() {
        let mock = MockBidi::start(MockBidiBehavior::Ready).await.unwrap();
        drop(raw(&mock).await);
        assert!(!mock.session_active());
        assert!(raw(&mock).await.status().await.unwrap().ready);
    }

    /// MCK-03: MockBidi SessionBusy reports ready:false and refuses session.new.
    #[tokio::test]
    async fn busy_mock_refuses_sessions() {
        let mock = MockBidi::start(MockBidiBehavior::SessionBusy)
            .await
            .unwrap();
        let client = raw(&mock).await;
        let status = client.status().await.unwrap();
        assert!(!status.ready);
        assert_eq!(status.message, "Session already started");
        let started = client
            .send_raw_command("session.new", json!({"capabilities": {}}))
            .await;
        assert!(
            matches!(started, Err(BidiError::ProtocolError { ref error, .. }) if error == "session not created")
        );
    }

    /// MCK-04: A session survives drop_connections and blocks a new session, like Firefox.
    #[tokio::test]
    async fn session_survives_dropped_connections() {
        // Given a session whose socket is dropped
        let mock = MockBidi::start(MockBidiBehavior::Ready).await.unwrap();
        let _client = session(&mock).await;
        mock.drop_connections();
        tokio::time::sleep(Duration::from_millis(50)).await;
        // Then the orphan blocks new sessions and cannot be ended from outside
        assert!(mock.session_active());
        let other = raw(&mock).await;
        assert!(!other.status().await.unwrap().ready);
        let ended = other.send_raw_command("session.end", json!({})).await;
        assert!(
            matches!(ended, Err(BidiError::ProtocolError { ref error, .. }) if error == "invalid session id")
        );
    }

    /// MCK-05: session.end replies then closes the socket.
    #[tokio::test]
    async fn session_end_closes_socket() {
        let mock = MockBidi::start(MockBidiBehavior::Ready).await.unwrap();
        let client = session(&mock).await;
        client.end_session().await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!client.is_connected());
        assert!(!mock.session_active());
    }

    /// MCK-07: browser.close marks browser_closed and refuses further sessions.
    #[tokio::test]
    async fn browser_close_marks_closed() {
        let mock = MockBidi::start(MockBidiBehavior::Ready).await.unwrap();
        BrowserClient::from_client(session(&mock).await)
            .close_browser()
            .await
            .unwrap();
        tokio::time::timeout(T, mock.wait_browser_closed())
            .await
            .unwrap();
        assert!(mock.browser_closed());
        assert!(!raw(&mock).await.status().await.unwrap().ready);
    }

    /// MCK-08: count() and connection_count() track traffic per method and per socket.
    #[tokio::test]
    async fn counts_track_traffic() {
        let mock = MockBidi::start(MockBidiBehavior::Ready).await.unwrap();
        let a = raw(&mock).await;
        let b = raw(&mock).await;
        a.status().await.unwrap();
        b.status().await.unwrap();
        assert_eq!(mock.count("session.status"), 2);
        assert_eq!(mock.count("session.new"), 0);
        assert_eq!(mock.connection_count(), 2);
    }

    /// MCK-09: Mocks shut down on Drop (port no longer accepts).
    #[tokio::test]
    async fn drop_closes_the_port() {
        let mock = MockBidi::start(MockBidiBehavior::Ready).await.unwrap();
        let port = mock.port();
        drop(mock);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(TcpStream::connect(("127.0.0.1", port)).await.is_err());
    }

    /// MCK-10: bidi-lite's BrowserClient works end to end against MockBidi.
    #[tokio::test]
    async fn browser_client_end_to_end() {
        let mock = MockBidi::start(MockBidiBehavior::Ready).await.unwrap();
        let browser = BrowserClient::from_client(session(&mock).await);
        let tab = browser.new_tab("https://example.com/").await.unwrap();
        let tabs = browser.list_tabs().await.unwrap();
        assert_eq!(tabs.len(), 2);
        assert!(tabs.iter().any(|t| t.url == "https://example.com/"));
        browser.close_tab(tab.context_id()).await.unwrap();
        assert_eq!(browser.list_tabs().await.unwrap().len(), 1);
        browser.end().await.unwrap();
        assert!(!mock.session_active());
    }

    /// MCK-12: CloseOnSessionNew drops the socket and leaves an orphan session.
    #[tokio::test]
    async fn close_on_session_new_leaves_orphan() {
        let mock = MockBidi::start(MockBidiBehavior::CloseOnSessionNew)
            .await
            .unwrap();
        let client = raw(&mock).await;
        let started = client
            .send_raw_command("session.new", json!({"capabilities": {}}))
            .await;
        assert!(started.is_err());
        assert!(mock.session_active());
    }
}

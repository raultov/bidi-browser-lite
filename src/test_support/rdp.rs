//! `MockRdp`: a Firefox debugger server (RDP over WebSocket) stand-in.

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::Message;

use super::fixtures::{self as fx, load};
use super::server::Server;

/// An RDP request received by [`MockRdp`].
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedRdpRequest {
    /// The actor the request was sent to.
    pub to: String,
    /// The request type.
    pub request_type: String,
    /// The request parameters or body.
    pub params: Value,
}

#[derive(Clone)]
struct TargetState {
    actor: String,
    thread_actor: String,
    console_actor: String,
}

struct State {
    requests: Vec<RecordedRdpRequest>,
    senders: HashMap<u64, tokio::sync::mpsc::UnboundedSender<Value>>,
    target: TargetState,
    target_generation: usize,
    watcher_actor: String,
    breakpoint_list_actor: String,
    thread_config_actor: String,
    tab_descriptor_actor: String,
    source_actor: String,
    long_string_actor: String,
    breakpoints: Vec<Value>,
    watched_resources: HashSet<String>,
    current_line: u32,
    current_column: u32,
    tabs: Vec<Value>,
    process_descriptor_actor: String,
    parent_console_actor: String,
    context_browser_ids: HashMap<String, u64>,
    parent_console_failures: usize,
    added_sources: Vec<Value>,
    source_positions: HashMap<String, Value>,
    before_reply: HashMap<String, Vec<Value>>,
}

impl State {
    fn new() -> Self {
        Self {
            requests: Vec::new(),
            senders: HashMap::new(),
            target: TargetState {
                actor: "server1.conn1.watcher2.process7//windowGlobalTarget2".to_string(),
                thread_actor: "server1.conn1.watcher2.process7//thread1".to_string(),
                console_actor: "server1.conn1.watcher2.process7//consoleActor3".to_string(),
            },
            target_generation: 2,
            watcher_actor: "server1.conn1.watcher3".to_string(),
            breakpoint_list_actor: "server1.conn1.breakpoint-list4".to_string(),
            thread_config_actor: "server1.conn1.thread-configuration5".to_string(),
            tab_descriptor_actor: "server1.conn1.tabDescriptor1".to_string(),
            source_actor: "server1.conn1.watcher2.process7//source18".to_string(),
            long_string_actor: "server1.conn0.longString2".to_string(),
            breakpoints: Vec::new(),
            watched_resources: HashSet::new(),
            current_line: 7,
            current_column: 10,
            tabs: tabs_of(fx::RDP_LIST_TABS),
            process_descriptor_actor: string_at(
                fx::RDP_GET_PROCESS,
                &["processDescriptor", "actor"],
            ),
            parent_console_actor: string_at(
                fx::RDP_PARENT_PROCESS_TARGET,
                &["process", "consoleActor"],
            ),
            context_browser_ids: HashMap::new(),
            parent_console_failures: 0,
            added_sources: Vec::new(),
            source_positions: HashMap::new(),
            before_reply: HashMap::new(),
        }
    }
}

struct Shared {
    state: Mutex<State>,
    request_arrived: Notify,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A mock RDP endpoint that greets as the root actor and answers
/// debugger requests with captured Firefox 157 frames.
pub struct MockRdp {
    server: Server,
    shared: Arc<Shared>,
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
        let shared = Arc::new(Shared {
            state: Mutex::new(State::new()),
            request_arrived: Notify::new(),
        });
        let serving = Arc::clone(&shared);
        let server = Server::start(port, move |stream, conn| {
            serve(stream, conn, greet, Arc::clone(&serving))
        })
        .await?;
        Ok(Self { server, shared })
    }

    /// The bound port.
    pub fn port(&self) -> u16 {
        self.server.port()
    }

    /// How many TCP connections were accepted.
    pub fn connection_count(&self) -> usize {
        self.server.connection_count()
    }

    /// Every request received in arrival order.
    pub fn requests(&self) -> Vec<RecordedRdpRequest> {
        self.shared.lock().requests.clone()
    }

    /// Requests sent to `actor` in arrival order.
    pub fn requests_to(&self, actor: &str) -> Vec<RecordedRdpRequest> {
        self.shared
            .lock()
            .requests
            .iter()
            .filter(|r| r.to == actor)
            .cloned()
            .collect()
    }

    /// Empties the request record.
    pub fn clear_requests(&self) {
        self.shared.lock().requests.clear();
    }

    /// Waits until a request of `request_type` arrives, or `timeout` expires.
    pub async fn wait_for_request(
        &self,
        request_type: &str,
        timeout: Duration,
    ) -> Option<RecordedRdpRequest> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let notified = self.shared.request_arrived.notified();
            {
                let state = self.shared.lock();
                if let Some(req) = state
                    .requests
                    .iter()
                    .rev()
                    .find(|r| r.request_type == request_type)
                {
                    return Some(req.clone());
                }
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                let state = self.shared.lock();
                return state
                    .requests
                    .iter()
                    .rev()
                    .find(|r| r.request_type == request_type)
                    .cloned();
            }
        }
    }

    /// Announces a pause twice: as a `paused` packet and as a `thread-state` resource.
    pub fn pause(&self, why: &str, frame: Option<Value>) {
        let state = self.shared.lock();
        let frame_val = frame.unwrap_or_else(|| {
            let mut default_frame = load(fx::RDP_THREAD_STATE_PAUSED)["frame"].clone();
            default_frame["where"]["line"] = Value::from(state.current_line);
            default_frame["where"]["column"] = Value::from(state.current_column);
            default_frame
        });
        let paused_packet = json!({
            "type": "paused",
            "actor": format!("{}//pause1", state.target.thread_actor),
            "from": state.target.thread_actor,
            "why": { "type": why, "actors": [Value::Null] },
            "frame": frame_val.clone(),
        });
        let mut resource_packet = load(fx::RDP_RESOURCES_THREAD_STATE_PAUSED);
        resource_packet["from"] = Value::from(state.target.actor.clone());
        resource_packet["array"][0][1][0]["why"]["type"] = Value::from(why);
        resource_packet["array"][0][1][0]["frame"] = frame_val;
        for tx in state.senders.values() {
            let _ = tx.send(paused_packet.clone());
            let _ = tx.send(resource_packet.clone());
        }
    }

    /// Swaps the target with new thread and console actors, announcing the switch.
    pub fn navigate(&self) {
        let mut state = self.shared.lock();
        let (destroyed, available, sources) = build_navigation_packets(&mut state);
        for tx in state.senders.values() {
            let _ = tx.send(destroyed.clone());
            let _ = tx.send(available.clone());
            let _ = tx.send(sources.clone());
        }
    }

    /// Emits a `resources-available-array` packet of `resource_type` from the current target.
    pub fn emit_resources(&self, resource_type: &str, resources: Vec<Value>) {
        let state = self.shared.lock();
        let packet = json!({
            "type": "resources-available-array",
            "from": state.target.actor,
            "array": [ [ resource_type, resources ] ]
        });
        for tx in state.senders.values() {
            let _ = tx.send(packet.clone());
        }
    }

    /// Emits a `resources-updated-array` packet of `resource_type` from the current target.
    pub fn emit_resource_updates(&self, resource_type: &str, updates: Vec<Value>) {
        let state = self.shared.lock();
        let packet = json!({
            "type": "resources-updated-array",
            "from": state.target.actor,
            "array": [ [ resource_type, updates ] ]
        });
        for tx in state.senders.values() {
            let _ = tx.send(packet.clone());
        }
    }

    /// Emits an arbitrary packet to all connected clients.
    pub fn emit_packet(&self, packet: Value) {
        let state = self.shared.lock();
        for tx in state.senders.values() {
            let _ = tx.send(packet.clone());
        }
    }

    /// All watcher resource types currently watched on the mock.
    pub fn watched_resources(&self) -> Vec<String> {
        let mut watched: Vec<_> = self
            .shared
            .lock()
            .watched_resources
            .iter()
            .cloned()
            .collect();
        watched.sort();
        watched
    }

    /// Replaces the tab descriptors `listTabs` answers with (by default the
    /// one tab of the captured `listTabs`). Each descriptor answers
    /// `getWatcher`; [`fixtures::RDP_LIST_TABS_TWO_TABS`](fx::RDP_LIST_TABS_TWO_TABS)
    /// holds captured descriptors with distinct `browserId`s on the same URL.
    pub fn set_tabs(&self, tabs: Vec<Value>) {
        self.shared.lock().tabs = tabs;
    }

    /// Makes the parent-process lookup of the BiDi context `context` (what
    /// `RdpClient::browser_id_of_context` evaluates) answer `browser_id`.
    /// Unmapped contexts answer `null`, like a closed or unknown context.
    pub fn map_context(&self, context: &str, browser_id: u64) {
        self.shared
            .lock()
            .context_browser_ids
            .insert(context.to_owned(), browser_id);
    }

    /// Makes the next evaluation on the parent-process console answer
    /// `noSuchActor`, like a console that went away with its target.
    pub fn fail_parent_console_once(&self) {
        self.shared.lock().parent_console_failures += 1;
    }

    /// Adds a source with `url` whose `getBreakpointPositionsCompressed`
    /// answers `positions` (`{"<line>": [columns]}`, `{}` for none),
    /// restricted to the queried lines. Sources are announced after the
    /// captured one, in the order they were added (one per inline
    /// `<script>` of a page share its URL); a source added while `source`
    /// resources are watched is announced at once. Returns its actor.
    pub fn add_source(&self, url: &str, positions: Value) -> String {
        let mut state = self.shared.lock();
        let actor = format!(
            "server1.conn1.watcher2.process7//source_added_{}",
            state.added_sources.len() + 1
        );
        let mut resource = load(fx::RDP_RESOURCES_SOURCES)["array"][0][1][0].clone();
        resource["actor"] = Value::from(actor.as_str());
        resource["url"] = Value::from(url);
        resource["sourceMapBaseURL"] = Value::from(url);
        state.added_sources.push(resource.clone());
        state.source_positions.insert(actor.clone(), positions);
        if state.watched_resources.contains("source") {
            let packet = json!({
                "type": "resources-available-array",
                "from": state.target.actor,
                "array": [ [ "source", [ resource ] ] ]
            });
            for tx in state.senders.values() {
                let _ = tx.send(packet.clone());
            }
        }
        actor
    }

    /// Sends `packet` right before the reply to the next request of
    /// `request_type`, e.g. a `forwardingCancelled` from `root` racing a
    /// `listTabs` reply.
    pub fn emit_before_reply(&self, request_type: &str, packet: Value) {
        self.shared
            .lock()
            .before_reply
            .entry(request_type.to_owned())
            .or_default()
            .push(packet);
    }
}

/// The string at `path` in a fixture, empty if absent.
fn string_at(fixture: &str, path: &[&str]) -> String {
    path.iter()
        .fold(&load(fixture), |value, key| &value[*key])
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// The `tabs` array of a `listTabs` fixture.
fn tabs_of(fixture: &str) -> Vec<Value> {
    match load(fixture)["tabs"].take() {
        Value::Array(tabs) => tabs,
        _ => Vec::new(),
    }
}

fn build_navigation_packets(state: &mut State) -> (Value, Value, Value) {
    state.target_generation += 1;
    let generation = state.target_generation;
    let old_target = state.target.actor.clone();
    state.target = TargetState {
        actor: format!("server1.conn1.watcher2.process7//windowGlobalTarget{generation}"),
        thread_actor: format!("server1.conn1.watcher2.process7//thread{generation}"),
        console_actor: format!("server1.conn1.watcher2.process7//consoleActor{generation}"),
    };
    let mut destroyed = load(fx::RDP_TARGET_DESTROYED);
    destroyed["from"] = Value::from(state.watcher_actor.as_str());
    destroyed["target"]["actor"] = Value::from(old_target);
    let mut available = load(fx::RDP_TARGET_AVAILABLE);
    available["from"] = Value::from(state.watcher_actor.as_str());
    available["target"]["actor"] = Value::from(state.target.actor.clone());
    available["target"]["threadActor"] = Value::from(state.target.thread_actor.clone());
    available["target"]["consoleActor"] = Value::from(state.target.console_actor.clone());
    let sources = json!({
        "type": "resources-available-array",
        "from": state.target.actor,
        "array": [ [ "source", [ {
            "actor": format!("server1.conn1.watcher2.process7//source_nav_{generation}"),
            "url": "https://example.com/navigated.html",
        } ] ] ]
    });
    (destroyed, available, sources)
}

struct ConnectionGuard {
    conn: u64,
    shared: Arc<Shared>,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.shared.lock().senders.remove(&self.conn);
    }
}

async fn serve(stream: TcpStream, conn: u64, greet: bool, shared: Arc<Shared>) {
    let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    if greet && ws.send(Message::text(fx::RDP_GREETING)).await.is_err() {
        return;
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    shared.lock().senders.insert(conn, tx);
    let _guard = ConnectionGuard {
        conn,
        shared: Arc::clone(&shared),
    };
    run_ws_loop(&mut ws, &mut rx, greet, &shared).await;
}

async fn run_ws_loop(
    ws: &mut tokio_tungstenite::WebSocketStream<TcpStream>,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Value>,
    greet: bool,
    shared: &Shared,
) {
    loop {
        tokio::select! {
            message = ws.next() => match message {
                Some(Ok(Message::Text(text))) => {
                    if greet && dispatch_incoming(shared, ws, &text).await {
                        let _ = ws.close(None).await;
                        break;
                    }
                }
                Some(Ok(_)) => {}
                _ => break,
            },
            Some(notif) = rx.recv() => {
                if ws.send(Message::text(notif.to_string())).await.is_err() {
                    break;
                }
            }
        }
    }
}

async fn dispatch_incoming(
    shared: &Shared,
    ws: &mut tokio_tungstenite::WebSocketStream<TcpStream>,
    text: &str,
) -> bool {
    let (reply, notifications) = handle_message(shared, text);
    for notif in notifications {
        if ws.send(Message::text(notif.to_string())).await.is_err() {
            return true;
        }
    }
    if let Some(reply) = reply
        && ws.send(Message::text(reply.to_string())).await.is_err()
    {
        return true;
    }
    false
}

fn handle_message(shared: &Shared, text: &str) -> (Option<Value>, Vec<Value>) {
    let packet: Value = serde_json::from_str(text).unwrap_or_default();
    let to = packet
        .get("to")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let kind = packet
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();

    let mut state = shared.lock();
    state.requests.push(RecordedRdpRequest {
        to: to.clone(),
        request_type: kind.clone(),
        params: packet.clone(),
    });
    shared.request_arrived.notify_waiters();
    let (reply, notifications) = route_request(&mut state, &to, &kind, &packet);
    match state.before_reply.remove(&kind) {
        Some(mut first) => {
            first.extend(notifications);
            (reply, first)
        }
        None => (reply, notifications),
    }
}

fn route_request(
    state: &mut State,
    to: &str,
    kind: &str,
    packet: &Value,
) -> (Option<Value>, Vec<Value>) {
    if to == "root" || to.is_empty() {
        return (Some(handle_root(state, kind)), Vec::new());
    }
    if to == state.process_descriptor_actor {
        return (Some(handle_process_descriptor(to, kind)), Vec::new());
    }
    if to == state.parent_console_actor {
        return handle_parent_console(state, to, kind, packet);
    }
    if to == state.tab_descriptor_actor || kind == "getWatcher" {
        return (Some(answer_watcher(to, &state.watcher_actor)), Vec::new());
    }
    if to == state.watcher_actor {
        return handle_watcher(state, to, kind, packet);
    }
    if to == state.breakpoint_list_actor {
        return handle_breakpoints(state, to, kind, packet);
    }
    if to == state.thread_config_actor {
        return (Some(json!({ "from": to })), Vec::new());
    }
    if to == state.source_actor || to.contains("source") {
        return (Some(handle_source(state, to, kind, packet)), Vec::new());
    }
    if to == state.long_string_actor {
        return (
            Some(json!({ "from": to, "substring": "function hugeScript() { return 42; }" })),
            Vec::new(),
        );
    }
    if to == state.target.thread_actor || to.contains("thread") {
        return handle_thread(state, to, kind, packet);
    }
    if to == state.target.console_actor || to.contains("console") {
        return handle_console(state, to, kind, packet);
    }
    (Some(json!({ "from": to })), Vec::new())
}

fn handle_root(state: &State, kind: &str) -> Value {
    match kind {
        "listTabs" => {
            let mut reply = load(fx::RDP_LIST_TABS);
            reply["tabs"] = Value::Array(state.tabs.clone());
            reply
        }
        "getRoot" => load(fx::RDP_GET_ROOT),
        "getTab" => json!({ "from": "root", "tab": state.tabs.first().cloned() }),
        "getProcess" => load(fx::RDP_GET_PROCESS),
        _ => unrecognized("root", kind),
    }
}

fn unrecognized(to: &str, kind: &str) -> Value {
    let mut err = load(fx::RDP_UNRECOGNIZED);
    err["from"] = Value::from(to);
    err["message"] = Value::from(format!(
        "Actor {to} does not recognize the packet type '{kind}'"
    ));
    err
}

fn handle_process_descriptor(to: &str, kind: &str) -> Value {
    match kind {
        "getTarget" => load(fx::RDP_PARENT_PROCESS_TARGET),
        _ => unrecognized(to, kind),
    }
}

/// The parent-process console: evaluations are the `NavigableManager`
/// lookup, answered with the `browserId` mapped to the context id the text
/// carries as a JSON string literal, or `null`.
fn handle_parent_console(
    state: &mut State,
    to: &str,
    kind: &str,
    packet: &Value,
) -> (Option<Value>, Vec<Value>) {
    if kind != "evaluateJSAsync" {
        return (Some(json!({ "from": to })), Vec::new());
    }
    if state.parent_console_failures > 0 {
        state.parent_console_failures -= 1;
        let gone = json!({
            "from": to,
            "error": "noSuchActor",
            "message": format!("No such actor for ID: {to}"),
        });
        return (Some(gone), Vec::new());
    }
    let text = packet
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let browser_id = state
        .context_browser_ids
        .iter()
        .find(|(context, _)| text.contains(&Value::from(context.as_str()).to_string()))
        .map(|(_, id)| Value::from(*id));
    let result_id = format!("lookup-{}", state.requests.len());
    let mut reply = load(fx::RDP_EVALUATE_REPLY);
    reply["from"] = Value::from(to);
    reply["resultID"] = Value::from(result_id.as_str());
    let mut result = load(fx::RDP_BROWSER_ID_LOOKUP);
    result["from"] = Value::from(to);
    result["resultID"] = Value::from(result_id);
    result["input"] = Value::from(text);
    result["result"] =
        browser_id.unwrap_or_else(|| load(fx::RDP_BROWSER_ID_LOOKUP_NULL)["result"].clone());
    (Some(reply), vec![result])
}

fn answer_watcher(to: &str, watcher: &str) -> Value {
    let mut reply = load(fx::RDP_GET_WATCHER);
    reply["from"] = Value::from(to);
    reply["actor"] = Value::from(watcher);
    reply
}

fn handle_watcher(
    state: &mut State,
    to: &str,
    kind: &str,
    packet: &Value,
) -> (Option<Value>, Vec<Value>) {
    match kind {
        "getBreakpointListActor" => (
            Some(json!({ "from": to, "breakpointList": { "actor": state.breakpoint_list_actor } })),
            Vec::new(),
        ),
        "getThreadConfigurationActor" => (
            Some(json!({ "from": to, "configuration": { "actor": state.thread_config_actor } })),
            Vec::new(),
        ),
        "watchTargets" => (
            Some(json!({ "from": to })),
            vec![build_target_available(state, to)],
        ),
        "watchResources" => handle_watch_resources(state, to, packet),
        "unwatchResources" => {
            for t in extract_resource_types(packet) {
                state.watched_resources.remove(&t);
            }
            (None, Vec::new())
        }
        "unwatchTargets" => (None, Vec::new()),
        _ => (Some(json!({ "from": to })), Vec::new()),
    }
}

fn build_target_available(state: &State, to: &str) -> Value {
    let mut notif = load(fx::RDP_TARGET_AVAILABLE);
    notif["from"] = Value::from(to);
    notif["target"]["actor"] = Value::from(state.target.actor.clone());
    notif["target"]["threadActor"] = Value::from(state.target.thread_actor.clone());
    notif["target"]["consoleActor"] = Value::from(state.target.console_actor.clone());
    notif
}

fn handle_watch_resources(
    state: &mut State,
    to: &str,
    packet: &Value,
) -> (Option<Value>, Vec<Value>) {
    let types = extract_resource_types(packet);
    let mut notifs = Vec::new();
    if types.iter().any(|t| t == "source") {
        let mut notif = load(fx::RDP_RESOURCES_SOURCES);
        notif["from"] = Value::from(state.target.actor.clone());
        if let Some(sources) = notif["array"][0][1].as_array_mut() {
            sources.extend(state.added_sources.iter().cloned());
        }
        notifs.push(notif);
    }
    for t in types {
        state.watched_resources.insert(t);
    }
    (Some(json!({ "from": to })), notifs)
}

fn extract_resource_types(packet: &Value) -> Vec<String> {
    packet
        .get("resourceTypes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn handle_breakpoints(
    state: &mut State,
    to: &str,
    kind: &str,
    packet: &Value,
) -> (Option<Value>, Vec<Value>) {
    match kind {
        "setBreakpoint" => {
            state.breakpoints.push(packet.clone());
            (Some(json!({ "from": to })), Vec::new())
        }
        "removeBreakpoint" => {
            state.breakpoints.retain(|bp| bp != packet);
            (Some(json!({ "from": to })), Vec::new())
        }
        _ => (Some(json!({ "from": to })), Vec::new()),
    }
}

fn handle_source(state: &State, to: &str, kind: &str, packet: &Value) -> Value {
    match kind {
        "getBreakpointPositionsCompressed" => {
            let start = packet["query"]["start"]["line"].as_u64().unwrap_or(7);
            let end = packet["query"]["end"]["line"].as_u64().unwrap_or(start);
            let mut pos = load(fx::RDP_BREAKPOINT_POSITIONS);
            pos["from"] = Value::from(to);
            pos["positions"] = match state.source_positions.get(to) {
                Some(Value::Object(lines)) => Value::Object(
                    lines
                        .iter()
                        .filter(|(line, _)| {
                            line.parse::<u64>()
                                .is_ok_and(|line| line == start || (start..end).contains(&line))
                        })
                        .map(|(line, columns)| (line.clone(), columns.clone()))
                        .collect(),
                ),
                Some(_) => json!({}),
                None => json!({ start.to_string(): [ 10 ] }),
            };
            pos
        }
        "source" if to == "server1.conn0.source1" => load(fx::RDP_SOURCE_LONG_STRING),
        "source" => json!({ "from": to, "source": "function test() { return 1; }" }),
        _ => json!({ "from": to }),
    }
}

fn handle_thread(
    state: &mut State,
    to: &str,
    kind: &str,
    packet: &Value,
) -> (Option<Value>, Vec<Value>) {
    match kind {
        "frames" => {
            let mut frames = load(fx::RDP_FRAMES);
            frames["from"] = Value::from(to);
            (Some(frames), Vec::new())
        }
        "interrupt" => (Some(json!({ "from": to })), Vec::new()),
        "resume" => handle_resume(state, to, packet),
        _ => (Some(json!({ "from": to })), Vec::new()),
    }
}

fn handle_resume(state: &mut State, to: &str, packet: &Value) -> (Option<Value>, Vec<Value>) {
    let mut resumed = load(fx::RDP_THREAD_STATE_RESUMED);
    resumed["from"] = Value::from(to);
    if packet.get("resumeLimit").is_none() || packet["resumeLimit"].is_null() {
        let mut thread_state = load(fx::RDP_RESOURCES_THREAD_STATE_RESUMED);
        thread_state["from"] = Value::from(state.target.actor.clone());
        return (Some(json!({ "from": to })), vec![resumed, thread_state]);
    }
    state.current_line += 1;
    let mut paused = load(fx::RDP_THREAD_STATE_PAUSED);
    paused["from"] = Value::from(to);
    paused["why"] = json!({ "type": "resumeLimit" });
    paused["frame"]["where"]["line"] = Value::from(state.current_line);
    let combined = json!({
        "type": "resources-available-array",
        "from": state.target.actor,
        "array": [ [ "thread-state", [
            { "state": "resumed" },
            { "state": "paused", "why": { "type": "resumeLimit" }, "frame": paused["frame"].clone() }
        ] ] ]
    });
    (Some(json!({ "from": to })), vec![resumed, paused, combined])
}

fn handle_console(
    state: &State,
    to: &str,
    kind: &str,
    packet: &Value,
) -> (Option<Value>, Vec<Value>) {
    if kind == "evaluateJSAsync" {
        let result_id = format!("eval-{}", state.requests.len());
        let mut reply = load(fx::RDP_EVALUATE_REPLY);
        reply["from"] = Value::from(to);
        reply["resultID"] = Value::from(result_id.as_str());
        let mut notif = load(fx::RDP_EVALUATION_RESULT);
        notif["from"] = Value::from(to);
        notif["resultID"] = Value::from(result_id.as_str());
        if let Some(text) = packet.get("text").and_then(Value::as_str) {
            notif["input"] = Value::from(text);
        }
        (Some(reply), vec![notif])
    } else {
        (Some(json!({ "from": to })), Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bidi_lite::rdp::client::RdpClient;
    use bidi_lite::rdp::debugger::Debugger;
    use bidi_lite::rdp::resources::{
        Breakable, BreakpointLocation, parse_resources, parse_thread_states,
    };
    use std::time::Duration;

    const T: Duration = Duration::from_secs(5);

    /// MCK-06: MockRdp greets with the captured root packet.
    #[tokio::test]
    async fn greets_with_captured_root_packet() {
        let mock = MockRdp::start().await.unwrap();
        let client = RdpClient::connect(&format!("127.0.0.1:{}", mock.port()), T)
            .await
            .unwrap();
        let tabs = client.request("root", "listTabs", json!({})).await.unwrap();
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

    /// MCK-19: Debugger::attach_selected against MockRdp bootstraps and lists sources.
    #[tokio::test]
    async fn attach_selected_bootstraps_and_lists_sources() {
        let mock = MockRdp::start().await.unwrap();
        let client = RdpClient::connect(&format!("127.0.0.1:{}", mock.port()), T)
            .await
            .unwrap();
        let debugger = Debugger::attach_selected(&client).await.unwrap();
        assert_eq!(debugger.watcher(), "server1.conn1.watcher3");
        assert_eq!(
            debugger.thread_actor(),
            "server1.conn1.watcher2.process7//thread1"
        );
        assert_eq!(
            debugger.console_actor(),
            "server1.conn1.watcher2.process7//consoleActor3"
        );
        let sources = debugger.sources();
        assert!(!sources.is_empty());
        assert_eq!(
            sources[0].url.as_deref(),
            Some("https://example.com/test.html")
        );
    }

    /// MCK-20: set_breakpoint with column None resolves the column from getBreakpointPositionsCompressed.
    #[tokio::test]
    async fn set_breakpoint_resolves_column() {
        let mock = MockRdp::start().await.unwrap();
        let client = RdpClient::connect(&format!("127.0.0.1:{}", mock.port()), T)
            .await
            .unwrap();
        let debugger = Debugger::attach_selected(&client).await.unwrap();
        let loc = BreakpointLocation {
            source_url: "https://example.com/test.html".to_string(),
            line: 7,
            column: None,
        };
        debugger.set_breakpoint(&loc, None).await.unwrap();
        let bp_requests = mock.requests_to(debugger.breakpoint_list_actor());
        let set_bp = bp_requests
            .iter()
            .find(|r| r.request_type == "setBreakpoint")
            .expect("setBreakpoint was recorded");
        assert_eq!(set_bp.params["location"]["column"], 10);
    }

    /// MCK-21: A scripted pause is seen once per pause by on_thread_state consumers that de-duplicate.
    #[tokio::test]
    async fn scripted_pause_announced_to_subscribers() {
        let mock = MockRdp::start().await.unwrap();
        let client = RdpClient::connect(&format!("127.0.0.1:{}", mock.port()), T)
            .await
            .unwrap();
        let debugger = Debugger::attach_selected(&client).await.unwrap();
        let mut raw_stream = client.subscribe();
        let mut thread_states = debugger.on_thread_state();

        mock.pause("breakpoint", None);

        let thread_pkt = tokio::time::timeout(Duration::from_secs(1), thread_states.next())
            .await
            .expect("thread state within timeout")
            .unwrap()
            .unwrap();
        let states = parse_thread_states(&thread_pkt);
        assert_eq!(states.len(), 1);

        let mut raw_count = 0;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
        while let Ok(Ok(_)) = tokio::time::timeout_at(deadline, raw_stream.recv()).await {
            raw_count += 1;
            if raw_count >= 2 {
                break;
            }
        }
        assert_eq!(raw_count, 2);
    }

    /// MCK-22: step_over yields the combined resumed and paused packets with the new location.
    #[tokio::test]
    async fn step_over_yields_combined_resumed_and_paused() {
        let mock = MockRdp::start().await.unwrap();
        let client = RdpClient::connect(&format!("127.0.0.1:{}", mock.port()), T)
            .await
            .unwrap();
        let debugger = Debugger::attach_selected(&client).await.unwrap();
        let mut thread_states = debugger.on_thread_state();

        debugger.step_over().await.unwrap();

        let step_pkt = tokio::time::timeout(Duration::from_secs(1), thread_states.next())
            .await
            .expect("step packet within timeout")
            .unwrap()
            .unwrap();
        let states = parse_thread_states(&step_pkt);
        assert_eq!(states.len(), 2);
    }

    /// MCK-23: evaluate_on_frame returns the grip from evaluationResult.
    #[tokio::test]
    async fn evaluate_on_frame_returns_result() {
        let mock = MockRdp::start().await.unwrap();
        let client = RdpClient::connect(&format!("127.0.0.1:{}", mock.port()), T)
            .await
            .unwrap();
        let debugger = Debugger::attach_selected(&client).await.unwrap();
        let evaluation = debugger
            .evaluate_on_frame(
                "server1.conn1.watcher2.process7//frame21",
                "window.testFn(10)",
            )
            .await
            .unwrap();
        assert_eq!(evaluation.result, 11);
    }

    /// MCK-24: detach sends removeBreakpoint for each breakpoint before the one-way unwatch requests.
    #[tokio::test]
    async fn detach_removes_breakpoints_before_unwatch() {
        let mock = MockRdp::start().await.unwrap();
        let client = RdpClient::connect(&format!("127.0.0.1:{}", mock.port()), T)
            .await
            .unwrap();
        let debugger = Debugger::attach_selected(&client).await.unwrap();
        let loc = BreakpointLocation {
            source_url: "https://example.com/test.html".to_string(),
            line: 7,
            column: Some(10),
        };
        debugger.set_breakpoint(&loc, None).await.unwrap();
        debugger.detach().await.unwrap();

        let waited = mock
            .wait_for_request("unwatchTargets", Duration::from_millis(500))
            .await;
        assert!(waited.is_some());

        let requests = mock.requests();
        let remove_idx = requests
            .iter()
            .position(|r| r.request_type == "removeBreakpoint")
            .expect("removeBreakpoint found");
        let unwatch_res_idx = requests
            .iter()
            .position(|r| r.request_type == "unwatchResources")
            .expect("unwatchResources found");
        let unwatch_targets_idx = requests
            .iter()
            .position(|r| r.request_type == "unwatchTargets")
            .expect("unwatchTargets found");

        assert!(remove_idx < unwatch_res_idx);
        assert!(remove_idx < unwatch_targets_idx);
    }

    /// MCK-25: MockRdp records requests with to, request_type, and params.
    #[tokio::test]
    async fn records_requests() {
        let mock = MockRdp::start().await.unwrap();
        let client = RdpClient::connect(&format!("127.0.0.1:{}", mock.port()), T)
            .await
            .unwrap();
        let _ = client.list_tabs().await.unwrap();

        let reqs = mock.requests();
        assert!(!reqs.is_empty());
        assert_eq!(reqs[0].to, "root");
        assert_eq!(reqs[0].request_type, "listTabs");

        let root_reqs = mock.requests_to("root");
        assert_eq!(root_reqs.len(), 1);

        let waited = mock
            .wait_for_request("listTabs", Duration::from_millis(500))
            .await;
        assert!(waited.is_some());

        mock.clear_requests();
        assert!(mock.requests().is_empty());
    }

    /// MCK-26: MockRdp navigate swaps the target with new thread and console actors.
    #[tokio::test]
    async fn navigate_swaps_target() {
        let mock = MockRdp::start().await.unwrap();
        let client = RdpClient::connect(&format!("127.0.0.1:{}", mock.port()), T)
            .await
            .unwrap();
        let debugger = Debugger::attach_selected(&client).await.unwrap();
        let old_thread = debugger.thread_actor();

        mock.navigate();

        mock.navigate();

        let changed = crate::test_support_wait(|| debugger.thread_actor() != old_thread).await;
        assert!(changed);
        assert_ne!(debugger.thread_actor(), old_thread);
        assert_ne!(debugger.thread_actor(), old_thread);
    }

    /// MCK-27: watchResources honours resourceTypes and does not send source if not requested.
    #[tokio::test]
    async fn watch_resources_honours_types() {
        let mock = MockRdp::start().await.unwrap();
        let client = RdpClient::connect(&format!("127.0.0.1:{}", mock.port()), T)
            .await
            .unwrap();
        let debugger = Debugger::attach_selected(&client).await.unwrap();
        let mut raw_resources = client.on_type("resources-available-array");

        debugger
            .watch_resources(&["websocket", "error-message"])
            .await
            .unwrap();

        let next_notif =
            tokio::time::timeout(Duration::from_millis(100), raw_resources.next()).await;
        assert!(next_notif.is_err());

        let watched = mock.watched_resources();
        assert!(watched.contains(&"websocket".to_string()));
        assert!(watched.contains(&"error-message".to_string()));
    }

    /// MCK-28: emit_resources pushes resources-available-array and parse_resources extracts them.
    #[tokio::test]
    async fn emit_resources_delivers_to_subscribers() {
        let mock = MockRdp::start().await.unwrap();
        let client = RdpClient::connect(&format!("127.0.0.1:{}", mock.port()), T)
            .await
            .unwrap();
        let debugger = Debugger::attach_selected(&client).await.unwrap();
        debugger.watch_resources(&["websocket"]).await.unwrap();
        let mut ws_stream = debugger.on_resource("websocket");

        let items = parse_resources(
            &bidi_lite::rdp::protocol::RdpPacket::parse(fx::RDP_RESOURCES_WEBSOCKET).unwrap(),
            "websocket",
        );
        assert!(!items.is_empty());

        mock.emit_resources("websocket", items.clone());

        let packet = tokio::time::timeout(Duration::from_secs(1), ws_stream.next())
            .await
            .expect("received emitted resource within timeout")
            .unwrap()
            .unwrap();
        let parsed = parse_resources(&packet, "websocket");
        assert_eq!(parsed.len(), items.len());
        assert_eq!(parsed[0]["wsMessageType"], "webSocketOpened");
    }

    /// MCK-29: emit_resource_updates pushes resources-updated-array to subscribers.
    #[tokio::test]
    async fn emit_resource_updates_delivers_to_subscribers() {
        let mock = MockRdp::start().await.unwrap();
        let client = RdpClient::connect(&format!("127.0.0.1:{}", mock.port()), T)
            .await
            .unwrap();
        let debugger = Debugger::attach_selected(&client).await.unwrap();
        debugger.watch_resources(&["websocket"]).await.unwrap();
        let mut updates_stream = client.on_type("resources-updated-array");

        mock.emit_resource_updates(
            "websocket",
            vec![json!({ "httpChannelId": 38, "status": "updated" })],
        );

        let packet = tokio::time::timeout(Duration::from_secs(1), updates_stream.next())
            .await
            .expect("received update within timeout")
            .unwrap()
            .unwrap();
        let parsed = parse_resources(&packet, "websocket");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["httpChannelId"], 38);
    }

    /// MCK-30: emit_packet broadcasts arbitrary packets to subscribers.
    #[tokio::test]
    async fn emit_packet_broadcasts_raw_packet() {
        let mock = MockRdp::start().await.unwrap();
        let client = RdpClient::connect(&format!("127.0.0.1:{}", mock.port()), T)
            .await
            .unwrap();
        let mut custom_stream = client.on_type("custom-escape-hatch");

        mock.emit_packet(json!({
            "type": "custom-escape-hatch",
            "from": "customActor1",
            "payload": "custom-data"
        }));

        let packet = tokio::time::timeout(Duration::from_secs(1), custom_stream.next())
            .await
            .expect("received raw packet within timeout")
            .unwrap()
            .unwrap();
        assert_eq!(packet.r#type.as_deref(), Some("custom-escape-hatch"));
        assert_eq!(packet.from.as_deref(), Some("customActor1"));
        assert_eq!(packet.body["payload"], "custom-data");
    }

    /// MCK-31: emit_resources after navigate uses the new target actor.
    #[tokio::test]
    async fn emit_resources_after_navigate_uses_new_target() {
        let mock = MockRdp::start().await.unwrap();
        let client = RdpClient::connect(&format!("127.0.0.1:{}", mock.port()), T)
            .await
            .unwrap();
        let debugger = Debugger::attach_selected(&client).await.unwrap();
        let old_thread = debugger.thread_actor();

        mock.navigate();
        assert!(crate::test_support_wait(|| debugger.thread_actor() != old_thread).await);

        let mut notifs = client.on_type("resources-available-array");
        mock.emit_resources(
            "error-message",
            vec![json!({ "pageError": { "errorMessage": "nav error" } })],
        );

        let packet = tokio::time::timeout(Duration::from_secs(1), notifs.next())
            .await
            .expect("received resource within timeout")
            .unwrap()
            .unwrap();
        assert_ne!(
            packet.from.as_deref(),
            Some("server1.conn1.watcher2.process7//windowGlobalTarget2")
        );
        assert!(
            packet
                .from
                .as_deref()
                .unwrap()
                .contains("windowGlobalTarget3")
        );
    }

    /// MCK-32: Debugger attach_selected still bootstraps and lists sources.
    #[tokio::test]
    async fn attach_selected_still_lists_sources() {
        let mock = MockRdp::start().await.unwrap();
        let client = RdpClient::connect(&format!("127.0.0.1:{}", mock.port()), T)
            .await
            .unwrap();
        let debugger = Debugger::attach_selected(&client).await.unwrap();
        assert_eq!(debugger.sources().len(), 1);
        assert_eq!(
            debugger.sources()[0].url.as_deref(),
            Some("https://example.com/test.html")
        );
    }

    /// MCK-33: unwatch_resources records unwatch request with the specified types.
    #[tokio::test]
    async fn unwatch_resources_records_request() {
        let mock = MockRdp::start().await.unwrap();
        let client = RdpClient::connect(&format!("127.0.0.1:{}", mock.port()), T)
            .await
            .unwrap();
        let debugger = Debugger::attach_selected(&client).await.unwrap();
        debugger
            .watch_resources(&["websocket", "css-message"])
            .await
            .unwrap();
        assert!(mock.watched_resources().contains(&"websocket".to_string()));

        debugger.unwatch_resources(&["websocket"]).unwrap();

        let req = mock
            .wait_for_request("unwatchResources", Duration::from_millis(500))
            .await
            .expect("unwatchResources was recorded");
        assert_eq!(req.params["resourceTypes"], json!(["websocket"]));
        assert!(!mock.watched_resources().contains(&"websocket".to_string()));
        assert!(
            mock.watched_resources()
                .contains(&"css-message".to_string())
        );
    }

    /// The URL of the two-tab capture, two inline `<script>` blocks.
    const INLINE: &str = "http://127.0.0.1:44223/inline";

    async fn connected(mock: &MockRdp) -> RdpClient {
        RdpClient::connect(&format!("127.0.0.1:{}", mock.port()), T)
            .await
            .unwrap()
    }

    fn types_sent(mock: &MockRdp, request_type: &str) -> Vec<String> {
        mock.requests()
            .into_iter()
            .filter(|r| r.request_type == request_type)
            .map(|r| r.to)
            .collect()
    }

    /// MCK-34: A mapped BiDi context is looked up in the parent process, whose console is found once.
    #[tokio::test]
    async fn mapped_context_is_looked_up_in_the_parent_process() {
        // Given a mock mapping the context "ctx-1" to browserId 2
        let mock = MockRdp::start().await.unwrap();
        mock.map_context("ctx-1", 2);
        let client = connected(&mock).await;

        // When the context is looked up twice
        let first = client.browser_id_of_context("ctx-1").await.unwrap();
        let second = client.browser_id_of_context("ctx-1").await.unwrap();

        // Then both answer 2, and the parent console was fetched only once
        assert_eq!((first, second), (Some(2), Some(2)));
        assert_eq!(types_sent(&mock, "getProcess"), ["root"]);
        assert_eq!(
            types_sent(&mock, "getTarget"),
            ["server1.conn0.processDescriptor4"]
        );
        assert_eq!(
            types_sent(&mock, "evaluateJSAsync"),
            ["server1.conn0.consoleActor7"; 2]
        );
    }

    /// MCK-35: An unmapped context has no tab, so attach_to_context fails with no such frame before getWatcher.
    #[tokio::test]
    async fn unmapped_context_has_no_tab() {
        // Given a mock that maps no context
        let mock = MockRdp::start().await.unwrap();
        let client = connected(&mock).await;

        // When the debugger of an unknown context is attached
        let found = client.tab_for_context("ctx-9").await.unwrap();
        let attached = Debugger::attach_to_context(&client, "ctx-9").await;

        // Then no tab is found, the attach fails with no such frame, and no watcher is asked
        assert!(found.is_none());
        assert!(
            matches!(&attached, Err(bidi_lite::error::BidiError::ProtocolError { error, .. }) if error == "no such frame"),
            "{:?}",
            attached.err()
        );
        assert!(types_sent(&mock, "getWatcher").is_empty());
    }

    /// MCK-36: Two tabs on the same URL: each context attaches to the descriptor with its browserId.
    #[tokio::test]
    async fn same_url_tabs_attach_by_browser_id() {
        // Given the captured three tabs, the last two on the same URL, and their contexts mapped
        let mock = MockRdp::start().await.unwrap();
        mock.set_tabs(tabs_of(fx::RDP_LIST_TABS_TWO_TABS));
        mock.map_context("ctx-a", 11);
        mock.map_context("ctx-b", 12);
        let client = connected(&mock).await;

        // When each context's tab is looked up and its debugger attached
        let tab_a = client.tab_for_context("ctx-a").await.unwrap().unwrap();
        let tab_b = client.tab_for_context("ctx-b").await.unwrap().unwrap();
        Debugger::attach_to_context(&client, "ctx-b").await.unwrap();
        Debugger::attach_to_context(&client, "ctx-a").await.unwrap();

        // Then each maps to its own descriptor, whatever the shared URL
        assert_eq!((tab_a.url.as_str(), tab_b.url.as_str()), (INLINE, INLINE));
        assert_eq!(
            (tab_a.browsing_context_id, tab_b.browsing_context_id),
            (Some(12), Some(13))
        );
        assert_eq!(
            types_sent(&mock, "getWatcher"),
            [
                "server1.conn0.tabDescriptor3",
                "server1.conn0.tabDescriptor2"
            ]
        );
    }

    /// MCK-37: A parent console answering noSuchActor once is fetched again and the lookup retried.
    #[tokio::test]
    async fn gone_parent_console_is_fetched_again() {
        // Given a looked-up context, then a parent console that answers noSuchActor once
        let mock = MockRdp::start().await.unwrap();
        mock.map_context("ctx-1", 2);
        let client = connected(&mock).await;
        client.browser_id_of_context("ctx-1").await.unwrap();
        mock.fail_parent_console_once();

        // When the context is looked up again
        let found = client.browser_id_of_context("ctx-1").await.unwrap();

        // Then the lookup succeeds after fetching the parent process again
        assert_eq!(found, Some(2));
        assert_eq!(types_sent(&mock, "getProcess").len(), 2);
        assert_eq!(types_sent(&mock, "evaluateJSAsync").len(), 3);
    }

    fn inline_page(mock: &MockRdp) -> (String, String) {
        let positions = |fixture| load(fixture)["positions"].clone();
        let first = mock.add_source(INLINE, positions(fx::RDP_BREAKPOINT_POSITIONS_INLINE_NONE));
        let second = mock.add_source(
            INLINE,
            positions(fx::RDP_BREAKPOINT_POSITIONS_INLINE_SECOND),
        );
        (first, second)
    }

    /// MCK-38: A column-less breakpoint on a line only the second same-URL source covers binds there.
    #[tokio::test]
    async fn columnless_breakpoint_binds_in_the_second_source() {
        // Given two sources of one page, only the second breakable on line 9 (column 2)
        let mock = MockRdp::start().await.unwrap();
        let (first, second) = inline_page(&mock);
        let client = connected(&mock).await;
        let debugger = Debugger::attach_selected(&client).await.unwrap();

        // When a breakpoint is set on line 9 without a column
        let location = BreakpointLocation {
            source_url: INLINE.to_string(),
            line: 9,
            column: None,
        };
        let bound = debugger
            .set_breakpoint_resolved(&location, None)
            .await
            .unwrap();

        // Then both sources were asked, in order, and it binds at column 2
        assert_eq!(
            types_sent(&mock, "getBreakpointPositionsCompressed"),
            [first, second]
        );
        assert_eq!((bound.location.column, bound.resolved), (Some(2), true));
        let set = mock
            .wait_for_request("setBreakpoint", T)
            .await
            .expect("setBreakpoint was recorded");
        assert_eq!(set.params["location"]["column"], 2);
    }

    /// MCK-39: breakable_column tells a line no source covers from a URL no source has.
    #[tokio::test]
    async fn breakable_column_reports_nowhere_and_not_loaded() {
        // Given two sources of one page, breakable only on line 9
        let mock = MockRdp::start().await.unwrap();
        inline_page(&mock);
        let client = connected(&mock).await;
        let debugger = Debugger::attach_selected(&client).await.unwrap();

        // When lines 9 and 8 of the page and a line of an unknown URL are asked
        let nine = debugger.breakable_column(INLINE, 9).await.unwrap();
        let eight = debugger.breakable_column(INLINE, 8).await.unwrap();
        let unknown = debugger
            .breakable_column("https://example.com/none.js", 1)
            .await
            .unwrap();

        // Then they are At(2), Nowhere and NotLoaded
        assert_eq!(
            (nine, eight, unknown),
            (Breakable::At(2), Breakable::Nowhere, Breakable::NotLoaded)
        );
    }

    /// MCK-40: A source added while sources are watched is announced at once.
    #[tokio::test]
    async fn source_added_while_watched_is_announced() {
        // Given an attached debugger watching sources
        let mock = MockRdp::start().await.unwrap();
        let client = connected(&mock).await;
        let debugger = Debugger::attach_selected(&client).await.unwrap();

        // When a source is added
        let actor = mock.add_source(INLINE, json!({ "9": [2] }));

        // Then the debugger caches it
        let announced =
            crate::test_support_wait(|| debugger.sources().iter().any(|s| s.actor == actor)).await;
        assert!(announced);
    }

    /// MCK-41: A forwardingCancelled from root before the listTabs reply is not taken as the reply.
    #[tokio::test]
    async fn forwarding_cancelled_before_list_tabs_is_skipped() {
        // Given a forwardingCancelled queued before the next listTabs reply
        let mock = MockRdp::start().await.unwrap();
        let client = connected(&mock).await;
        mock.emit_before_reply("listTabs", load(fx::RDP_FORWARDING_CANCELLED));

        // When the tabs are listed
        let tabs = client.list_tabs().await.unwrap();

        // Then the real reply is returned
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].actor, "server1.conn1.tabDescriptor1");
    }
}

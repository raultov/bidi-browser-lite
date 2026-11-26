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

/// An iframe of another origin: a target of its own, and its sources.
struct MockFrame {
    target: TargetState,
    url: String,
    sources: Vec<Value>,
}

/// The `targetType` of dedicated workers.
const DEDICATED_WORKER: &str = "worker";
/// The `targetType` of service workers.
const SERVICE_WORKER: &str = "service_worker";

/// A worker: a target of its own, announced while its kind is watched.
struct MockWorker {
    target: TargetState,
    /// [`DEDICATED_WORKER`] or [`SERVICE_WORKER`].
    target_type: &'static str,
    url: String,
    id: String,
    /// The actor of its script, announced as its source.
    source_actor: String,
}

/// The worker `targetType` `target_type` names, if it names one.
fn worker_kind(target_type: Option<&str>) -> Option<&'static str> {
    match target_type {
        Some(DEDICATED_WORKER) => Some(DEDICATED_WORKER),
        Some(SERVICE_WORKER) => Some(SERVICE_WORKER),
        _ => None,
    }
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
    targets_watched: bool,
    frames: Vec<MockFrame>,
    frame_count: usize,
    workers: Vec<MockWorker>,
    worker_count: usize,
    watched_workers: HashSet<&'static str>,
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
            targets_watched: false,
            frames: Vec::new(),
            frame_count: 0,
            workers: Vec::new(),
            worker_count: 0,
            watched_workers: HashSet::new(),
        }
    }

    /// Sends `packet` to every connection.
    fn broadcast(&self, packet: &Value) {
        for tx in self.senders.values() {
            let _ = tx.send(packet.clone());
        }
    }

    /// The target whose thread actor is `thread`: a frame's or a worker's,
    /// else the tab's.
    fn target_of_thread(&self, thread: &str) -> &TargetState {
        let frames = self.frames.iter().map(|frame| &frame.target);
        let workers = self.workers.iter().map(|worker| &worker.target);
        frames
            .chain(workers)
            .find(|target| target.thread_actor == thread)
            .unwrap_or(&self.target)
    }

    /// Announces `worker` (its `target-available-form`, then its source if
    /// sources are watched), after the reply being sent.
    fn announce_worker(&self, worker: &MockWorker) {
        self.broadcast(&worker_form(
            &self.watcher_actor,
            worker,
            "target-available-form",
        ));
        if self.watched_resources.contains("source") {
            self.broadcast(&worker_sources(worker));
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
        let target = state.target.clone();
        announce_pause(&state, &target, why, frame);
    }

    /// Announces a pause of the thread of the iframe `frame_target` (see
    /// [`MockRdp::add_frame`]), as [`MockRdp::pause`] does for the tab's.
    pub fn pause_frame(&self, frame_target: &str, why: &str, frame: Option<Value>) {
        let state = self.shared.lock();
        let found = state.frames.iter().find(|f| f.target.actor == frame_target);
        if let Some(target) = found.map(|f| f.target.clone()) {
            announce_pause(&state, &target, why, frame);
        }
    }

    /// Adds an iframe of another origin showing `url`: a target of its own,
    /// with its own thread and console, as Firefox runs such frames in
    /// another process. It is announced (`target-available-form` with
    /// `isTopLevelTarget: false`) at once if targets are watched, and on each
    /// later `watchTargets`. Its thread answers like the tab's. Returns its
    /// target actor.
    pub fn add_frame(&self, url: &str) -> String {
        let mut state = self.shared.lock();
        state.frame_count += 1;
        let process = format!("server1.conn1.watcher2.process{}", 20 + state.frame_count);
        let frame = MockFrame {
            target: TargetState {
                actor: format!("{process}//windowGlobalTarget2"),
                thread_actor: format!("{process}//thread1"),
                console_actor: format!("{process}//consoleActor3"),
            },
            url: url.to_owned(),
            sources: Vec::new(),
        };
        let actor = frame.target.actor.clone();
        if state.targets_watched {
            state.broadcast(&frame_available(&state.watcher_actor, &frame));
        }
        state.frames.push(frame);
        actor
    }

    /// Adds a source with `url` to the iframe `frame_target`, whose
    /// `getBreakpointPositionsCompressed` answers `positions` (as
    /// [`MockRdp::add_source`]). It is announced from the frame's target at
    /// once if sources are watched, and on each later `watchResources`.
    /// Returns its actor.
    pub fn add_frame_source(&self, frame_target: &str, url: &str, positions: Value) -> String {
        let mut state = self.shared.lock();
        let watched = state.watched_resources.contains("source");
        let Some(frame) = state
            .frames
            .iter_mut()
            .find(|f| f.target.actor == frame_target)
        else {
            return String::new();
        };
        let process = frame_target.split("//").next().unwrap_or_default();
        let actor = format!("{process}//source{}", frame.sources.len() + 1);
        let resource = source_resource(&actor, url);
        frame.sources.push(resource.clone());
        let packet = sources_packet(frame_target, vec![resource]);
        state.source_positions.insert(actor.clone(), positions);
        if watched {
            state.broadcast(&packet);
        }
        actor
    }

    /// Starts a dedicated worker running the script `url`: a target of its
    /// own (`targetType: "worker"`), with its own thread and console, and the
    /// script as its source. Like Firefox, it is announced only while
    /// dedicated workers are watched (`watchTargets` with `targetType:
    /// "worker"`), *after* the reply: at once if they are, and on each later
    /// such `watchTargets`. Returns its target actor.
    pub fn add_worker(&self, url: &str) -> String {
        self.start_worker(DEDICATED_WORKER, url)
    }

    /// Starts a service worker running the script `url`, as
    /// [`MockRdp::add_worker`] does a dedicated one (`targetType:
    /// "service_worker"`). Returns its target actor.
    pub fn add_service_worker(&self, url: &str) -> String {
        self.start_worker(SERVICE_WORKER, url)
    }

    /// Ends the worker `worker_target` (terminated, its document gone, or a
    /// service worker Firefox stopped), announcing its whole form as a
    /// `target-destroyed-form`, as Firefox does.
    pub fn remove_worker(&self, worker_target: &str) {
        let mut state = self.shared.lock();
        let Some(index) = state
            .workers
            .iter()
            .position(|w| w.target.actor == worker_target)
        else {
            return;
        };
        let worker = state.workers.remove(index);
        state.broadcast(&worker_form(
            &state.watcher_actor,
            &worker,
            "target-destroyed-form",
        ));
    }

    /// Emits a `resources-available-array` packet of `resource_type` from the
    /// target `target` (a frame's or a worker's, or the tab's): e.g. a
    /// worker's `console-message` resources, which Firefox sends from the
    /// worker's own target.
    pub fn emit_target_resources(&self, target: &str, resource_type: &str, resources: Vec<Value>) {
        let packet = json!({
            "type": "resources-available-array",
            "from": target,
            "array": [ [ resource_type, resources ] ]
        });
        self.shared.lock().broadcast(&packet);
    }

    fn start_worker(&self, target_type: &'static str, url: &str) -> String {
        let mut state = self.shared.lock();
        state.worker_count += 1;
        let n = state.worker_count;
        let base = format!("server1.conn1.watcher2.process{}//workerTarget{n}", 40 + n);
        let worker = MockWorker {
            target: TargetState {
                actor: format!("{base}/workerTarget5"),
                thread_actor: format!("{base}/thread1"),
                console_actor: format!("{base}/console2"),
            },
            target_type,
            url: url.to_owned(),
            id: format!("mock-worker-{n}"),
            source_actor: format!("{base}/source6"),
        };
        if state.watched_workers.contains(target_type) {
            state.announce_worker(&worker);
        }
        let actor = worker.target.actor.clone();
        state.workers.push(worker);
        actor
    }

    /// Removes the iframe `frame_target`, announcing its
    /// `target-destroyed-form`.
    pub fn remove_frame(&self, frame_target: &str) {
        let mut state = self.shared.lock();
        state.frames.retain(|f| f.target.actor != frame_target);
        let mut destroyed = load(fx::RDP_TARGET_DESTROYED);
        destroyed["from"] = Value::from(state.watcher_actor.as_str());
        destroyed["target"]["actor"] = Value::from(frame_target);
        destroyed["target"]["isTopLevelTarget"] = Value::from(false);
        state.broadcast(&destroyed);
    }
}

/// Announces a pause of `target`'s thread twice: as a `paused` packet from
/// the thread and as a `thread-state` resource from the target.
fn announce_pause(state: &State, target: &TargetState, why: &str, frame: Option<Value>) {
    {
        let frame_val = frame.unwrap_or_else(|| {
            let mut default_frame = load(fx::RDP_THREAD_STATE_PAUSED)["frame"].clone();
            default_frame["where"]["line"] = Value::from(state.current_line);
            default_frame["where"]["column"] = Value::from(state.current_column);
            default_frame
        });
        let paused_packet = json!({
            "type": "paused",
            "actor": format!("{}//pause1", target.thread_actor),
            "from": target.thread_actor,
            "why": { "type": why, "actors": [Value::Null] },
            "frame": frame_val.clone(),
        });
        let mut resource_packet = load(fx::RDP_RESOURCES_THREAD_STATE_PAUSED);
        resource_packet["from"] = Value::from(target.actor.clone());
        resource_packet["array"][0][1][0]["why"]["type"] = Value::from(why);
        resource_packet["array"][0][1][0]["frame"] = frame_val;
        state.broadcast(&paused_packet);
        state.broadcast(&resource_packet);
    }
}

impl MockRdp {
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
        let resource = source_resource(&actor, url);
        state.added_sources.push(resource.clone());
        state.source_positions.insert(actor.clone(), positions);
        if state.watched_resources.contains("source") {
            state.broadcast(&sources_packet(&state.target.actor, vec![resource]));
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

/// A `source` resource with `actor` and `url`, shaped like the captured one.
fn source_resource(actor: &str, url: &str) -> Value {
    let mut resource = load(fx::RDP_RESOURCES_SOURCES)["array"][0][1][0].clone();
    resource["actor"] = Value::from(actor);
    resource["url"] = Value::from(url);
    resource["sourceMapBaseURL"] = Value::from(url);
    resource
}

/// The `resources-available-array` announcing `sources` from target `from`.
fn sources_packet(from: &str, sources: Vec<Value>) -> Value {
    json!({
        "type": "resources-available-array",
        "from": from,
        "array": [ [ "source", sources ] ]
    })
}

/// The captured `target-available-form` of an iframe, as `frame`'s, from
/// the watcher `watcher`.
fn frame_available(watcher: &str, frame: &MockFrame) -> Value {
    let mut form = load(fx::RDP_FRAME_TARGET_AVAILABLE);
    form["from"] = Value::from(watcher);
    form["target"]["actor"] = Value::from(frame.target.actor.as_str());
    form["target"]["threadActor"] = Value::from(frame.target.thread_actor.as_str());
    form["target"]["consoleActor"] = Value::from(frame.target.console_actor.as_str());
    form["target"]["url"] = Value::from(frame.url.as_str());
    form
}

/// The captured form of a worker of `worker`'s kind, as `worker`'s, from the
/// watcher `watcher`, of `form_type` (`target-available-form` or
/// `target-destroyed-form`).
fn worker_form(watcher: &str, worker: &MockWorker, form_type: &str) -> Value {
    let captured = match worker.target_type {
        SERVICE_WORKER => fx::RDP_SERVICE_WORKER_TARGET_AVAILABLE,
        _ => fx::RDP_WORKER_TARGET_AVAILABLE,
    };
    let mut form = load(captured);
    form["type"] = Value::from(form_type);
    form["from"] = Value::from(watcher);
    let target = &mut form["target"];
    target["actor"] = Value::from(worker.target.actor.as_str());
    target["threadActor"] = Value::from(worker.target.thread_actor.as_str());
    target["consoleActor"] = Value::from(worker.target.console_actor.as_str());
    target["url"] = Value::from(worker.url.as_str());
    target["id"] = Value::from(worker.id.as_str());
    form
}

/// The `source` resource of `worker`'s script, from its target.
fn worker_sources(worker: &MockWorker) -> Value {
    let source = source_resource(&worker.source_actor, &worker.url);
    sources_packet(&worker.target.actor, vec![source])
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
        "watchTargets" => match worker_kind(packet["targetType"].as_str()) {
            Some(kind) => {
                watch_workers(state, kind);
                (Some(json!({ "from": to })), Vec::new())
            }
            None => {
                state.targets_watched = true;
                let mut forms = vec![build_target_available(state, to)];
                forms.extend(state.frames.iter().map(|frame| frame_available(to, frame)));
                (Some(json!({ "from": to })), forms)
            }
        },
        "watchResources" => handle_watch_resources(state, to, packet),
        "unwatchResources" => {
            for t in extract_resource_types(packet) {
                state.watched_resources.remove(&t);
            }
            (None, Vec::new())
        }
        "unwatchTargets" => {
            if let Some(kind) = worker_kind(packet["targetType"].as_str()) {
                state.watched_workers.remove(kind);
            }
            (None, Vec::new())
        }
        _ => (Some(json!({ "from": to })), Vec::new()),
    }
}

/// Watches the workers of `kind`, announcing those running after the reply.
fn watch_workers(state: &mut State, kind: &'static str) {
    state.watched_workers.insert(kind);
    for worker in state.workers.iter().filter(|w| w.target_type == kind) {
        state.announce_worker(worker);
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
        let frames = state
            .frames
            .iter()
            .filter(|frame| !frame.sources.is_empty());
        notifs
            .extend(frames.map(|frame| sources_packet(&frame.target.actor, frame.sources.clone())));
        let workers = state
            .workers
            .iter()
            .filter(|w| state.watched_workers.contains(w.target_type));
        notifs.extend(workers.map(worker_sources));
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
        thread_state["from"] = Value::from(state.target_of_thread(to).actor.clone());
        return (Some(json!({ "from": to })), vec![resumed, thread_state]);
    }
    state.current_line += 1;
    let mut paused = load(fx::RDP_THREAD_STATE_PAUSED);
    paused["from"] = Value::from(to);
    paused["why"] = json!({ "type": "resumeLimit" });
    paused["frame"]["where"]["line"] = Value::from(state.current_line);
    let combined = json!({
        "type": "resources-available-array",
        "from": state.target_of_thread(to).actor,
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

    /// The script of the mock's iframe of another origin.
    const FRAME_SCRIPT: &str = "http://localhost:8765/inner.js";

    /// MCK-42: An iframe of another origin is announced with its own target
    /// and sources, whether added before or after the debugger attached, and
    /// is gone once removed.
    #[tokio::test]
    async fn frames_are_announced_with_their_sources() {
        // Given a frame added before the debugger attaches, with a source
        let mock = MockRdp::start().await.unwrap();
        let early = mock.add_frame("http://localhost:8765/inner.html");
        let early_source = mock.add_frame_source(&early, FRAME_SCRIPT, json!({ "2": [16] }));
        let client = connected(&mock).await;
        let debugger = Debugger::attach_selected(&client).await.unwrap();

        // Then it is known with its source, apart from the tab's
        let known = || {
            debugger
                .frame_sources(&early)
                .iter()
                .any(|s| s.actor == early_source)
        };
        assert!(crate::test_support_wait(known).await);
        assert!(
            debugger
                .sources()
                .iter()
                .all(|s| s.url.as_deref() != Some(FRAME_SCRIPT))
        );
        let frame = &debugger.frame_targets()[0];
        assert_ne!(frame.thread_actor, debugger.thread_actor());

        // When another frame is added after, with a source, and the first is removed
        let late = mock.add_frame("http://localhost:8765/other.html");
        let late_source = mock.add_frame_source(&late, FRAME_SCRIPT, json!({}));
        mock.remove_frame(&early);

        // Then only the late frame remains, with its source
        let late_known = || {
            debugger
                .frame_sources(&late)
                .iter()
                .any(|s| s.actor == late_source)
        };
        assert!(crate::test_support_wait(late_known).await);
        let only_late = || {
            debugger
                .frame_targets()
                .iter()
                .map(|f| f.actor.clone())
                .eq([late.clone()])
        };
        assert!(crate::test_support_wait(only_late).await);
    }

    /// MCK-43: A frame's thread pauses from its own target, and resumes
    /// through its own thread actor.
    #[tokio::test]
    async fn frame_threads_pause_and_resume_on_their_own() {
        // Given an attached debugger and a frame
        let mock = MockRdp::start().await.unwrap();
        let frame = mock.add_frame("http://localhost:8765/inner.html");
        let client = connected(&mock).await;
        let debugger = Debugger::attach_selected(&client).await.unwrap();
        let known = || debugger.thread_of(&frame).is_some();
        assert!(crate::test_support_wait(known).await);
        let mut states = debugger.on_any_thread_state();

        // When the frame pauses at a breakpoint
        mock.pause_frame(&frame, "breakpoint", None);

        // Then the pause comes from the frame's target
        let packet = tokio::time::timeout(T, states.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(packet.from.as_deref(), Some(frame.as_str()));
        assert!(matches!(
            parse_thread_states(&packet).as_slice(),
            [bidi_lite::rdp::resources::ThreadState::Paused { .. }]
        ));

        // When the frame's thread resumes
        let thread = debugger.thread_of(&frame).unwrap();
        thread.resume().await.unwrap();

        // Then the request went to its thread, and its target says it resumed
        let resumed = mock.wait_for_request("resume", T).await.unwrap();
        assert_eq!(resumed.to, thread.thread_actor());
        let packet = tokio::time::timeout(T, states.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(packet.from.as_deref(), Some(frame.as_str()));
    }

    /// MCK-44: A column-less breakpoint on a frame's script binds at that
    /// source's first breakable column.
    #[tokio::test]
    async fn breakpoints_bind_in_frame_sources() {
        // Given a frame whose inner.js breaks at line 2, column 16
        let mock = MockRdp::start().await.unwrap();
        let frame = mock.add_frame("http://localhost:8765/inner.html");
        let source = mock.add_frame_source(&frame, FRAME_SCRIPT, json!({ "2": [16] }));
        let client = connected(&mock).await;
        let debugger = Debugger::attach_selected(&client).await.unwrap();
        let known = || !debugger.frame_sources(&frame).is_empty();
        assert!(crate::test_support_wait(known).await);

        // When a breakpoint is set on line 2 without a column
        let location = BreakpointLocation {
            source_url: FRAME_SCRIPT.to_string(),
            line: 2,
            column: None,
        };
        let bound = debugger
            .set_breakpoint_resolved(&location, None)
            .await
            .unwrap();

        // Then the frame's source was asked, and it binds at column 16
        assert_eq!(
            types_sent(&mock, "getBreakpointPositionsCompressed"),
            [source]
        );
        assert_eq!((bound.location.column, bound.resolved), (Some(16), true));
    }

    /// The scripts of the mock's workers.
    const WORKER_SCRIPT: &str = "http://127.0.0.1:8765/worker.js";
    const SERVICE_WORKER_SCRIPT: &str = "http://127.0.0.1:8765/sw.js";

    /// The resources of a captured `resources-available-array` fixture.
    fn captured(fixture: &str) -> Vec<Value> {
        match load(fixture)["array"][0][1].take() {
            Value::Array(resources) => resources,
            _ => Vec::new(),
        }
    }

    /// A debugger on `mock` following both kinds of workers, once `ready` holds.
    async fn watching_workers(mock: &MockRdp, ready: impl Fn(&Debugger) -> bool) -> Debugger {
        let client = connected(mock).await;
        let debugger = Debugger::attach_selected(&client).await.unwrap();
        debugger.watch_workers().await.unwrap();
        debugger.watch_service_workers().await.unwrap();
        assert!(crate::test_support_wait(|| ready(&debugger)).await);
        debugger
    }

    /// MCK-45: Dedicated and service workers are announced as targets of
    /// their kind once watched, with their script as a source, whether added
    /// before or after; a removed worker is gone.
    #[tokio::test]
    async fn workers_are_announced_with_their_sources() {
        // Given a dedicated worker and a service worker started before the debugger watches them
        let mock = MockRdp::start().await.unwrap();
        let early = mock.add_worker(WORKER_SCRIPT);
        let service = mock.add_service_worker(SERVICE_WORKER_SCRIPT);

        // When the debugger watches both kinds
        let debugger = watching_workers(&mock, |d| {
            d.worker_sources(&early).len() == 1 && d.service_worker_sources(&service).len() == 1
        })
        .await;

        // Then each is followed as its kind, with its script as its source
        assert_eq!(debugger.worker_targets()[0].actor, early);
        assert_eq!(debugger.service_worker_targets()[0].actor, service);
        let script = debugger.worker_sources(&early)[0].url.clone();
        assert_eq!(script.as_deref(), Some(WORKER_SCRIPT));

        // When another dedicated worker starts, and the first one ends
        let late = mock.add_worker("http://127.0.0.1:8765/late.js");
        mock.remove_worker(&early);

        // Then only the late one is followed, and the service worker still is
        let only_late = || {
            debugger
                .worker_targets()
                .iter()
                .map(|w| w.actor.clone())
                .eq([late.clone()])
        };
        assert!(crate::test_support_wait(only_late).await);
        assert_eq!(debugger.service_worker_targets().len(), 1);
    }

    /// MCK-46: A worker's resources are emitted from its own target: the
    /// captured console messages reach the stream of its kind.
    #[tokio::test]
    async fn worker_console_messages_come_from_their_targets() {
        // Given a debugger following a dedicated worker and a service worker
        let mock = MockRdp::start().await.unwrap();
        let worker = mock.add_worker(WORKER_SCRIPT);
        let service = mock.add_service_worker(SERVICE_WORKER_SCRIPT);
        let debugger = watching_workers(&mock, |d| {
            d.worker_targets().len() == 1 && d.service_worker_targets().len() == 1
        })
        .await;
        let mut dedicated = debugger.on_worker_resource("console-message");
        let mut services = debugger.on_service_worker_resource("console-message");

        // When the service worker logs, then the dedicated worker, as captured
        let sw_messages = captured(fx::RDP_SERVICE_WORKER_RESOURCES_CONSOLE_MESSAGE);
        mock.emit_target_resources(&service, "console-message", sw_messages);
        let messages = captured(fx::RDP_WORKER_RESOURCES_CONSOLE_MESSAGE);
        mock.emit_target_resources(&worker, "console-message", messages);

        // Then each kind's stream yields its own worker's messages
        let packet = tokio::time::timeout(T, dedicated.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(packet.from.as_deref(), Some(worker.as_str()));
        let logged = parse_resources(&packet, "console-message");
        assert_eq!(logged[1]["level"], "error");
        let packet = tokio::time::timeout(T, services.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(packet.from.as_deref(), Some(service.as_str()));
    }

    /// MCK-47: A worker's thread resumes from its own target.
    #[tokio::test]
    async fn worker_threads_resume_from_their_own_targets() {
        // Given a debugger following a dedicated worker, and a filter on its thread states
        let mock = MockRdp::start().await.unwrap();
        let worker = mock.add_worker(WORKER_SCRIPT);
        let debugger = watching_workers(&mock, |d| d.worker_targets().len() == 1).await;
        let mut states = debugger.on_worker_resource("thread-state");

        // When its thread resumes
        let thread = debugger.thread_of(&worker).unwrap();
        thread.resume().await.unwrap();

        // Then the request went to its thread, and its own target says it resumed
        let resumed = mock.wait_for_request("resume", T).await.unwrap();
        assert_eq!(resumed.to, thread.thread_actor());
        let packet = tokio::time::timeout(T, states.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(packet.from.as_deref(), Some(worker.as_str()));
    }

    /// MCK-48: Workers are announced only while their kind is watched.
    #[tokio::test]
    async fn workers_are_announced_only_while_watched() {
        // Given a debugger that watched dedicated workers, then stopped
        let mock = MockRdp::start().await.unwrap();
        let client = connected(&mock).await;
        let debugger = Debugger::attach_selected(&client).await.unwrap();
        debugger.watch_workers().await.unwrap();
        debugger.unwatch_workers().unwrap();
        assert!(mock.wait_for_request("unwatchTargets", T).await.is_some());
        let mut forms = client.on_type("target-available-form");

        // When a dedicated worker and a service worker start
        mock.add_worker(WORKER_SCRIPT);
        mock.add_service_worker(SERVICE_WORKER_SCRIPT);

        // Then neither is announced
        let quiet = tokio::time::timeout(Duration::from_millis(300), forms.next()).await;
        assert!(quiet.is_err(), "{quiet:?}");
    }
}

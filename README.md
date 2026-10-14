# bidi-browser-lite

Full lifecycle control of WebDriver BiDi browsers (Firefox) for Rust. It finds
Firefox, launches a managed instance or attaches to a running one, allocates
its ports, prepares its profile, owns its **single BiDi session**, hands out
[`bidi-lite`](https://crates.io/crates/bidi-lite) clients already connected to
it, and stops everything cleanly.

It is the BiDi twin of [`cdp-browser-lite`](https://crates.io/crates/cdp-browser-lite)
(Chrome/CDP): same concepts and the same type and method names wherever the
concept is the same. The differences are listed in
[Coming from cdp-browser-lite](#coming-from-cdp-browser-lite).

## Features

- **Discovery** on Linux (snap first), macOS and Windows; `FIREFOX_PATH` and
  `firefox_path(..)` override it.
- **Launch modes** `Auto`, `LaunchNew`, `AttachOnly`; **profile modes**
  `Ephemeral`, `Persistent`, `PersistentPerPort` (and `UserDefault` for
  attaching only).
- **Two ports per instance:** BiDi always, the Firefox debugger (RDP)
  **opt-in**; allocated without collisions under concurrency.
- **Side-effect free probes:** no probe ever creates a BiDi session.
- **One shared session** per browser behind `client()`, `browser_client()`
  and the tab helpers; recovery of a managed Firefox stuck with an orphaned
  session; `SessionBusy` for an attached one.
- **Clean stop:** `browser.close`, wait, terminate if needed, remove the
  ephemeral profile; never signals a browser it did not start.
- `restart`, `auto_relaunch`, `keep_alive_on_drop`, and a `BrowserPool`.
- **Test support** (feature `test-support`): in-process BiDi/RDP mocks built
  from frames captured from Firefox, and a `fake_firefox_helper` binary.

## Quick Start

```toml
[dependencies]
bidi-browser-lite = "0.1"
tokio = { version = "1", features = ["full"] }
```

```rust,no_run
use bidi_browser_lite::{Browser, BrowserConfig, LaunchMode, ReadinessState};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let config = BrowserConfig::builder().mode(LaunchMode::LaunchNew).build();
let browser = Browser::ensure(config).await?;

let tab = browser.new_tab("about:blank").await?;
tab.navigate("https://www.rust-lang.org", ReadinessState::Complete).await?;

// Closes Firefox and removes its ephemeral profile.
browser.stop().await?;
# Ok(())
# }
```

`new_tab`, `list_tabs`, `attach_tab`, `close_tab`, `client()` and
`browser_client()` give you the `bidi-lite` handles (`Tab`, `BrowserClient`,
`BidiClient`), re-exported at the crate root, so you depend on one crate.

## Debugging JavaScript (RDP, opt-in)

BiDi has no debugger. Firefox's own debugger protocol (RDP) runs on a second
port that this crate enables only when asked:

```rust,no_run
use bidi_browser_lite::{Browser, BrowserConfig, Debugger, LaunchMode};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let config = BrowserConfig::builder().mode(LaunchMode::LaunchNew).rdp(true).build();
let browser = Browser::ensure(config).await?;
let tab = browser.new_tab("https://www.rust-lang.org").await?;

// The debugger of the tab showing this BiDi context, found by `browserId`:
// the tab is not activated and tabs on the same URL are told apart.
let rdp = browser.rdp_client().await?;
let debugger = Debugger::attach_to_context(&rdp, tab.context_id()).await?;
// ... breakpoints, pause, frames, evaluation: see bidi-lite's Debugger ...
debugger.detach().await?;
browser.stop().await?;
# Ok(())
# }
```

See [Security](#security) before enabling RDP.

## Architecture

**Your application, this crate, bidi-lite, Firefox.** You call typed methods
and get `Result`s; this crate spawns and signals the Firefox process and owns
its profile files; the `bidi-lite` clients it hands out speak JSON over two
WebSockets.

```text
 ┌──────────────────────── your application (e.g. an MCP server) ─────────────────────────┐
 │ Browser::ensure(cfg) / BrowserPool::open(cfg)   browser.new_tab(..)  browser.stop()     │
 └───────────────┬─────────────────────────────────────────────────▲───────────────────────┘
                 │ method calls                                    │ Result<Browser | Tab |
                 ▼                                                 │ BrowserClient | RdpClient, BrowserError>
 ┌───────────────────────────────────── bidi-browser-lite ─────────┴───────────────────────┐
 │ config ─► validate ─► ensure ─┬─ probe     TCP; WS /session + session.status; RDP greet │
 │                               ├─ decide    LaunchMode × what answers (pure function)    │
 │                               ├─ ports     PortAllocator: BiDi + RDP pair, one lock     │
 │                               ├─ discovery FIREFOX_PATH, /snap/bin/firefox, PATH, ...   │
 │                               ├─ profile   dir, user.js managed block, port file, locks │
 │                               └─ process   build_args, spawn, readiness, terminate      │
 │ Browser { one tokio Mutex<BrowserState> }: process, profile, ports, managed, stopped,   │
 │           the single session (BrowserClient), cached RdpClient                          │
 └────────┬──────────────────────────────┬───────────────────────────────┬─────────────────┘
          │ spawn, SIGTERM/kill,          │ files: user.js,               │ bidi-lite clients
          │ exit code                     │ WebDriverBiDiServer.json, lock│ (clones share sockets)
 ┌────────▼──────────────────────────────▼──────────┐     ┌──────────────▼──────────────────┐
 │ Firefox process        profile directory          │◄────│ BidiClient / BrowserClient / Tab│
 │   :BIDI  ws://127.0.0.1:BIDI/session  (BiDi)      │     │   JSON commands ⇄ replies+events│
 │   :RDP   ws://127.0.0.1:RDP   (opt-in debugger)   │◄────│ RdpClient / Debugger            │
 └───────────────────────────────────────────────────┘     │   JSON requests ⇄ replies       │
                                                           └─────────────────────────────────┘
```

**Lifecycle.**

```text
 ensure(cfg) ─► validate ─► probe port ─► decide ─┬─ Attach ─► open session ───────────────────┐
                                                  └─ Launch ─► reserve ports ─► find Firefox    │
                                                       ─► prepare profile ─► spawn ─► ready? ─► open session
                                                                                                │
 ready ◄────────────────────────────────────────────────────────────────────────────────────────┘
   │ client()/browser_client(): session alive? ─ yes ─► clone
   │                              └ no ─► free? ─ yes ─► new session
   │                                       └ busy ─► managed: restart │ attached: SessionBusy
   ├ restart() ─► stop + launch again (fixed port kept) │ end + re-attach
   └ stop() ─► managed: browser.close ─► wait ─► terminate ─► remove ephemeral profile
               attached: session.end only
```

**Launch decision.** "Owned" means a running Firefox holds the per-port
profile of that port (`PersistentPerPort`: another manager's instance).

| Mode | Port | What answers | Action |
| --- | --- | --- | --- |
| `AttachOnly` | P | nothing, or not BiDi | `RemoteUnavailable` |
| `AttachOnly` | P | BiDi, session free | attach |
| `AttachOnly` | P | BiDi, session taken | `SessionBusy` |
| `LaunchNew` | 0 | (not probed) | launch; Firefox picks the port |
| `LaunchNew` | P | nothing | launch on P |
| `LaunchNew` | P | anything | `PortConflict` |
| `Auto` | P | nothing | launch on P |
| `Auto` | P | not BiDi, or owned (Unix only) | launch on a free port above P |
| `Auto` | P | BiDi, session free | attach |
| `Auto` | P | BiDi, session taken | `SessionBusy` |

## The one-session rule

Firefox allows **one** BiDi session, and it **outlives its WebSocket**: if
the client that created it dies without `session.end`, Firefox refuses new
sessions (`Session already started`) until it is restarted, and no other
connection can end it. This crate therefore:

- owns exactly one session per `Browser`; every client it hands out is a
  clone of it;
- never creates a session to probe (`session.status` on a session-less
  connection says whether one could be created);
- ends or closes the session on every stop path (`stop`, `restart`, drop,
  pool close);
- restarts a **managed** Firefox found with an orphaned session (it is
  ours), and returns `BrowserError::SessionBusy` for an **attached** one,
  which it never signals.

Always call `stop()` on an attached browser (and on one kept alive with
`keep_alive_on_drop`): dropping it only *tries* to end the session in a
background task, which never runs if the Tokio runtime is shutting down
(e.g. at the end of `#[tokio::main]`). A session that does not answer
`session.status` in time is reported as a `Timeout` and kept, never
replaced: replacing it would orphan our own session.

## Ports

- **BiDi**: `port(0)` (with `LaunchNew`) lets Firefox pick a free port; the
  real one is read from `WebDriverBiDiServer.json` in the profile and
  reported by `debug_address()`.
- **RDP** (opt-in): `rdp(true)` allocates a port, `rdp_port(p)` fixes one.
  Firefox treats `--start-debugger-server ws:0` as port 6000, so this crate
  always passes a real port. `rdp_address()` reports it.
- A `BrowserPool` or `ensure_with_allocator` shares a `PortAllocator` so
  concurrent launches never pick the same port.

## Profiles

| Mode | Directory | After `stop()` |
| --- | --- | --- |
| `Ephemeral` (default) | a new `bidi-browser-lite-*` dir: under `~/snap/firefox/common/bidi-browser-lite/profiles` for snap Firefox (the snap cannot see `/tmp`), the temp dir otherwise | removed (orphans older than 1 h are swept on Unix) |
| `Persistent(dir)` | `dir` | kept |
| `PersistentPerPort { root, prefix }` | `root/<prefix><port>` (needs a fixed port) | kept |
| `UserDefault` | the user's own profile | `AttachOnly` only: launching would write remote-control prefs into the real profile |

The library writes its prefs in a **managed block** of `user.js`
(`// BEGIN bidi-browser-lite` … `// END bidi-browser-lite`), rewritten on
every launch; anything you put outside the block is preserved. Your own
`pref(..)` values come last, so they win. Prefs that leave the block are
also removed from `prefs.js`, where Firefox keeps a copy. A persistent
profile held by a running Firefox is refused (`ProfileError::Locked`): a
second Firefox on it would silently hand the launch over to the running one.
The holder is read from Firefox's `lock` symlink (`IP:+PID`) on Linux and
from whether `parent.lock` is held open on Windows. On macOS Firefox may
only use an fcntl lock on `.parentlock`, which is not checked: there the
case surfaces as `EarlyExit { reason: ProfileInUse }` (untested).

## Coming from cdp-browser-lite

| cdp-browser-lite | bidi-browser-lite | Why |
| --- | --- | --- |
| `Browser`, `BrowserConfig(Builder)`, `LaunchMode`, `ProfileMode`, `BrowserPool`, `BrowserId`, `BrowserEntry`, `PortAllocator`, `PortSearch`, `PortReservation`, `BrowserError` | same names | same concepts |
| `ensure`, `ensure_with_allocator`, `client`, `browser_client`, `new_tab`, `attach_tab`, `attach_to_all_tabs`, `list_tabs`, `close_tab`, `stop`, `restart`, `is_managed`, `is_alive`, `debug_address`, `profile_dir`, `pid` | same names | same concepts |
| `client()` and `browser_client()`: two WebSockets | one WebSocket, one session, shared by every clone | Firefox allows one BiDi session |
| `attach_tab(target_id)` | `attach_tab(context_id)` | BiDi contexts instead of CDP targets |
| `chrome_path`, `CHROME_PATH` | `firefox_path`, `FIREFOX_PATH` | browser |
| — | `rdp(bool)`, `rdp_port(u16)`, `rdp_client()`, `rdp_address()` | Firefox debugger, opt-in |
| — | `pref(name, value)`, `prefs(..)`, `PrefValue` | Firefox is configured through prefs |
| `proxy(url)` → `--proxy-server` | `proxy(url)` → `network.proxy.*` prefs; `http://` and `socks5://`; credentials refused | Firefox has no proxy flag; prefs cannot hold credentials (answer auth with BiDi `network.continueWithAuth`) |
| `user_agent` → flag | `user_agent` → `general.useragent.override` | pref |
| `window_size` → `--window-size` | `window_size` → `--width`/`--height` | Firefox ignores `--window-size` |
| `enable_automation`, `no_sandbox` | not offered | no Firefox equivalent: `navigator.webdriver` is always true under remote control; headless Firefox runs in Docker without a sandbox switch |
| `ProfileMode::UserDefault` launches on the user's profile | `AttachOnly` only | safety |
| `probe::is_chrome_cdp` | `probe::is_firefox_bidi`, `bidi_session_available`, `is_firefox_rdp` | protocols |
| `process::ChromeProcess` | `process::FirefoxProcess` | browser |
| `DevToolsActivePort`, `SingletonLock` | `WebDriverBiDiServer.json`, `lock` symlink | browser |
| `BrowserError::Cdp`, `InvalidConfig(String)`, `Profile(String)`, `StartupTimeout { timeout }` | `Bidi`, `InvalidConfig(ConfigError)`, `Profile(ProfileError)`, `StartupTimeout { timeout, port }`; new `SessionBusy`, `RdpDisabled`; `EarlyExit { code, reason }` | typed errors, two ports, one session |
| `BrowserEntry { id, host, port, profile_dir, managed }` | adds `rdp_port` | two ports |
| `test_support::mock_devtools::{MockChrome, MockDevTools}`, `fake_chrome_helper` | `test_support::{MockBidi, MockRdp, MockFirefox}`, `fake_firefox_helper` | protocols |

## Test support for downstream crates

```toml
[dev-dependencies]
bidi-browser-lite = { version = "0.1", features = ["test-support"] }
```

```rust,no_run
use bidi_browser_lite::test_support::{MockBidiBehavior, MockFirefox};
use bidi_browser_lite::{Browser, BrowserConfig, BrowserError, Debugger, LaunchMode};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let firefox = MockFirefox::start(true).await?; // BiDi + RDP on 127.0.0.1:0
let config = BrowserConfig::builder()
    .mode(LaunchMode::AttachOnly)
    .port(firefox.bidi_port())
    .rdp_port(firefox.rdp_port().unwrap_or_default())
    .build();
let browser = Browser::ensure(config).await?;
let tab = browser.new_tab("https://example.com/").await?;
assert_eq!(firefox.bidi().count("session.new"), 1);

// Assert recorded command parameters:
let created = firefox.bidi().commands_named("browsingContext.create");
assert!(!created.is_empty());

// Drive the RDP debugger against the mock in-process. Map the tab's BiDi
// context to the browserId of the mock's tab descriptor (2) first:
firefox.rdp().unwrap().map_context(tab.context_id(), 2);
let rdp_client = browser.rdp_client().await?;
let debugger = Debugger::attach_to_context(&rdp_client, tab.context_id()).await?;
assert!(!debugger.sources().is_empty());

// Watch watcher resources, emit captured frames, and parse them:
debugger.watch_resources(&["websocket"]).await?;
let mut ws_stream = debugger.on_resource("websocket");
let frame = bidi_browser_lite::parse_resources(
    &bidi_browser_lite::bidi_lite::rdp::protocol::RdpPacket::parse(
        bidi_browser_lite::test_support::fixtures::RDP_RESOURCES_WEBSOCKET,
    )?,
    "websocket",
);
firefox.rdp().unwrap().emit_resources("websocket", frame);
let packet = futures_util::StreamExt::next(&mut ws_stream).await.unwrap()?;
let received = bidi_browser_lite::parse_resources(&packet, "websocket");
assert_eq!(received[0]["wsMessageType"], "webSocketOpened");

browser.stop().await?;

// The one-session rule, simulated:
let busy = bidi_browser_lite::test_support::MockBidi::start(MockBidiBehavior::SessionBusy).await?;
let config = BrowserConfig::builder().mode(LaunchMode::AttachOnly).port(busy.port()).build();
assert!(matches!(Browser::ensure(config).await, Err(BrowserError::SessionBusy { .. })));
# Ok(())
# }
```

`MockBidi` behaviours: `Ready`, `SessionBusy`, `CloseOnSessionNew`,
`SilentPeer`, `PlainHttp`; plus `commands()`, `commands_named()`,
`wait_for_command()`, `emit()`, `respond_with()`, `drop_connections()` (the
session survives as an orphan, like Firefox), `release_session()`,
`occupy_session()`, `stop_replying()`, `count(method)`, `connection_count()`.

`MockRdp` serves the full `bidi_lite::rdp::Debugger` lifecycle from captured
Firefox 157 frames (tabs, watcher, breakpoint list, sources with long strings,
stepping, frame evaluation, `pause()`, and `navigate()`), honours `resourceTypes`
in `watchResources`, emits arbitrary watcher resources (`emit_resources()`,
`emit_resource_updates()`, `emit_packet()`), tracks watched types
(`watched_resources()`), and records requests (`requests()`, `requests_to()`,
`wait_for_request()`, `clear_requests()`). For `Debugger::attach_to_context`
it answers the parent-process lookup (`getProcess`, `getTarget`, the
`NavigableManager` evaluation) with the `browserId` given to `map_context()`
(`null` for unmapped contexts) and can lose its parent console once
(`fail_parent_console_once()`); `set_tabs()` replaces the tab descriptors
(`fixtures::RDP_LIST_TABS_TWO_TABS` has two on the same URL); `add_source()`
declares extra sources with their breakpoint positions per line, as the
inline `<script>` blocks of one page; `emit_before_reply()` sends a packet
(e.g. `fixtures::RDP_FORWARDING_CANCELLED`) right before a reply.

The `fake_firefox_helper` binary (same feature) stands in for Firefox in
launch tests: point `firefox_path` at it and pick a behaviour with the
`FAKE_FIREFOX_MODE` environment variable (`serve`, `serve_session_busy`,
`rdp_silent`, `ignore_sigterm`, `stubborn`, `handoff`, `port_in_use`,
`exit_immediately`, `hang_no_port`); see the binary's documentation.

## Security

- **RDP gives chrome privileges to anyone who can reach its port.** Enabling
  it writes `devtools.debugger.remote-enabled = true`,
  `devtools.chrome.enabled = true` (Firefox refuses to start the server
  without it) and `devtools.debugger.prompt-connection = false`: any local
  process that connects can drive the browser with full privileges, with no
  prompt. RDP is therefore off by default, the prefs are written only while
  it is enabled, and Firefox binds the port to localhost. On a persistent
  profile, the next launch without RDP drops them from the managed block
  and from `prefs.js` (where Firefox copies `user.js` prefs); that removal
  is unit-tested but **untested against a real Firefox**.
- The BiDi port also gives full control of the browser to local processes.
  Firefox only accepts a loopback `Host` header and rejects any `Origin`
  header (verified with Firefox 157), so other machines and web pages cannot
  use it directly. To reach a Firefox in a container or on another machine,
  forward its port to `127.0.0.1` (SSH tunnel, port forward).
- Logs never contain environment variable values, pref values, the proxy URL
  or BiDi payloads; only names, ports, PIDs, paths and method names.

## Troubleshooting

- **`ExecutableNotFound`**: set `FIREFOX_PATH` or `firefox_path(..)`. The
  error lists every path searched.
- **Ubuntu (snap)**: `/usr/bin/firefox` is a shell wrapper that rewrites
  desktop settings before starting the snap; this crate uses
  `/snap/bin/firefox` directly. Snap Firefox cannot see `/tmp`, so
  persistent profiles must live under `$HOME`.
- **`SessionBusy`**: another client holds the only session of that Firefox
  (possibly an orphan left by a crashed client). End it from its owner or
  restart that Firefox.
- **`EarlyExit { reason: ProfileInUse }`**: Firefox exited with code 0 before
  it was ready, which it does after handing a launch over to a running
  instance on the same profile.
- **`EarlyExit { reason: PortInUse }`**: Firefox exited with code 69, its
  BiDi port was taken.
- **Docker / no display**: use `headless(true)`; without it Firefox exits
  with `no DISPLAY` (`EarlyExit`, code 1).

## Supported platforms and limitations

- **Tested against a real Firefox:** 157.0.1, Linux, snap (examples twice in
  a row and the `#[ignore]`d tests, visible windows), and the Debian
  `firefox-esr` 153 and Mozilla 157 builds headless in Docker during the
  design spike (BiDi only). No minimum version is claimed.
- **Windows:** the `#[ignore]`d real-Firefox tests run headless in CI
  (`windows-latest`, Firefox installed by `setup-firefox`), followed by a
  check that no `firefox` process is left. The examples and visible windows
  are untested there.
- **macOS: unit and integration tests run in CI (with the fake helper); a
  real Firefox is untested.** Its discovery paths come from Mozilla's
  install layout.
- **Proxy:** the prefs were verified against a local listener (HTTP `GET`,
  HTTPS `CONNECT`, SOCKS5 greeting); a real proxy server is untested.
- **Windows:** there is no SIGTERM, so a stop that cannot use
  `browser.close` kills the process. Firefox runs in a Job Object, so that
  kill (and a drop) takes down every process it started; processes created
  in the instant between the spawn and the job assignment would escape it.
  `--wait-for-browser` is passed so the spawned process is the browser, not
  Firefox's short-lived launcher.
- Flatpak Firefox is not supported. Chrome over BiDi is not supported in 0.1.
- Pairing a BiDi tab with an RDP tab descriptor is left to the caller (by
  URL or selected tab).
- `BrowserPool` entries are snapshots taken at `open`: after a `restart` or
  an `auto_relaunch` with a Firefox-picked BiDi port or an allocated RDP
  port, ask the `Browser` (`debug_address`, `rdp_address`) for the current
  ports.

## Development

```sh
make check              # fmt-check, clippy -D warnings, tests (unit + integration + doctests), doc, dupes
make test-real-firefox  # #[ignore]d tests against a real Firefox
make e2e                # examples against a real Firefox (visible windows; E2E_HEADLESS=1 adds a headless run)
```

`make check` never starts a real browser and never touches the network.

## License

MIT, see [LICENSE](LICENSE).

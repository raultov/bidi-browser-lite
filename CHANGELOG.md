# Changelog

All notable changes to `bidi-browser-lite` are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.3] - 2026-10-12

### Added
- `test-support`: `MockBidi` records incoming commands (`commands()`, `commands_named()`, `clear_commands()`, `wait_for_command()`) with method, parameters, and connection ID.
- `test-support`: `MockBidi::emit()` pushes events to the session connection; `MockBidi::respond_with()` overrides command replies or errors.
- `test-support`: `MockRdp` (and `MockRdpDebugger`) serves the full Firefox RDP `Debugger` protocol flow from captured Firefox 157 frames (tab descriptors, watcher bootstrap, sources, breakpoints, stepping, frame evaluation, `pause()`, and `navigate()`).
- `test-support`: `MockRdp` records incoming requests (`requests()`, `requests_to()`, `clear_requests()`, `wait_for_request()`) with target actor, request type, and parameters.

## [0.1.2] - 2026-10-12

### Fixed
- Windows: a persistent profile in use by a running Firefox is now detected
  (its `parent.lock` is held open) and refused with `ProfileError::Locked`,
  instead of a launch that Firefox hands to the running instance; the
  orphaned-profile sweep only removes ephemeral profiles whose lock is not
  held.
- Windows: Firefox and every process it starts run in a Job Object, so
  killing it (stop fallback, drop, startup timeout, cancelled launch) leaves
  no content or launcher process behind.
- Dropping a managed browser, or cancelling `Browser::ensure` while Firefox
  starts, now waits (bounded) for the killed process to exit before its
  ephemeral profile is removed, with or without a Tokio runtime.

### Added
- CI runs the real-Firefox tests on Windows too, and fails if any Firefox
  process outlives them; a new real-Firefox test checks that a profile in
  use is refused.

## [0.1.0] - 2026-10-12

### Added
- Project scaffolding: quality gates (`make check`: fmt, clippy, tests, docs,
  duplication), `AGENTS.md`, MIT licence.
- CI on Linux, macOS and Windows, a real-Firefox job on Ubuntu, and a release
  job: once every check passes, a push to `master` creates the GitHub release
  of the `Cargo.toml` version (and its tag) if it has none yet, with that
  version's CHANGELOG section as notes; a pushed version tag does the same
  for that tag.
- `error` module: `BrowserError` (`#[non_exhaustive]`) with typed details
  `ConfigError`, `ProfileError` and `EarlyExitReason` (derived from Firefox's
  exit code: 0 = profile handed to a running instance, 69 = port in use).
  Messages are actionable and never contain secrets.
- `config` module: `LaunchMode`, `ProfileMode`, `PrefValue`, `BrowserConfig`
  and `BrowserConfigBuilder` with cdp-browser-lite's defaults, plus
  `firefox_path`, opt-in `rdp`/`rdp_port`, `pref`/`prefs` for `user.js`, and
  `validate()`. `ProfileMode::UserDefault` is accepted only with
  `AttachOnly`; proxy URLs with credentials are refused; `Debug` never shows
  environment values, pref values or the proxy.
- `discovery` module: `discover_default()` finds Firefox on Linux (snap
  binary first, then `PATH`, then fixed paths), macOS (release, Developer
  Edition, Nightly; system and `~/Applications`) and Windows (`ProgramFiles`,
  `ProgramFiles(x86)`, `LOCALAPPDATA`, then `PATH` with `PATHEXT`);
  `FIREFOX_PATH` wins and a wrong value is an error. `is_snap_firefox()`.
- `ports` module: `PortAllocator` (`reserve_near`, `reserve_pair` for a BiDi
  + RDP pair under one lock, `is_reserved`), `PortSearch`, `PairRequest`,
  `PortPair` and `PortReservation`, released synchronously on drop.
- `test_support` module (feature `test-support`): `MockBidi` (behaviours
  `Ready`, `SessionBusy`, `CloseOnSessionNew`, `SilentPeer`, `PlainHttp`; one
  session that outlives its WebSocket, like Firefox), `MockRdp` (root
  greeting, `listTabs`, `getRoot`; or silent) and `MockFirefox`. Replies are
  built from frames captured from Firefox 157.
- `probe` module: `is_port_open`, `is_firefox_bidi`, `bidi_session_available`
  (WebSocket to `/session` + `session.status`, never `session.new`) and
  `is_firefox_rdp` (root actor greeting), each bounded by its timeout.
- `fake_firefox_helper` binary (feature `test-support`): a Firefox stand-in
  driven by `FAKE_FIREFOX_MODE` (`serve`, `serve_session_busy`, `rdp_silent`,
  `ignore_sigterm`, `handoff`, `port_in_use`, `exit_immediately`,
  `hang_no_port`) that serves the mocks on the ports of its command line,
  locks the profile, writes `WebDriverBiDiServer.json` for port 0 and logs
  its argv to `FAKE_FIREFOX_ARGS_LOG`.
- `profile` module: ephemeral profiles (under `$HOME/snap/firefox/common/`
  for snap Firefox, the temp dir otherwise) removed on stop or drop, with an
  orphan sweep (on Windows it removes only profiles without a
  `parent.lock`, whose owner cannot be checked); persistent and per-port
  profiles refused while a running Firefox holds them (on Unix, where the
  lock names its PID); a managed `user.js` block that preserves the
  user's own prefs; Firefox's `WebDriverBiDiServer.json` port file.
- `process` module: Firefox spawned with null stdio, readiness of the BiDi
  port (and the RDP port when enabled) checked with the side-effect free
  probes, early exits classified by exit code, `FirefoxProcess::terminate`
  (SIGTERM then kill on Unix, kill on Windows).
- `browser` module: `Browser` with `ensure`/`ensure_with_allocator`
  (`Auto`, `LaunchNew`, `AttachOnly`), one shared BiDi session behind
  `client`, `browser_client` and the tab helpers, opt-in `rdp_client`,
  `stop` (`browser.close` for managed browsers, `session.end` only for
  attached ones), `restart`, `auto_relaunch`, recovery of a managed Firefox
  stuck with an orphaned session, `SessionBusy` for attached ones,
  `keep_alive_on_drop`, and accessors (`debug_address`, `rdp_address`,
  `profile_dir`, `is_managed`, `is_alive`).
- `pool` module: `BrowserPool` (`new`, `with_allocator`, `open`, `get`,
  `entry`, `entries`, `len`, `is_empty`, `close`, `close_all`) with its own
  port allocator, `BrowserId`, and `BrowserEntry` snapshots (with
  `rdp_port`); an open with a Firefox-picked port is retried up to 5 times
  after losing a port race.
- Crate-root re-exports of the `bidi-lite` types (`BidiClient`,
  `BrowserClient`, `ContextInfo`, `ReadinessState`, `Tab`, `EventFilter`,
  `WsCommand`, `WsResponse`, `NoParams`, `BidiError`, `BidiResult`,
  `RdpClient`, `Debugger`) and of this crate's main types.
- `Browser::browser_client` (and everything built on it) checks the cached
  session with a side-effect free `session.status` before handing it out:
  a connection whose tasks died without a close frame still reports
  connected. `MockBidi::stop_replying` simulates such a peer.
- Examples `simple`, `attach`, `pool`, `debugger` (RDP: pause on an uncaught
  exception, read a variable in the paused frame) and `persistent_profile`;
  `tests/run_all_e2e.sh` (`make e2e`), `tests/firefox.sh` and the
  `#[ignore]`d real-Firefox tests (`make test-real-firefox`).
- README: quick start, RDP debugging, architecture and lifecycle diagrams,
  launch decision table, the one-session rule, ports, profiles, the
  cdp-browser-lite equivalence table, test support, security,
  troubleshooting, and supported platforms with what is untested. Its code
  blocks compile as doctests.
- Fixes from the code review, each with a test: a slow `session.status`
  no longer makes `browser_client` drop (and orphan) the browser's own
  session; `Auto`/`AttachOnly` with `rdp(true)` are refused (an attached
  Firefox's RDP port cannot be discovered); prefs that leave the managed
  block are removed from `prefs.js`; a cancelled `ensure` ends the session
  its in-flight `session.new` creates; a dropped managed browser removes its
  profile only after the process exited; `--wait-for-browser` on Windows;
  the user agent is redacted from `BrowserConfig`'s `Debug`.
- `ConfigError::PerPortProfileNeedsPort`: `PersistentPerPort` needs a fixed
  BiDi port.

[Unreleased]: https://github.com/raultov/bidi-browser-lite/compare/v0.1.3...HEAD
[0.1.3]: https://github.com/raultov/bidi-browser-lite/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/raultov/bidi-browser-lite/compare/v0.1.0...v0.1.2
[0.1.0]: https://github.com/raultov/bidi-browser-lite/releases/tag/v0.1.0

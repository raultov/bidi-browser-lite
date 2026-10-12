# Changelog

All notable changes to `bidi-browser-lite` are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed
- Windows: the crate failed to build with `-D warnings` because the profile
  lock code used only on Unix was dead code there. Lock classification is
  now one platform-neutral function; only reading the lock and checking a
  PID differ per OS.

### Changed
- Windows: the orphaned ephemeral-profile sweep now runs too. It removes only
  directories with no `parent.lock` (a lock's owner cannot be checked there,
  so a locked profile is never deleted).

## [0.1.0] - 2026-10-12

### Added
- Project scaffolding: quality gates (`make check`: fmt, clippy, tests, docs,
  duplication), `AGENTS.md`, MIT licence.
- CI on Linux, macOS and Windows, a real-Firefox job on Ubuntu, and a release
  job: pushing a version tag creates its GitHub release with that version's
  CHANGELOG section as notes, once every check passes.
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
  orphan sweep on Unix; persistent and per-port profiles refused while a
  running Firefox holds them; a managed `user.js` block that preserves the
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

[Unreleased]: https://github.com/raultov/bidi-browser-lite/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/raultov/bidi-browser-lite/releases/tag/v0.1.0

# Changelog

All notable changes to `bidi-browser-lite` are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

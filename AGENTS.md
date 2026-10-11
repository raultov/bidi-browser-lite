# AGENTS.md — conventions for bidi-browser-lite

Rules for autonomous agents working in this repository. Specs and phase plans
(`docs/specs/`, gitignored) are local-only working documents provided by the
human; they are never committed and may be absent in a fresh clone. This file
is the standing contract.

## What this crate is

Full lifecycle control of **WebDriver BiDi** browsers — Firefox in 0.1 — on
top of `bidi-lite`: discovery, launch, profiles, ports, readiness probes,
attach, a **single shared BiDi session** per browser, the optional Firefox
debugger port (RDP), stop/restart, and a pool. It is the BiDi twin of
`cdp-browser-lite` (Chrome/CDP).

It is **not** a protocol client: commands, events and debugging live in
`bidi-lite`. Do not add typed BiDi helpers, CDP, or a protocol-neutral
abstraction here. If a lifecycle need requires a bidi-lite change, write a
request in the specs; never edit `../bidi-lite`.

## Ground rules

1. **Write only inside this repository**, plus the E2E home
   (`$BIDI_BROWSER_LITE_E2E_HOME`, else `~/snap/firefox/common/bidi-browser-lite/`
   with snap Firefox, else `~/.local/share/bidi-browser-lite/e2e/`) and the
   system temp dir for ephemeral test profiles. Read sibling checkouts
   freely; never write into them.
2. **Never kill a browser this library did not start.** `stop()` on an
   attached browser only ends the session. Scripts stop only PIDs they
   started.
3. **Rust:** edition 2024; **no `unsafe`**; **no `#[allow]`** (including
   `cfg_attr(.., allow(..))`); `#[expect(..., reason = "...")]` only as a last
   resort and reported to the human. No `unwrap`/`expect`/`panic!`/
   `unreachable!` outside tests. Fix clippy/dupes findings by refactoring;
   never loosen `clippy.toml` or `dupes.toml`.
4. All code, comments, docs, README, CHANGELOG and commits in **English**.
5. **Cross-platform** (Linux, macOS, Windows): platform code in small `#[cfg]`
   functions behind injectable traits; no shelling out from `src/`; signals
   through `nix` on Unix. Shell scripts only in `tests/`, outside `make check`.
6. **Secrets never reach the logs:** log ids, ports, pids, paths, method
   names and sizes; never env var values, pref values, the proxy URL or BiDi
   payloads. Tests assert it.
7. **Errors** are typed `BrowserError`/`ConfigError`/`ProfileError` variants;
   never a `String` when a typed variant fits.
8. **Tests:** every behaviour is a BDD scenario with an ID in the specs and
   exactly one Rust test (`/// <ID>: <title>`, `// Given/When/Then`). TDD:
   the failing test first (compiles, fails for the expected reason). Unit
   tests in the module first; `tests/` only when a real child process (the
   fake helper) is needed. **Hermetic gate:** `make check` never starts a
   real Firefox, never touches the network, binds only `127.0.0.1:0`.
   Real-Firefox tests are `#[ignore = "requires a real Firefox"]`.
9. After every task: `make check && git add <explicit files> && git commit -m
   "phaseN(FN.x): <summary>"`. CHANGELOG `[Unreleased]` and README in the
   same commit as the change.
10. If a spec is wrong about Firefox, an API or a path: fix the spec first,
    then the test, then the code, and log the deviation; correct every place
    stating the old belief.
11. Never commit `target/`, `.idea/`, `.knot/`, `docs/specs/`, `PROMPT.md`.
    No `git add -A`/`git add .`. Push, tag, repo creation or `cargo publish`
    only when the human explicitly asks.
12. Git identity: `raultov <raultov@users.noreply.github.com>`; no other
    e-mail address in commits, files or metadata.
13. Use the knot MCP tools (`search_hybrid_context`, `find_callers`,
    `explore_file`) when reading indexed code.

## Architecture

- **Twin of cdp-browser-lite:** same type/method names where the concept is
  the same; differences only where BiDi/Firefox differ, each listed in the
  README equivalence table. Check the cdp-browser-lite counterpart before
  naming anything public.
- **One BiDi session per browser, owned by `Browser`.** `client()`,
  `browser_client()` and tabs are clones of it. Probes never send
  `session.new`. Stop paths close/end the session first. A managed Firefox
  with an orphan session is restarted; an attached one yields `SessionBusy`
  and is never signalled.
- **RDP is opt-in** (`rdp(true)` / `rdp_port(p)`); its prefs are written only
  then.
- **Launch knowledge lives in `launcher/firefox.rs`** (flags, prefs, port
  file, lock names); `build_args` and the `ensure` decision are pure
  functions.
- **Guards everywhere:** port reservations, ephemeral profiles and children
  are released/killed on drop, so a cancelled `ensure` leaves nothing behind.
- `Drop` lives on `BrowserState`, never needs the mutex.

## Layout

```
src/
├── lib.rs            crate docs, modules, re-exports of bidi-lite types
├── error.rs          BrowserError, ConfigError, ProfileError
├── config.rs         LaunchMode, ProfileMode, PrefValue, BrowserConfig(+Builder), validate
├── discovery.rs      discover_default, injectable env/fs, snap detection
├── launcher/         per-browser launch knowledge (firefox.rs)
├── ports.rs          PortAllocator, PortSearch, PortReservation, pairs
├── probe.rs          side-effect free TCP/BiDi/RDP probes
├── profile.rs        profile roots, user.js managed block, port file, locks, sweep
├── process.rs        spawn + readiness, FirefoxProcess::terminate
├── browser.rs        Browser: ensure, session, clients, stop/restart, Drop
├── pool.rs           BrowserPool, BrowserId, BrowserEntry
├── test_support/     feature `test-support`: MockBidi, MockRdp, MockFirefox
└── bin/fake_firefox_helper.rs   test double (feature `test-support`)
examples/             runnable programs; need a real Firefox
tests/
├── fixtures/         real BiDi/RDP frames captured from Firefox
├── *.rs              integration tests spawning the fake helper
├── e2e_real_firefox.rs  #[ignore]d real-Firefox tests (CI firefox job)
├── firefox.sh        start/stop a visible Firefox for the attach example
└── run_all_e2e.sh    `make e2e`
```

## Gates

```sh
make check              # fmt-check, clippy -D warnings, test (unit + integration + doctests), doc, dupes
make test-lib           # fast loop, not a gate
make test-real-firefox  # #[ignore]d tests against a real Firefox
make e2e                # examples against a real Firefox (visible windows)
make publish-check      # cargo publish --dry-run
```

## Running Firefox for the examples

The examples launch Firefox themselves. Only `attach` needs
`tests/firefox.sh start` first. With snap Firefox, profiles live under
`$HOME` (never `/tmp`).

## Adding a config option

1. Check the cdp-browser-lite counterpart and its name.
2. Scenario in the specs (CFG + LCH if it changes args/prefs), failing test.
3. Builder setter + `///` docs + `validate` rule if needed + `build_args`/
   prefs mapping; never log its value if it can carry a secret.
4. README (setter table + equivalence table) and CHANGELOG.

## Adding a browser launcher

Add `launcher/<browser>.rs` with candidates, args, prefs/config, readiness
and lock knowledge; select it without breaking existing public types
(`#[non_exhaustive]` enums only); same scenarios as Firefox's.

## Plans

Specs and phase plans (`docs/specs/`, gitignored) hold the phase order, the
checklist, the traceability matrix and the deviation log.

# Fixture Sources and Transcripts

Origin of embedded frames in `src/test_support/fixtures/`.

## Captured from Firefox 157.0.1 on Ubuntu 24.04

- `rdp/resources_websocket.json`: `resources-available-array` carrying `websocket` resources
  (`webSocketOpened`, `frameSent` [text, binary, close], `frameReceived` [text, binary], `webSocketClosed`).
  Captured against a local WebSocket echo server on 127.0.0.1:8808 in `bidi-lite`.
- `rdp/resources_error_message.json`: `resources-available-array` carrying `error-message` resources
  (`ReferenceError` script error with stacktrace and `cookieSameSite` rejection).
  Captured from page scripts and cookies on http://127.0.0.1:8809/ in `bidi-lite`.
- `rdp/resources_css_message.json`: `resources-available-array` carrying `css-message` resources
  (CSS Parser warnings: unknown property and invalid value).
  Captured from stylesheet parsing on http://127.0.0.1:8809/ in `bidi-lite`.

## Copied from bidi-lite 0.1.3 (`tests/fixtures/`, Firefox 157.0.1, one session)

- `rdp/get_process.json`: reply from `root` to `getProcess { id: 0 }` (parent process descriptor).
- `rdp/parent_process_target.json`: reply from the process descriptor to `getTarget` (its `consoleActor`).
- `rdp/browser_id_lookup.json` / `rdp/browser_id_lookup_null.json`: `evaluationResult` of the
  `NavigableManager` lookup of a context (`result: 12`) and of an unknown one (`{type: "null"}`).
- `rdp/list_tabs_two_tabs.json`: `listTabs` with three tabs, the last two on the same URL
  (`browserId` 2/11/12, `browsingContextID` 3/12/13).
- `rdp/breakpoint_positions_inline_none.json` / `rdp/breakpoint_positions_inline_second.json`:
  `getBreakpointPositionsCompressed` for line 9 of a page with two inline `<script>` blocks, from the
  first source (`{}`) and the second (`{"9": [2]}`).
- `rdp/forwarding_cancelled.json`: `forwardingCancelled` from `root` after a debugger detached and its
  tab closed.

## Hand-made fixtures (`*.handmade.json`)

- `rdp/source_long_string.json`: synthetic `longString` grip.
- `rdp/resources_platform_message.handmade.json`: synthetic `platform-message` resource array.

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
- `rdp/frame_target_available.json` (Firefox 159.0, headless): `target-available-form` of an iframe
  of another origin (`localhost` in a page of `127.0.0.1`), which Firefox runs in its own process:
  `isTopLevelTarget: false`, its own `threadActor` and `consoleActor`, `browsingContextID`, `url`.

## Captured from Firefox 160.0.1 on Ubuntu 24.04 (copied from `bidi-lite` 0.1.6/0.1.7)

The tab's watcher asked to `watchTargets` with `targetType: "service_worker"` or `"worker"`, with
`dom.worker.console.dispatch_events_to_main_thread` set to `false` (`bidi-lite`'s
`tests/fixtures/rdp/sw-*.json` and `worker-*.json`):
- `rdp/worker_target_available.json`: `target-available-form` of a page's dedicated worker
  (`targetType: "worker"`, `type: 0`, `threadActor`, `consoleActor`, `id`, `url`,
  `relatedDocumentInnerWindowId`).
- `rdp/worker_resources_console_message.json`: two `console-message` resources from that worker's
  target (`console.warn` with an object grip, `console.error` with its `stacktrace`).
- `rdp/service_worker_target_available.json`: `target-available-form` of a service worker
  (`targetType: "service_worker"`, `type: 2`).
- `rdp/service_worker_resources_console_message.json`: two `console-message` resources from that
  service worker's target (`console.warn`, `console.error` with its `stacktrace`).

## Hand-made fixtures (`*.handmade.json`)

- `rdp/source_long_string.json`: synthetic `longString` grip.
- `rdp/resources_platform_message.handmade.json`: synthetic `platform-message` resource array.

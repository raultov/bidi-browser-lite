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

## Hand-made fixtures (`*.handmade.json`)

- `rdp/source_long_string.json`: synthetic `longString` grip.
- `rdp/resources_platform_message.handmade.json`: synthetic `platform-message` resource array.

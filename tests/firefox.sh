#!/usr/bin/env bash
# Starts or stops a dedicated, visible Firefox for the `attach` example.
# Only the PID this script saved is ever stopped.
#
#   tests/firefox.sh start|stop|status
#
# Profile home: $BIDI_BROWSER_LITE_E2E_HOME, else ~/snap/firefox/common/bidi-browser-lite
# with snap Firefox (the snap cannot see /tmp), else ~/.local/share/bidi-browser-lite/e2e.
set -euo pipefail

BIDI_PORT="${BIDI_PORT:-9322}"
if [ -n "${BIDI_BROWSER_LITE_E2E_HOME:-}" ]; then
  HOME_DIR="$BIDI_BROWSER_LITE_E2E_HOME"
elif [ -d "$HOME/snap/firefox/common" ]; then
  HOME_DIR="$HOME/snap/firefox/common/bidi-browser-lite"
else
  HOME_DIR="$HOME/.local/share/bidi-browser-lite/e2e"
fi
PROFILE="$HOME_DIR/attach-profile"
PID_FILE="$HOME_DIR/attach-firefox.pid"
LOG="$HOME_DIR/attach-firefox.log"
FIREFOX="${FIREFOX_PATH:-$( [ -x /snap/bin/firefox ] && echo /snap/bin/firefox || command -v firefox )}"

wait_port() {
  for _ in $(seq 200); do
    if (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; then return 0; fi
    sleep 0.1
  done
  return 1
}

start() {
  mkdir -p "$PROFILE"
  printf 'user_pref("browser.shell.checkDefaultBrowser", false);\nuser_pref("app.update.disabledForTesting", true);\n' > "$PROFILE/user.js"
  "$FIREFOX" --no-remote --profile "$PROFILE" --remote-debugging-port="$BIDI_PORT" >"$LOG" 2>&1 &
  echo $! > "$PID_FILE"
  wait_port "$BIDI_PORT" || { echo "Firefox did not open port $BIDI_PORT" >&2; stop; exit 1; }
  echo "Firefox $(cat "$PID_FILE") listening on $BIDI_PORT"
}

stop() {
  [ -f "$PID_FILE" ] || return 0
  local pid; pid="$(cat "$PID_FILE")"
  if kill -0 "$pid" 2>/dev/null; then
    kill -TERM "$pid"
    for _ in $(seq 100); do kill -0 "$pid" 2>/dev/null || break; sleep 0.1; done
    kill -0 "$pid" 2>/dev/null && kill -KILL "$pid"
  fi
  rm -f "$PID_FILE"
  echo "Firefox $pid stopped"
}

status() {
  if [ -f "$PID_FILE" ] && kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then echo running; else echo stopped; fi
}

case "${1:-}" in
  start) start ;;
  stop) stop ;;
  status) status ;;
  *) echo "usage: $0 start|stop|status" >&2; exit 2 ;;
esac

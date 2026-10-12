#!/usr/bin/env bash
# Runs every example against the real Firefox and checks its output
# (`make e2e`; not part of `make check`). Visible windows by default;
# E2E_HEADLESS=1 adds a headless run of `simple`.
#
# The examples launch Firefox themselves; only `attach` needs the Firefox
# started by tests/firefox.sh, which this script stops (and only that one).
# Fails if any Firefox process started during the run is still alive at the end.
set -uo pipefail
cd "$(dirname "$0")/.."

if [ -z "${BIDI_BROWSER_LITE_E2E_HOME:-}" ]; then
  if [ -d "$HOME/snap/firefox/common" ]; then
    export BIDI_BROWSER_LITE_E2E_HOME="$HOME/snap/firefox/common/bidi-browser-lite"
  else
    export BIDI_BROWSER_LITE_E2E_HOME="$HOME/.local/share/bidi-browser-lite/e2e"
  fi
fi
mkdir -p "$BIDI_BROWSER_LITE_E2E_HOME"

PASS=0; FAIL=0; SKIP=0
before="$(pgrep -x firefox | sort)"

cleanup() { tests/firefox.sh stop >/dev/null 2>&1 || true; }
trap cleanup EXIT

cargo build --examples --quiet || { echo "build failed"; exit 1; }

run() { # name expected [env...]
  local name=$1 expected=$2; shift 2
  local out
  out="$(env "$@" RUST_LOG="${RUST_LOG:-info}" timeout 120 cargo run --quiet --example "$name" 2>&1)"
  local code=$?
  if [ $code -eq 0 ] && grep -q "$expected" <<<"$out"; then
    echo "PASS $name"; PASS=$((PASS + 1))
  else
    echo "FAIL $name (exit $code)"; echo "$out" | tail -20; FAIL=$((FAIL + 1))
  fi
}

run simple "Title: "
run pool "Pool closed: 3 browsers"
run debugger "Paused on \"exception\": answer = 42"
run persistent_profile "Profile reused: "
if tests/firefox.sh start >/dev/null; then
  attach_pid="$(cat "$BIDI_BROWSER_LITE_E2E_HOME/attach-firefox.pid")"
  run attach "Attached to Firefox, tabs: "
  if kill -0 "$attach_pid" 2>/dev/null; then echo "PASS attach left Firefox running"; PASS=$((PASS + 1));
  else echo "FAIL attach killed the Firefox it attached to"; FAIL=$((FAIL + 1)); fi
  tests/firefox.sh stop >/dev/null
else
  echo "SKIP attach (tests/firefox.sh start failed)"; SKIP=$((SKIP + 1))
fi
if [ -n "${E2E_HEADLESS:-}" ]; then
  run simple "Title: " BIDI_BROWSER_LITE_HEADLESS=1
else
  echo "SKIP simple headless (set E2E_HEADLESS=1)"; SKIP=$((SKIP + 1))
fi

sleep 1
after="$(pgrep -x firefox | sort)"
leftover="$(comm -13 <(echo "$before") <(echo "$after") | tr '\n' ' ')"
if [ -n "${leftover// /}" ]; then
  echo "FAIL leftover Firefox processes: $leftover"; FAIL=$((FAIL + 1))
else
  echo "PASS no leftover Firefox processes"; PASS=$((PASS + 1))
fi

echo "Summary: $PASS PASS, $FAIL FAIL, $SKIP SKIP"
[ $FAIL -eq 0 ]

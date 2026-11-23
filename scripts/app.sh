#!/usr/bin/env bash
# One entry point for the two long-running processes.
#
#   ./scripts/app.sh start              build if needed, start both, open the page
#   ./scripts/app.sh start --no-build   reuse whatever is already in target/
#   ./scripts/app.sh stop               stop both
#   ./scripts/app.sh status             are they up, and what does the board see
#   ./scripts/app.sh logs               follow both logs (ctrl-c to leave)
#
# Prerequisites, both outside this repo:
#   * MetaTrader 5 is running with its MCP server on 127.0.0.1:22346
#   * runs/live/mcp_token holds the token that server printed. The terminal
#     issues a new one every time it restarts, so refresh the file then.

set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT" || exit 1

PORT="${XAU_DASH_PORT:-8787}"
URL="http://127.0.0.1:$PORT"
DASH_BIN="$ROOT/target/release/xau-dashboard"
LIVE_BIN="$ROOT/target/release/xau-live"
TOKEN_FILE="$ROOT/runs/live/mcp_token"
DASH_LOG="$ROOT/runs/live/dashboard.log"
LIVE_LOG="$ROOT/runs/live/rust_h1_runner.log"
LIVE_OUT="$ROOT/runs/live/rust_h1_runner.out"

# Matched against the process command line, so it works whether the binary was
# started from here, from RustRover, or by hand as ./target/release/...
DASH_PAT="target/release/xau-dashboard"
LIVE_PAT="target/release/xau-live"

say()  { printf '%s\n' "$*"; }
fail() { printf '%s\n' "$*" >&2; exit 1; }

dash_pids() { pgrep -f "$DASH_PAT" 2>/dev/null || true; }
live_pids() { pgrep -f "$LIVE_PAT" 2>/dev/null || true; }
http_code() {
  local c
  c="$(curl -s -o /dev/null -m 3 -w '%{http_code}' "$1" 2>/dev/null)"
  [[ -z "$c" ]] && c=000
  printf '%s' "$c"
}

port_open() {
  nc -z -G 2 127.0.0.1 22346 >/dev/null 2>&1
}

stop_all() {
  pkill -f "$DASH_PAT" 2>/dev/null
  pkill -f "$LIVE_PAT" 2>/dev/null
  local i
  for i in $(seq 1 24); do
    [[ -z "$(dash_pids)$(live_pids)" ]] && return 0
    sleep 0.25
  done
  pkill -9 -f "$DASH_PAT" 2>/dev/null
  pkill -9 -f "$LIVE_PAT" 2>/dev/null
  sleep 0.5
}

cmd_status() {
  local d l c
  d="$(dash_pids)"; l="$(live_pids)"; c="$(http_code "$URL/healthz")"

  if [[ -n "$d" ]]; then say "dashboard  running (pid $(echo "$d" | tr '\n' ' '))"
  else say "dashboard  stopped"; fi
  if [[ -n "$l" ]]; then say "runner     running (pid $(echo "$l" | tr '\n' ' '))"
  else say "runner     stopped"; fi

  if [[ "$c" == "000" ]]; then
    say "page       not answering on $URL"
    return 0
  fi
  say "page       $URL (health $c)"
  say ""

  local tmp="${TMPDIR:-/tmp}/xau_state.$$.json"
  curl -s -m 5 "$URL/api/state" -o "$tmp"
  python3 - "$tmp" <<'PY'
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception:
    print("  could not read /api/state")
    raise SystemExit

acct = d.get("account") or {}
print("  account    {} {} · equity {}".format(
    acct.get("kind", "?"), acct.get("login", "?"), acct.get("equity")))
print("  terminal   {} · market {}".format(
    "connected" if d.get("connected") else "DISCONNECTED",
    "open" if d.get("market_open") else "closed"))
if d.get("error"):
    print("  error      {}".format(d["error"]))

for t in d.get("tracks") or []:
    pos = t.get("open_position")
    tag = "none" if not pos else (pos if isinstance(pos, str) else "open")
    print("  track {:<5}{} · position {} · closed {}".format(
        t.get("period", "?"),
        "running" if t.get("alive") else "stale",
        tag, t.get("closed_trades")))

for a in d.get("analyses") or []:
    side = (a.get("side") or "no signal").upper()
    win = "open" if a.get("window_open") else "shut"
    print("  signal     {}: {} · window {} · next fill {:02d}:00 UTC".format(
        a.get("period", "?"), side, win, int(a.get("fill_hour_utc") or 0)))
PY
  rm -f "$tmp"
}

cmd_start() {
  local no_build=0
  [[ "${1:-}" == "--no-build" ]] && no_build=1

  [[ -s "$TOKEN_FILE" ]] || fail "no MCP token. Write the terminal's token to runs/live/mcp_token
  (or export MT5_MCP_TOKEN=...) and run this again."
  port_open || say "note: nothing is listening on 127.0.0.1:22346 — the runner will
      keep retrying until MetaTrader 5 is up with its MCP server."

  if [[ $no_build -eq 1 ]]; then
    [[ -x "$DASH_BIN" && -x "$LIVE_BIN" ]] || fail "no binaries in target/release yet — run without --no-build"
  else
    say "building (a minute or two the first time)…"
    cargo build --release -p xau-dashboard -p xau-live || fail "build failed — fix the error above"
  fi

  stop_all
  mkdir -p "$ROOT/runs/live"

  say "starting the dashboard…"
  nohup "$DASH_BIN" >"$DASH_LOG" 2>&1 &
  disown

  local c=000 i
  for i in $(seq 1 40); do
    c="$(http_code "$URL/healthz")"
    [[ "$c" != "000" ]] && break
    sleep 0.5
  done
  [[ "$c" == "000" ]] && fail "the dashboard did not come up. Last lines of $DASH_LOG:
$(tail -n 15 "$DASH_LOG" 2>/dev/null)"

  say "starting the intraday runner (H1, 0.01 lot, entries 07:00–18:00 UTC)…"
  # The runner writes its own JSON log; stdout is only for panics and warnings.
  # Pointing it at the same file would double every line.
  nohup "$LIVE_BIN" --period H1 --intraday --lots 0.01 --poll 60 >"$LIVE_OUT" 2>&1 &
  disown
  sleep 3

  say ""
  cmd_status
  say ""
  say "logs: ./scripts/app.sh logs    stop: ./scripts/app.sh stop"
  [[ "$(uname -s)" == "Darwin" ]] && open "$URL" >/dev/null 2>&1
}

cmd_logs() {
  tail -n 20 -f "$DASH_LOG" "$LIVE_LOG" "$LIVE_OUT"
}

case "${1:-help}" in
  start)  shift; cmd_start "${1:-}" ;;
  stop)   stop_all; say "stopped." ;;
  status) cmd_status ;;
  logs)   cmd_logs ;;
  *)
    sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
    ;;
esac

#!/bin/zsh
# Minimal MCP client for the MetaTrader 5 terminal MCP server (ops tooling).
# Usage: scripts/mcp.sh call <tool> [json-args]
set -euo pipefail

TOKEN='oo6totlCD01Iu//GWbs0E/0qqP28lE+8TGkXB/WXYL'
URL='http://127.0.0.1:22346/mcp'
SIDFILE="/tmp/mt5_mcp_sid"

new_session() {
  local sid
  sid=$(/usr/bin/curl -s -m 10 -D - -o /dev/null -X POST "$URL" \
    -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream" \
    -H "Authorization: Bearer $TOKEN" \
    -d '{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"xau-agent","version":"0.1.0"}}}' |
    grep -i '^Mcp-Session-Id:' | tr -d '\r' | awk '{print $2}')
  /usr/bin/curl -s -m 10 -o /dev/null -X POST "$URL" \
    -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream" \
    -H "Authorization: Bearer $TOKEN" -H "Mcp-Session-Id: $sid" \
    -d '{"jsonrpc":"2.0","method":"notifications/initialized"}'
  echo "$sid" > "$SIDFILE"
  echo "$sid"
}

get_sid() {
  if [[ -s "$SIDFILE" ]]; then
    cat "$SIDFILE"
  else
    new_session
  fi
}

# Retry once with a fresh session on session/protocol errors.
call_tool() {
  local tool="$1" args="$2" sid out
  if [[ -z "$args" ]]; then
    args='{}'
  fi
  sid=$(get_sid)
  python3 -c "
import json, sys
body = {'jsonrpc': '2.0', 'id': 99, 'method': 'tools/call',
        'params': {'name': sys.argv[1], 'arguments': json.loads(sys.argv[2])}}
open('/tmp/mcp_body.json', 'w').write(json.dumps(body))
" "$tool" "$args"
  out=$(/usr/bin/curl -s -m 120 -X POST "$URL" \
    -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream" \
    -H "Authorization: Bearer $TOKEN" -H "Mcp-Session-Id: $sid" \
    --data-binary @/tmp/mcp_body.json)
  if echo "$out" | grep -q '"is not initialized"\|"Session not found"\|invalid session'; then
    rm -f "$SIDFILE"
    sid=$(new_session)
    out=$(/usr/bin/curl -s -m 120 -X POST "$URL" \
      -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream" \
      -H "Authorization: Bearer $TOKEN" -H "Mcp-Session-Id: $sid" \
      --data-binary @/tmp/mcp_body.json)
  fi
  printf '%s' "$out" > /tmp/mcp_resp.json
  python3 -c "
import json
raw = open('/tmp/mcp_resp.json').read()
try:
    obj = json.loads(raw)
except Exception as e:
    print('PARSE_FAIL:', e); print(raw[:2000]); raise SystemExit
if 'error' in obj:
    print('RPC_ERROR:', json.dumps(obj['error'])[:500]); raise SystemExit
res = obj.get('result', {})
for c in res.get('content', []):
    print(c.get('text', '') if c.get('type') == 'text' else json.dumps(c)[:2000])
if res.get('isError'):
    print('TOOL_ERROR: true')
"
}

case "${1:-help}" in
  init) rm -f "$SIDFILE"; new_session >/dev/null; echo "session ready" ;;
  call)
    tool="$2"
    args="$3"
    if [[ -z "$args" ]]; then
      args='{}'
    fi
    call_tool "$tool" "$args"
    ;;
  *) echo "usage: mcp.sh init | call <tool> [args-json]" ;;
esac

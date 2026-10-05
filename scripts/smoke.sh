#!/usr/bin/env bash
# Drive one tool call against the real server over stdio: a full MCP handshake, one tools/call, print
# the text result. This is the "does my install actually work" check, and the only way to see what a
# tool's output really looks like without wiring it into a client.
#
#   scripts/smoke.sh jira_search '{"jql":"project = PROJ ORDER BY updated DESC","limit":5}'
#   scripts/smoke.sh jira_get_issue '{"issue_key":"PROJ-142"}'
set -euo pipefail

TOOL="${1:?usage: smoke.sh <tool> [json-args]}"
ARGS="${2:-{\}}"
BIN="${UJIRA_BIN:-$(dirname "$0")/../target/debug/ujira}"

[ -x "$BIN" ] || { echo "no binary at $BIN (cargo build first, or set UJIRA_BIN)" >&2; exit 1; }

{
  echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}'
  echo '{"jsonrpc":"2.0","method":"notifications/initialized"}'
  echo "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"$TOOL\",\"arguments\":$ARGS}}"
  # The server has no idea we're done; give the call time to land, then close the pipe.
  sleep "${UJIRA_SMOKE_WAIT:-15}"
} | "$BIN" mcp serve 2>/dev/null | python3 -c '
import sys, json
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    if msg.get("id") != 2:
        continue
    if "error" in msg:
        sys.exit("error: " + json.dumps(msg["error"]))
    for block in msg["result"]["content"]:
        print(block.get("text", ""))
'

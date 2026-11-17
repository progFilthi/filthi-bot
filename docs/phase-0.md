# Phase 0 — Feasibility spike: MT5 EA ↔ Rust TCP round trip under Wine

**Status: PASSED (2026-11-17).** A minimal MQL5 EA running inside MetaTrader 5
(build 6320, under Wine 11.7 on macOS) opened a TCP socket to a Rust server on
`127.0.0.1:5555`, streamed ticks, received a test market order with a mandatory
SL, reported the fill, applied an SL modify (verified), executed the close, and
acked it. The server exited 0 with `ROUND_TRIP_OK`.

## What was built

| Piece | Location | Notes |
|---|---|---|
| Core types crate | `crates/xau-core` | `Tick`, `Bar`, `Side`, `OrderRequest`, `Fill` (+ unit tests) |
| Bridge server (phase 0 spike) | `crates/bridge` | NDJSON protocol (README §4), session state machine, transcript JSONL writer, TOML config, clap CLI |
| Bridge EA (the only non-Rust piece) | `mql5/Bridge.mq5` | TCP client; hello + snapshots, tick stream, order/modify/close/close_all, validation (lots, mandatory SL, stops level), idempotency ring (128), flat-JSON parser, reconnect every 1.5 s, heartbeats 1 s |
| Compile/install helper | `scripts/install_mql5.sh` | Headless MetaEditor compile into a no-space temp dir (`C:\temp\…`), then copy `.ex5` into `MQL5\Experts` |
| MCP automation helper | `scripts/mcp.sh` | Minimal MCP client for the MT5 terminal MCP server (port 22346): journal reads, chart/EA control, terminal info |

## Verification evidence

Successful run `2026-11-17T08:17:40Z` (server stdout + `runs/phase0/transcript.jsonl`,
18 wire records, both directions):

| Stage | t (ms from connect) | Detail |
|---|---|---|
| hello | 4 | account 115694190, MetaQuotes-Demo, XAUUSD, digits=2, point=0.01, contract=100, min_lot=0.01, lot_step=0.01, stops_level=0, demo=true, server_utc_offset≈+7200 s |
| account snapshot | — | balance 5000.00, equity 5000.00 |
| first tick | 112 | bid 3862.79 / ask 3862.92 |
| test order out | 112 | buy 0.01 lots, SL 3861.92 ($1.00 below ask), TP 3864.92 (2R), slippage cap 30 pts, magic 424242, UUID order_id |
| entry fill | 877 | ticket 11431865285, price 3862.99, realized slippage 7 pts |
| modify out | 877 | SL tightened to 3862.03 (90% of original distance) |
| modify verified | ~1190 | position snapshot shows SL=3862.03 |
| close out | 1190 | ticket 11431865285 |
| close ack | 1334 | fill `order_id="close-11431865285"`, price 3862.88, slippage 0 pts |
| result | 1334 | `ROUND_TRIP_OK`, server exit 0 |

Total round trip connect→close-ack: **1.33 s**. Account left flat
(`get_trading_open_positions` → no positions/orders). Honest P&L for the test
round trip: **−$0.11** (entry slippage 7 pts + exit 11 cents below entry on
1 oz). No profitability claims — this proves plumbing only.

Quality gates at commit time: `cargo fmt` clean, `cargo clippy --workspace
--all-targets -- -D warnings` clean, `cargo test --workspace` 12/12 passed
(8 bridge incl. deadlock regression test, 4 xau-core).

## Bugs found by the spike (and fixed)

1. **Transcript writer deadlock** (`crates/bridge/src/transcript.rs`):
   `record()` locked the writer mutex and then called `open()`, which locked
   the same non-recursive mutex again — the very first inbound line (the EA's
   hello) deadlocked the entire session task. Symptom: connection ESTABLISHED,
   EA sending fine, server received "nothing", no timeouts fired (the whole
   select loop was frozen inside the mutex). Fixed by having `open()` take the
   already-locked slot; regression test `record_does_not_deadlock_and_writes_all_lines`
   added. Diagnosis: macOS `sample` stack dump showing `__psynch_mutexwait`.
2. **Hello working-copy bug** (`crates/bridge/src/session.rs`): the `Hello`
   handler set only `out.hello` (report artifact), never the `self.hello`
   working copy used by `send_test_order()`/`round()` — test order aborted
   with "cannot send order before hello". Fixed by setting both.
3. **Wrong MQL5 API** (`mql5/Bridge.mq5`): `SocketWrite` does not exist; the
   send path is `SocketSend(socket, buffer, len)`. (Caught at compile time.)
4. **Stale-binary gotcha**: `cargo clippy`/`cargo test` do not refresh the
   runnable `target/debug/xau-bridge-server` binary; a run with the
   pre-fix binary reproduced the "dead" server. Always `cargo build` before
   running the server.

## Environment / operational notes (matter for Phase 6)

- MT5 build 6320 under Wine 11.7 (`~/Library/Application Support/net.metaquotes.wine.metatrader5`).
  Local timezone (CAT, UTC+2) == server timezone. Demo account 115694190,
  MetaQuotes-Demo, hedging mode.
- **Sockets require a manual GUI step**: `127.0.0.1` must be added to
  Options → Expert Advisors → "Allow WebRequest for listed URL" (err 4014
  otherwise). Not scriptable.
- **Trading requires two live switches**: (1) the global "Algo Trading"
  toolbar toggle (`experts_trade_allowed`), and (2) the per-EA "Allow Algo
  Trading" checkbox in the EA's properties (Common tab). The per-EA checkbox
  is **grayed out while the global switch is off** — enable global first.
  Visual cue: the EA's hat icon at the chart corner is white when the EA may
  not trade and **blue** when it may. Order attempts blocked by either switch
  return retcode 10027 / err 4752 ("autotrading disabled by client terminal").
- Attaching the EA via MCP `chart_add_expert` resets the per-EA permission to
  OFF; there is no MCP parameter for it (checked the full 107-tool catalog).
  After a manual re-attach, redo the properties checkbox.
- The EA reconnects by itself (1.5 s cadence) and self-heals when the server
  process dies (journal: `SocketRead failed r=-1 err=5273` → disconnect →
  reconnect). Heartbeats run every 1 s in both directions.
- MT5 MCP server (127.0.0.1:22346) sessions expire; re-run `scripts/mcp.sh init`
  when tools return `PARSE_FAIL`. Journal datetime windows must include the
  current time.
- Headless MetaEditor compile requires a no-space path (`C:\temp\Bridge.mq5`);
  spaced paths fail silently under Wine. The compile script handles this.
- EA idempotency ring stores the *reply* per order_id, including rejects — a
  rejected order_id replayed after reconnect resends the stored reject. The
  server generates a fresh UUID per attempt, so this is benign in the spike.
- Server exit semantics: success → `ROUND_TRIP_OK`, exit 0; fatal test failure
  (e.g. order reject) → exit 1, stop; disconnects → warn and await reconnect.

## Known issues / limitations

- Wine-side socket writes never surfaced an error even when the server was
  deadlocked (all `SocketSend` calls returned success into a buffer nobody
  drained); server-side staleness detection (README §8: halt on >5 s inbound
  silence) is the safety net and is already implemented in the session loop.
- The spike server is single-connection, sequential, and per-connection
  stateful — fine for phase 0; the Phase 6 live bridge will extend this.
- `stops_level=0` on this demo broker means the SL-distance validation path
  (stops-level rejection) was exercised only by unit tests, not by the broker.
- The MCP automation token is not persisted in any readable repo config
  (deliberate); it lives in `scripts/mcp.sh` for now — rotate before any
  shared-machine use.

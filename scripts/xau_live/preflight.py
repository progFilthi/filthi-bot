#!/usr/bin/env python3
"""Read-only preflight for the demo runner.

Checks the things that must be true before a single order goes out:
  - the terminal is connected, is a DEMO account, and trading is allowed
  - no position is already open on XAUUSD
  - the strategy maths produces a sane state on fresh H1 bars
  - the broker would accept the exact order shape the strategy needs
    (market entry, stop loss, NO take profit)

It never places an order. `trade_check_market_order` is a validation call.

Exit code 0 = all clear, 1 = something must be fixed first.
"""

from __future__ import annotations

import sys
from datetime import datetime, timedelta

from client import Mcp, McpError
import strategy as S

SYMBOL = "XAUUSD"
LOT = 0.01          # demo size; see docs/demo-readiness.md §9
BARS = 420          # enough for EMA300 + Donchian55 + margin


def ok(label: str, detail: str = "") -> None:
    print(f"  PASS  {label}" + (f"  ({detail})" if detail else ""))


def bad(label: str, detail: str = "") -> None:
    print(f"  FAIL  {label}" + (f"  ({detail})" if detail else ""))


def main() -> int:
    problems = 0
    print("=" * 72)
    print("D3 DEMO PREFLIGHT")
    print("=" * 72)

    try:
        mcp = Mcp()
    except Exception as exc:  # noqa: BLE001
        bad("MCP reachable", str(exc)[:200])
        return 1
    ok("MCP reachable")

    # ---- account ------------------------------------------------------
    acct = mcp.account()
    a = acct["account"]
    term = acct["terminal"]
    print(f"\n  account {a['login']} ({a['name']}) @ {a['server']}")
    print(f"  type={a['type']} currency={a['currency']} balance={a['balance']:.2f} "
          f"equity={a['equity']:.2f} margin_free={a['margin_free']:.2f}")

    if a["type"] != "demo":
        bad("account is DEMO", f"type={a['type']}")
        problems += 1
    else:
        ok("account is DEMO")

    if not term.get("mcp_trade_allowed", False):
        bad("terminal allows MCP trading", "mcp_trade_allowed=false")
        problems += 1
    else:
        ok("terminal allows MCP trading")

    if term.get("connection", {}).get("state") != "synchronized":
        bad("terminal synchronized", str(term.get("connection", {}).get("state")))
        problems += 1
    else:
        ok("terminal synchronized")

    # ---- positions ----------------------------------------------------
    pos = mcp.positions()
    open_positions = pos.get("positions") or []
    xau = [p for p in open_positions if p.get("symbol") == SYMBOL]
    if xau:
        for p in xau:
            print(f"  open: ticket={p.get('ticket')} {p.get('type')} "
                  f"{p.get('volume')} @ {p.get('price_open')} sl={p.get('sl')}")
        bad("no existing XAUUSD position", f"{len(xau)} open")
        problems += 1
    else:
        ok("no existing XAUUSD position")

    # ---- data ---------------------------------------------------------
    h1 = mcp.bars("H1", BARS)
    if not h1:
        bad("H1 history available")
        return 1

    # Drop the still-forming bar: signals use CLOSED bars only.
    now_utc = mcp.now_utc()
    closes_at = lambda b: b["ts"] + timedelta(hours=1)  # noqa: E731
    closed = h1 if closes_at(h1[-1]) <= now_utc else h1[:-1]
    if h1 and h1[-1] is not closed[-1]:
        print(f"  (dropped forming bar {h1[-1]['ts']:%Y-%m-%d %H:%M} UTC)")

    ok("H1 history available",
       f"{len(closed)} closed bars through {closed[-1]['ts']:%Y-%m-%d %H:%M} UTC")

    sig = S.evaluate(closed)
    print(f"\n  last closed bar: {closed[-1]['ts']:%Y-%m-%d %H:%M} UTC  "
          f"close {closed[-1]['close']:.2f}")
    print(f"  EMA{S.TREND_MA}={sig.ema:.2f}  ATR{S.ATR_PERIOD}={sig.atr:.2f}  "
          f"donchian[{S.LOOKBACK}] hi={sig.donchian_hi:.2f} lo={sig.donchian_lo:.2f}")
    print(f"  signal: {sig.side or 'none'}  ({sig.reason})")

    if not (sig.atr == sig.atr) or sig.atr <= 0:
        bad("ATR is valid", str(sig.atr))
        problems += 1
    else:
        ok("ATR is valid", f"{sig.atr:.4f}")

    # ---- order shape --------------------------------------------------
    side = sig.side or "buy"
    price = mcp.price()
    stop = S.stop_price(side, price, sig.atr)
    risk_usd = abs(price - stop) * 100.0 * LOT
    print(f"\n  current price {price:.2f}")
    print(f"  hypothetical {side.upper()} {LOT} lot, SL {stop:.2f} "
          f"({abs(price - stop):.2f} price = {abs(price - stop) / 0.01:.0f} pts)")
    print(f"  risk at {LOT} lot: ${risk_usd:.2f}  "
          f"({risk_usd / a['equity'] * 100:.2f}% of equity)")

    try:
        chk = mcp.call("trade_check_market_order", {
            "symbol": SYMBOL, "type": side, "volume": LOT, "sl": round(stop, 2),
        })
        print(f"  broker check: {chk}")
        ok("broker would accept the order shape")
    except McpError as exc:
        bad("broker would accept the order shape", str(exc)[:300])
        problems += 1

    # ---- summary ------------------------------------------------------
    print("\n" + "=" * 72)
    if problems:
        print(f"PREFLIGHT FAILED: {problems} problem(s) — do not start the runner")
        return 1
    print("PREFLIGHT OK — the runner can start")
    print(f"  next action depends on the signal: currently {sig.side or 'none'}")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except McpError as exc:
        print(f"\nMCP ERROR: {exc}")
        sys.exit(1)

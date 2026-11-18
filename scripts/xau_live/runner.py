#!/usr/bin/env python3
"""D3 live runner for the MT5 demo account.

Mirrors `crates/simulator/src/engine.rs` bar-for-bar, one closed H1 bar at a
time:

  * act only on a newly CLOSED H1 bar (the forming bar is always discarded)
  * manage first: time exit at 240 bars, then trail to 2.5 x ATR from the best
    price; the broker holds the stop, so intrabar fills match the engine
  * no take profit, no opposite-signal exit, one position at a time
  * enter at market as soon as a closed bar produces a Donchian 55 breakout in
    the direction of the EMA300 gate

State lives in `runs/live/d3_state.json` so a restart does not lose the trail
or the bar counter. The broker is the source of truth for whether a position
still exists: if it disappears (stop hit, manual close) the runner reconciles
and goes flat instead of re-entering blindly.

Usage:
  python3 runner.py                 # live loop on the demo account
  python3 runner.py --once          # single pass (cron / testing)
  python3 runner.py --dry-run       # decide and log, never send an order
"""

from __future__ import annotations

import argparse
import json
import sys
import time
import traceback
from datetime import datetime, timezone
from pathlib import Path

from client import Mcp, McpError
import strategy as S

SYMBOL = "XAUUSD"
LOT = 0.01                 # demo size; see docs/demo-readiness.md §9
BARS = 420                 # EMA300 + Donchian55 + ATR14 with margin
POLL_SECONDS = 60
COMMENT = "D3"

ROOT = Path(__file__).resolve().parents[2]
LIVE_DIR = ROOT / "runs" / "live"
STATE_PATH = LIVE_DIR / "d3_state.json"
LOG_PATH = LIVE_DIR / "d3_runner.log"

# Halt if equity falls this far below the balance the runner started with.
MAX_DRAWDOWN_PCT = 25.0


# ---------------------------------------------------------------------------
# logging
# ---------------------------------------------------------------------------
def log(event: str, **fields) -> None:
    LIVE_DIR.mkdir(parents=True, exist_ok=True)
    rec = {
        "ts": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "event": event,
        **fields,
    }
    with LOG_PATH.open("a") as fh:
        fh.write(json.dumps(rec, default=str) + "\n")
    bits = " ".join(f"{k}={v}" for k, v in fields.items())
    print(f"{rec['ts']} {event:<14} {bits}", flush=True)


# ---------------------------------------------------------------------------
# state
# ---------------------------------------------------------------------------
def load_state() -> dict:
    if STATE_PATH.exists():
        try:
            return json.loads(STATE_PATH.read_text())
        except json.JSONDecodeError:
            log("state_corrupt", path=str(STATE_PATH))
    return {
        "last_bar_ts": 0,
        "start_balance": None,
        "position": None,
        "closed_trades": 0,
        "realised_usd": 0.0,
    }


def save_state(state: dict) -> None:
    LIVE_DIR.mkdir(parents=True, exist_ok=True)
    tmp = STATE_PATH.with_suffix(".tmp")
    tmp.write_text(json.dumps(state, indent=1))
    tmp.replace(STATE_PATH)


# ---------------------------------------------------------------------------
# broker helpers
# ---------------------------------------------------------------------------
def xau_positions(m: Mcp) -> list[dict]:
    return [p for p in (m.positions().get("positions") or []) if p.get("symbol") == SYMBOL]


def close_position(m: Mcp, pos: dict) -> dict:
    return m.call(
        "trade_close_single_position",
        {"symbol": SYMBOL, "position_ticket": str(pos["position_id"])},
    )


def send_entry(m: Mcp, side: str, stop: float) -> dict:
    return m.call(
        "trade_send_market_order",
        {
            "symbol": SYMBOL,
            "type": side,
            "volume": LOT,
            "sl": round(stop, 2),
            "comment": COMMENT,
        },
    )


# ---------------------------------------------------------------------------
# one pass
# ---------------------------------------------------------------------------
def reconcile(m: Mcp, state: dict, dry_run: bool) -> None:
    """Make state agree with the broker before any decision is taken."""
    live = xau_positions(m)
    tracked = state.get("position")

    if tracked and not any(p["position_id"] == tracked["position_id"] for p in live):
        log(
            "closed_by_broker",
            side=tracked["side"],
            entry=tracked["entry"],
            bars_held=tracked["bars_held"],
            balance=round(m.account()["account"]["balance"], 2),
        )
        state["position"] = None
        state["closed_trades"] += 1
        save_state(state)
        return

    if not tracked and live:
        p = live[0]
        # Adopt an orphaned position rather than opening a second one.
        state["position"] = {
            "position_id": p["position_id"],
            "side": p["action"],
            "entry": p["price_open"],
            "sl": p.get("stop_loss") or 0.0,
            "best": p["price_open"],
            "bars_held": 0,
            "atr_entry": None,
            "adopted": True,
        }
        log("adopted_position", side=p["action"], entry=p["price_open"], id=p["position_id"])
        save_state(state)

    if dry_run and state.get("position"):
        pass  # dry run may still track a previously opened position

    if len(live) > 1:
        log("warning_multiple_positions", count=len(live))


def manage(m: Mcp, state: dict, bar: dict, atr_now: float, dry_run: bool) -> None:
    """Trail / time-exit the tracked position against a newly closed bar."""
    p = state["position"]
    p["bars_held"] += 1

    if p["bars_held"] >= S.MAX_HOLD_BARS:
        log("time_exit", side=p["side"], bars=p["bars_held"], close=bar["close"])
        if not dry_run:
            live = xau_positions(m)
            if live:
                res = close_position(m, live[0])
                log("time_exit_filled", retcode=res.get("retcode"), price=res.get("price"))
        state["position"] = None
        state["closed_trades"] += 1
        save_state(state)
        return

    if not atr_now or atr_now != atr_now:
        save_state(state)
        return

    if p["side"] == "buy":
        p["best"] = max(p["best"], bar["high"])
        cand = S.trail_price("buy", p["best"], atr_now)
        improve = cand > p["sl"]
    else:
        p["best"] = min(p["best"], bar["low"])
        cand = S.trail_price("sell", p["best"], atr_now)
        improve = cand < p["sl"] or p["sl"] == 0.0

    if improve and round(cand, 2) != round(p["sl"], 2):
        log(
            "trail",
            side=p["side"],
            best=round(p["best"], 2),
            old_sl=round(p["sl"], 2),
            new_sl=round(cand, 2),
            bars=p["bars_held"],
        )
        if not dry_run:
            live = xau_positions(m)
            if live:
                try:
                    res = m.call(
                        "trade_modify_sl_tp",
                        {
                            "symbol": SYMBOL,
                            "position_ticket": str(live[0]["position_id"]),
                            "sl": round(cand, 2),
                        },
                    )
                    log("trail_filled", retcode=res.get("retcode"))
                except McpError as exc:
                    log("trail_rejected", error=str(exc)[:200])
                    save_state(state)
                    return
        p["sl"] = round(cand, 2)
        p["trailed"] = True

    save_state(state)


def maybe_enter(m: Mcp, state: dict, closed: list[dict], dry_run: bool) -> None:
    if state["position"] is not None:
        return
    sig = S.evaluate(closed)
    if sig.side is None:
        log(
            "no_signal",
            close=round(sig.close, 2),
            hi=round(sig.donchian_hi, 2),
            lo=round(sig.donchian_lo, 2),
            ema=round(sig.ema, 2),
            atr=round(sig.atr, 2),
            why=sig.reason,
        )
        return

    price = m.price()
    stop = S.stop_price(sig.side, price, sig.atr)
    risk = abs(price - stop) * 100.0 * LOT
    log(
        "entry_signal",
        side=sig.side,
        price=round(price, 2),
        sl=round(stop, 2),
        atr=round(sig.atr, 2),
        risk_usd=round(risk, 2),
        why=sig.reason,
    )
    if dry_run:
        return

    res = send_entry(m, sig.side, stop)
    log("entry_filled", retcode=res.get("retcode"), detail=res.get("retcode_details"),
        price=res.get("price"), deal=res.get("deal"))

    live = xau_positions(m)
    if not live:
        log("entry_missing", note="order accepted but no position visible")
        return
    p = live[0]
    state["position"] = {
        "position_id": p["position_id"],
        "side": p["action"],
        "entry": p["price_open"],
        "sl": p.get("stop_loss") or round(stop, 2),
        "best": p["price_open"],
        "bars_held": 0,
        "atr_entry": round(sig.atr, 4),
        "trailed": False,
    }
    save_state(state)


def pass_once(m: Mcp, state: dict, dry_run: bool) -> bool:
    """One poll. Returns True if a new bar was processed."""
    h1 = m.bars("H1", BARS)
    now = m.now_utc()
    closed = h1 if h1[-1]["ts"].timestamp() + 3600 <= now.timestamp() else h1[:-1]
    if not closed:
        return False

    acct = m.account()["account"]
    if state["start_balance"] is None:
        state["start_balance"] = acct["balance"]
        save_state(state)
    floor = state["start_balance"] * (1.0 - MAX_DRAWDOWN_PCT / 100.0)
    if acct["equity"] <= floor:
        log("halt_drawdown", equity=acct["equity"], floor=round(floor, 2))
        raise SystemExit(2)

    reconcile(m, state, dry_run)

    last_ts = closed[-1]["ts"].timestamp()
    if last_ts <= state["last_bar_ts"]:
        return False

    bootstrap = state["last_bar_ts"] == 0
    missed = 0 if bootstrap else sum(
        1 for b in closed if b["ts"].timestamp() > state["last_bar_ts"]
    ) - 1
    if bootstrap:
        log("bootstrap", bars=len(closed), last_bar=str(closed[-1]["ts"]))
    elif missed > 0:
        log("catch_up", missed_bars=missed,
            note="skipped management on missed bars; no stale entries")

    bar = closed[-1]
    atr_now = S.atr(closed, S.ATR_PERIOD)[-1]

    if state["position"] is not None:
        manage(m, state, bar, atr_now, dry_run)
    else:
        maybe_enter(m, state, closed, dry_run)

    state["last_bar_ts"] = last_ts
    save_state(state)
    return True


# ---------------------------------------------------------------------------
def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--once", action="store_true", help="single pass, then exit")
    ap.add_argument("--dry-run", action="store_true", help="never send or modify orders")
    ap.add_argument("--allow-live", action="store_true", help="permit a real-money account")
    args = ap.parse_args()

    state = load_state()
    log("runner_start", lot=LOT, dry_run=args.dry_run, symbol=SYMBOL,
        max_hold=S.MAX_HOLD_BARS, trail_atr=S.TRAIL_ATR)

    m: Mcp | None = None
    while True:
        try:
            if m is None:
                m = Mcp()
            acct = m.account()["account"]
            if acct["type"] != "demo" and not args.allow_live:
                log("refused", reason=f"account type is {acct['type']}, not demo")
                return 1
            processed = pass_once(m, state, args.dry_run)
            if args.once:
                log("runner_once_done", processed=processed)
                return 0
            if not processed:
                time.sleep(POLL_SECONDS)
        except SystemExit:
            raise
        except (McpError, OSError) as exc:
            log("poll_error", error=str(exc)[:200])
            m = None
            if args.once:
                return 1
            time.sleep(POLL_SECONDS)
        except Exception:  # noqa: BLE001
            log("unexpected_error", trace=traceback.format_exc()[-800:])
            m = None
            if args.once:
                return 1
            time.sleep(POLL_SECONDS)


if __name__ == "__main__":
    sys.exit(main())

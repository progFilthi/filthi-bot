#!/usr/bin/env python3
"""Bulk-fetch XAUUSD history from the MT5 terminal MCP server.

Why this exists
---------------
The MCP `get_chart_history` tool caps at 100k bars per call, and the terminal
backfills history *progressively*: each request makes a little more of the past
available. This script:

  1. warms the terminal up by repeating the request until the earliest
     available bar stops moving backwards,
  2. pages forward with a cursor until the requested end date is reached,
  3. converts SERVER-LOCAL timestamps (MetaQuotes-Demo runs EET/EEST:
     UTC+2 winter, UTC+3 summer) to true UTC using EU DST rules,
  4. streams rows straight to CSV in the format `xau data import` expects.

Because the output is already UTC, import it with a spec that has
`server_utc_offset_s = 0` (configs/symbols/xauusd_utc.toml).

Usage
-----
  python3 scripts/fetch_mcp_history.py \
      --period H1 --from 2010-01-01 --to 2026-11-19 \
      --out data/raw/xauusd_h1_mcp.csv
"""

from __future__ import annotations

import argparse
import http.client
import json
import sys
from datetime import datetime, timedelta, timezone

TOKEN = "oo6totlCD01Iu//GWbs0E/0qqP28lE+8TGkXB/WXYL"


def _load_token() -> str:
    """The terminal rotates its token on restart; prefer the live copy."""
    import os

    env = os.environ.get("XAU_MCP_TOKEN")
    if env:
        return env.strip()
    for p in ("runs/live/mcp_token", "../runs/live/mcp_token"):
        try:
            with open(p) as fh:
                t = fh.read().strip()
            if t:
                return t
        except OSError:
            pass
    return TOKEN
HOST = "127.0.0.1"
PORT = 22346
PATH = "/mcp"
MAX_LIMIT = 100_000

PERIOD_MINUTES = {
    "M1": 1, "M2": 2, "M3": 3, "M4": 4, "M5": 5, "M6": 6,
    "M10": 10, "M12": 12, "M15": 15, "M20": 20, "M30": 30,
    "H1": 60, "H2": 120, "H3": 180, "H4": 240, "H6": 360,
    "H8": 480, "H12": 720, "D1": 1440,
}


# ---------------------------------------------------------------------------
# EU daylight saving: UTC+2 (EET) in winter, UTC+3 (EEST) in summer.
# Summer time: last Sunday of March 01:00 UTC -> last Sunday of October 01:00 UTC.
# ---------------------------------------------------------------------------
def _last_sunday(year: int, month: int) -> int:
    nxt = datetime(year + 1, 1, 1) if month == 12 else datetime(year, month + 1, 1)
    last = nxt - timedelta(days=1)
    return last.day - (last.weekday() + 1) % 7


def server_offset_hours(naive_server: datetime) -> int:
    """Hours to subtract from server-local time to reach UTC."""
    y = naive_server.year
    dst_start = datetime(y, 3, _last_sunday(y, 3), 1, 0)
    dst_end = datetime(y, 10, _last_sunday(y, 10), 1, 0)
    approx_utc = naive_server - timedelta(hours=3)
    return 3 if dst_start <= approx_utc < dst_end else 2


def to_utc(server_dt: datetime) -> datetime:
    return server_dt - timedelta(hours=server_offset_hours(server_dt))


# ---------------------------------------------------------------------------
# MCP plumbing
# ---------------------------------------------------------------------------
class Mcp:
    def __init__(self) -> None:
        self.sid = ""
        self._initialize()

    def _post(self, body: dict) -> str:
        headers = {
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream",
            "Authorization": f"Bearer {TOKEN}",
        }
        if self.sid:
            headers["Mcp-Session-Id"] = self.sid
        conn = http.client.HTTPConnection(HOST, PORT, timeout=600)
        conn.request("POST", PATH, json.dumps(body), headers)
        resp = conn.getresponse()
        sid = resp.getheader("Mcp-Session-Id")
        if sid:
            self.sid = sid
        data = resp.read().decode()
        conn.close()
        return data

    def _initialize(self) -> None:
        self._post({
            "jsonrpc": "2.0", "id": 0, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "xau-fetcher", "version": "1.0"},
            },
        })
        self._post({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def call(self, tool: str, args: dict) -> dict:
        out = self._post({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": tool, "arguments": args},
        })
        obj = json.loads(out)
        if "error" in obj:
            raise RuntimeError(f"RPC error: {obj['error']}")
        result = obj.get("result", {})
        if result.get("isError"):
            raise RuntimeError(f"tool error: {result}")
        for c in result.get("content", []):
            if c.get("type") == "text":
                return json.loads(c["text"])
        raise RuntimeError("no text content in response")


# ---------------------------------------------------------------------------
# Fetch
# ---------------------------------------------------------------------------
def chart(mcp: Mcp, symbol: str, period: str, start: str, end: str) -> dict:
    return mcp.call("get_chart_history", {
        "symbol": symbol, "period": period,
        "datetime_from": start, "datetime_to": end,
        "limit": MAX_LIMIT,
    })


def warmup(mcp: Mcp, symbol: str, period: str, start: str, end: str,
           max_calls: int) -> str:
    """Force the terminal to backfill. Returns the earliest time it reached."""
    prev_first = None
    stable = 0
    earliest = None
    for i in range(1, max_calls + 1):
        page = chart(mcp, symbol, period, start, end)
        hist = page.get("history") or []
        if not hist:
            print(f"  warmup {i:>3}: no bars yet", flush=True)
            stable += 1
            if stable >= 3:
                break
            continue
        first = hist[0]["time"]
        earliest = first
        print(f"  warmup {i:>3}: {len(hist):>6} bars, earliest {first}", flush=True)
        if first <= start:
            print("  warmup: reached requested start", flush=True)
            break
        if prev_first == first:
            stable += 1
            if stable >= 4:
                print("  warmup: history stops extending", flush=True)
                break
        else:
            stable = 0
        prev_first = first
    return earliest or start


def write_header(f) -> None:
    f.write("time,open,high,low,close,tick_volume,spread,real_volume,time_epoch\n")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--symbol", default="XAUUSD")
    ap.add_argument("--period", default="H1")
    ap.add_argument("--from", dest="start", required=True)
    ap.add_argument("--to", dest="end", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--warmup-calls", type=int, default=150)
    args = ap.parse_args()

    global TOKEN
    TOKEN = _load_token()

    start = f"{args.start}T00:00:00"
    end = f"{args.end}T00:00:00"
    step = timedelta(minutes=PERIOD_MINUTES[args.period])

    print(f"=== {args.symbol} {args.period}: {start} -> {end} ===", flush=True)
    mcp = Mcp()

    print("Phase 1: warming up terminal history", flush=True)
    warmup(mcp, args.symbol, args.period, start, end, args.warmup_calls)

    print("\nPhase 2: paging forward", flush=True)
    written = 0
    last_written: str | None = None
    cursor = start
    calls = 0
    first_utc = None
    last_utc = None

    with open(args.out, "w", encoding="utf-8") as f:
        write_header(f)
        while True:
            if calls > 2000:
                raise RuntimeError("pagination runaway guard")
            page = chart(mcp, args.symbol, args.period, cursor, end)
            calls += 1
            hist = page.get("history") or []
            if not hist:
                print(f"  page {calls}: no more bars", flush=True)
                break

            new = 0
            for b in hist:
                t = b["time"]
                if last_written is not None and t <= last_written:
                    continue
                server_dt = datetime.fromisoformat(t)
                utc_dt = to_utc(server_dt)
                epoch = int(utc_dt.replace(tzinfo=timezone.utc).timestamp())
                f.write(
                    f"{utc_dt.strftime('%Y.%m.%d %H:%M')},{b['open']},{b['high']},"
                    f"{b['low']},{b['close']},{int(b.get('tick_volume') or 0)},"
                    f"{int(b.get('spread') or 0)},{int(b.get('real_volume') or 0)},"
                    f"{epoch}\n"
                )
                last_written = t
                if first_utc is None:
                    first_utc = utc_dt
                last_utc = utc_dt
                new += 1
            written += new
            f.flush()

            print(
                f"  page {calls:>3}: {len(hist):>6} bars, +{new:>6} new, "
                f"total {written:>7}, through {last_utc}",
                flush=True,
            )

            if len(hist) < MAX_LIMIT:
                break
            if new == 0:
                print("  page made no progress; stopping", flush=True)
                break
            cursor = (datetime.fromisoformat(hist[-1]["time"]) + step).isoformat()

    print(f"\nWrote {written} rows to {args.out} in {calls} page call(s)")
    if written:
        print(f"First UTC bar: {first_utc}")
        print(f"Last  UTC bar: {last_utc}")
    return 0 if written else 1


if __name__ == "__main__":
    sys.exit(main())

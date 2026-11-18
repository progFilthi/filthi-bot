"""Minimal MCP client for the MT5 terminal.

Used by the demo runner and the preflight check. Deliberately small: one
session, one call helper, plus the handful of typed helpers the strategy
needs. Everything is read-only unless a caller explicitly invokes a
`trade_*` method.

The terminal returns SERVER-LOCAL timestamps (MetaQuotes-Demo runs EET/EEST:
UTC+2 winter, UTC+3 summer). All helpers that return bars convert to UTC, so
callers never have to think about it.
"""

from __future__ import annotations

import http.client
import json
import os
from datetime import datetime, timedelta

HOST = "127.0.0.1"
PORT = 22346
PATH = "/mcp"
TOKEN = os.environ.get(
    "MT5_MCP_TOKEN", "oo6totlCD01Iu//GWbs0E/0qqP28lE+8TGkXB/WXYL"
)

PERIOD_MINUTES = {
    "M1": 1, "M5": 5, "M15": 15, "M30": 30,
    "H1": 60, "H4": 240, "D1": 1440,
}


class McpError(RuntimeError):
    pass


# ---------------------------------------------------------------------------
# EU daylight saving (mirrors scripts/fetch_mcp_history.py)
# ---------------------------------------------------------------------------
def _last_sunday(year: int, month: int) -> int:
    nxt = datetime(year + 1, 1, 1) if month == 12 else datetime(year, month + 1, 1)
    last = nxt - timedelta(days=1)
    return last.day - (last.weekday() + 1) % 7


def server_offset_hours(naive_server: datetime) -> int:
    y = naive_server.year
    dst_start = datetime(y, 3, _last_sunday(y, 3), 1, 0)
    dst_end = datetime(y, 10, _last_sunday(y, 10), 1, 0)
    approx_utc = naive_server - timedelta(hours=3)
    return 3 if dst_start <= approx_utc < dst_end else 2


def to_utc(server_dt: datetime) -> datetime:
    return server_dt - timedelta(hours=server_offset_hours(server_dt))


class Mcp:
    def __init__(self, timeout: int = 60) -> None:
        self.timeout = timeout
        self.sid: str | None = None
        self._initialize()

    # -- plumbing ---------------------------------------------------------
    def _post(self, body: dict) -> dict:
        headers = {
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream",
            "Authorization": f"Bearer {TOKEN}",
        }
        if self.sid:
            headers["Mcp-Session-Id"] = self.sid
        conn = http.client.HTTPConnection(HOST, PORT, timeout=self.timeout)
        conn.request("POST", PATH, json.dumps(body), headers)
        resp = conn.getresponse()
        sid = resp.getheader("Mcp-Session-Id")
        if sid:
            self.sid = sid
        raw = resp.read().decode()
        status = resp.status
        conn.close()
        # 202 = accepted notification (empty body); 200 = normal result.
        if status not in (200, 202):
            raise McpError(f"HTTP {status}: {raw[:400]}")
        if not raw.strip():
            return {}
        try:
            return json.loads(raw)
        except json.JSONDecodeError as exc:
            raise McpError(f"non-JSON response: {raw[:400]}") from exc

    def _initialize(self) -> None:
        self._post({
            "jsonrpc": "2.0", "id": 0, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "xau-demo-runner", "version": "1.0"},
            },
        })
        self._post({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def call(self, tool: str, args: dict | None = None):
        obj = self._post({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": tool, "arguments": args or {}},
        })
        if "error" in obj:
            raise McpError(f"{tool}: {obj['error']}")
        result = obj.get("result", {})
        texts = [
            c.get("text", "")
            for c in result.get("content", [])
            if c.get("type") == "text"
        ]
        if result.get("isError"):
            raise McpError(f"{tool} failed: {' '.join(texts)[:600]}")
        if not texts:
            return None
        payload = texts[0]
        try:
            return json.loads(payload)
        except json.JSONDecodeError:
            return payload

    # -- read helpers -----------------------------------------------------
    def account(self) -> dict:
        return self.call("get_trading_account_info")

    def positions(self) -> dict:
        return self.call("get_trading_open_positions")

    def server_now(self) -> datetime:
        t = self.call("get_time_information")
        return datetime.fromisoformat(t["trade_server_last_known_time"])

    def now_utc(self) -> datetime:
        return to_utc(self.server_now())

    def bars(self, period: str = "H1", count: int = 400) -> list[dict]:
        """Last `count` bars, oldest first, timestamps in UTC.

        The final bar may be the still-forming one; callers decide.
        """
        now = self.server_now()
        per = PERIOD_MINUTES[period]
        # Ask for comfortably more calendar time than `count` bars need.
        span_days = int(count * per / 60 / 24 * 2.2) + 10
        start = (now - timedelta(days=span_days)).strftime("%Y-%m-%dT%H:%M:%S")
        end = (now + timedelta(days=1)).strftime("%Y-%m-%dT%H:%M:%S")
        page = self.call("get_chart_history", {
            "symbol": "XAUUSD", "period": period,
            "datetime_from": start, "datetime_to": end,
            "limit": 100_000,
        })
        hist = page.get("history") or []
        hist = hist[-count:]
        out = []
        for b in hist:
            utc = to_utc(datetime.fromisoformat(b["time"]))
            out.append({
                "ts": utc,
                "open": float(b["open"]),
                "high": float(b["high"]),
                "low": float(b["low"]),
                "close": float(b["close"]),
                "volume": int(b.get("tick_volume") or 0),
                "spread": int(b.get("spread") or 0),
            })
        return out

    def price(self) -> float:
        """Current price (close of the latest M1 bar)."""
        return self.bars("M1", 2)[-1]["close"]

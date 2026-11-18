"""D3 strategy maths, ported 1:1 from crates/simulator/src/strategy.rs.

Keeping this a faithful port matters: the demo run is only evidence about the
backtest if the arithmetic is identical. The Rust side is the reference; if you
change a parameter there, change it here.

Signals are computed on CLOSED bars only and are acted on at the next bar's
open, exactly like `engine::backtest`.
"""

from __future__ import annotations

from dataclasses import dataclass

# --- D3 parameters (configs/phase0.toml [strategy]) -------------------------
LOOKBACK = 55
TREND_MA = 300
ATR_PERIOD = 14
SL_ATR = 1.5
TP_ATR = 0.0        # no take profit
TRAIL_ATR = 2.5
MAX_HOLD_BARS = 240


def ema(values: list[float], period: int) -> list[float]:
    """Seeded with the SMA of the first window, then k = 2/(period+1)."""
    n = len(values)
    out = [float("nan")] * n
    if period <= 0 or n < period:
        return out
    k = 2.0 / (period + 1.0)
    seed = sum(values[:period]) / period
    out[period - 1] = seed
    prev = seed
    for i in range(period, n):
        prev = values[i] * k + prev * (1.0 - k)
        out[i] = prev
    return out


def atr(bars: list[dict], period: int) -> list[float]:
    """Wilder's ATR in price units."""
    n = len(bars)
    out = [float("nan")] * n
    if period <= 0 or n < period + 1:
        return out
    trs = [bars[0]["high"] - bars[0]["low"]]
    for i in range(1, n):
        p = bars[i - 1]
        b = bars[i]
        trs.append(
            max(
                b["high"] - b["low"],
                abs(b["high"] - p["close"]),
                abs(b["low"] - p["close"]),
            )
        )
    seed = sum(trs[1 : period + 1]) / period
    out[period] = seed
    prev = seed
    for i in range(period + 1, n):
        prev = (prev * (period - 1) + trs[i]) / period
        out[i] = prev
    return out


def donchian(bars: list[dict], lookback: int) -> tuple[list[float], list[float]]:
    """Highest high / lowest low of the `lookback` bars BEFORE index i."""
    n = len(bars)
    hi = [float("nan")] * n
    lo = [float("nan")] * n
    if lookback == 0 or n <= lookback:
        return hi, lo
    for i in range(lookback, n):
        window = bars[i - lookback : i]
        hi[i] = max(b["high"] for b in window)
        lo[i] = min(b["low"] for b in window)
    return hi, lo


@dataclass
class Signal:
    side: str | None       # "buy" | "sell" | None
    reason: str
    close: float
    donchian_hi: float
    donchian_lo: float
    ema: float
    atr: float


def evaluate(bars: list[dict]) -> Signal:
    """Signal from the most recent CLOSED bar (the last element of `bars`)."""
    if len(bars) < max(LOOKBACK, TREND_MA, ATR_PERIOD) + 2:
        raise ValueError(
            f"need at least {max(LOOKBACK, TREND_MA, ATR_PERIOD) + 2} bars, "
            f"got {len(bars)}"
        )
    closes = [b["close"] for b in bars]
    hi, lo = donchian(bars, LOOKBACK)
    trend = ema(closes, TREND_MA)
    a = atr(bars, ATR_PERIOD)

    i = len(bars) - 1
    close = closes[i]
    up_ok = trend[i] != trend[i] or close > trend[i]   # NaN check
    dn_ok = trend[i] != trend[i] or close < trend[i]

    side = None
    reason = "no breakout"
    if close > hi[i] and up_ok:
        side = "buy"
        reason = f"close {close:.2f} > {LOOKBACK}-bar high {hi[i]:.2f}"
    elif close < lo[i] and dn_ok:
        side = "sell"
        reason = f"close {close:.2f} < {LOOKBACK}-bar low {lo[i]:.2f}"
    elif close > hi[i]:
        reason = f"breakout up but below EMA{TREND_MA}"
    elif close < lo[i]:
        reason = f"breakdown but above EMA{TREND_MA}"

    return Signal(
        side=side,
        reason=reason,
        close=close,
        donchian_hi=hi[i],
        donchian_lo=lo[i],
        ema=trend[i],
        atr=a[i],
    )


def stop_price(side: str, entry: float, atr_value: float) -> float:
    return entry - SL_ATR * atr_value if side == "buy" else entry + SL_ATR * atr_value


def trail_price(side: str, best: float, atr_value: float) -> float:
    return best - TRAIL_ATR * atr_value if side == "buy" else best + TRAIL_ATR * atr_value

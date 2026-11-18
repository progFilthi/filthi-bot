# Backtest Report — XAUUSD, 0.10 lots

**Date:** 2026-11-18
**Data:** `data/cache/xauusd_m1.parquet` — 100,000 M1 bars
**Period:** 2026-08-03 → 2026-11-17 (~3.5 months)
**Price range:** 3832.35 → 4696.73
**Runner:** `cargo run -p xau-sim --bin xau-backtest`

## Contract math (verified against the run)

XAUUSD: 1 point = 0.01 price, contract size = 100 oz.

| Lot size | $ per point | SL 100 pts | TP 200 pts |
| -------- | ----------- | ---------- | ---------- |
| 0.01     | $0.01       | $1.00      | $2.00      |
| 0.10     | $0.10       | $10.00     | $20.00     |
| 1.00     | $1.00       | $100.00    | $200.00    |

`$ per point = 100 (oz) × lots × 0.01 (point)`.

## Primary run — 0.10 lots, MA 20/50, SL 100 / TP 200, spread 20 pts

| Metric            | Value        |
| ----------------- | ------------ |
| Trades            | 2,204        |
| Wins / Losses     | 706 / 1,498  |
| Win rate          | 32.03%       |
| Total PnL         | −$5,268.00   |
| Gross profit      | $12,708.00   |
| Gross loss        | −$17,976.00  |
| Final equity      | $19,732.00   |
| Max drawdown      | $5,388.00    |
| Profit factor     | 0.707        |
| EV per trade      | −$2.39       |
| Return on deposit | −21.07%      |

Run with `--maxdd 20` (the production kill-switch) the sim halts at $19,990.00 after
2,055 trades, for a −$5,010.00 / −20.04% result. Same conclusion, earlier stop.

## Breakeven math

- Win = TP 200 − spread 20 = **180 pts → +$18.00**
- Loss = SL 100 + spread 20 = **120 pts → −$12.00**
- Breakeven win rate: `18p = 12(1 − p)` → **p = 40.0%**

Actual win rate is 32.03%, i.e. ~8 points below breakeven. The system is not
marginally losing — it is structurally losing.

## Is there any raw edge without the spread?

Re-running with `--spread 0` isolates the signal:

| Config                    | Trades | Win rate | Total PnL | Profit factor |
| ------------------------- | ------ | -------- | --------- | ------------- |
| MA 20/50, spread 0        | 2,204  | 32.03%   | −$860.00  | 0.943         |
| MA 20/50, SL 300 / TP 600 | 2,026  | 35.24%   | −$572.00  | 0.986         |

At a clean 2:1 R:R the breakeven win rate is 33.33%. The strategy lands at
32–35%, so the underlying signal is worth roughly **zero** before costs. Cost
is what converts "flat" into "steady bleed".

## MA period sweep (0.10 lots, spread 20 pts, no kill-switch)

| Fast/Slow | Trades | Win rate | Total PnL   | Profit factor | EV/trade |
| --------- | ------ | -------- | ----------- | ------------- | -------- |
| 5 / 20    | 6,076  | 32.78%   | −$13,152.00 | 0.732         | −$2.16   |
| 10 / 30   | 3,846  | 33.59%   | −$7,392.00  | 0.759         | −$1.92   |
| 20 / 50   | 2,204  | 32.03%   | −$5,268.00  | 0.707         | −$2.39   |
| 50 / 200  | 631    | 33.12%   | −$1,302.00  | 0.743         | −$2.06   |

Every parameter set clusters at 32–34% win rate. Faster MAs trade more and lose
more; slower MAs trade less and lose less. No setting is profitable.

## Lot size scaling (MA 20/50, same signals)

| Lots | Total PnL | Return on deposit | EV/trade |
| ---- | --------- | ----------------- | -------- |
| 0.01 | −$526.80  | −2.11%            | −$0.24   |
| 0.10 | −$5,268.00| −21.07%           | −$2.39   |

PnL scales linearly with lot size, as expected. **Raising lot size does not
create an edge — it multiplies a negative one.** The loss percentage is
identical; only the dollar figure changes.

> Correction to earlier notes in this project: at 0.10 lots, 1 point is worth
> **$0.10**, not $1.00. The $1.00 figure belongs to 1.00 lots. This is confirmed
> by the gross numbers: 1,498 losses × $12.00 (−120 pts) = −$17,976.00.

## Verdict

**The strategy does not have an edge.** It wins 32% of the time while needing
40% to break even, and its raw signal is flat-to-negative even with zero spread.
At 0.10 lots over 3.5 months it loses 21% of the account. This is not a
lot-size or broker-cost problem — it is a strategy problem.

Possible next directions, in order of effort:

1. **Filter the signal, don't replace it.** MA crossover is a trend follower
   getting chopped in this data. Require an ADX/ATR regime filter, or only take
   crosses aligned with the higher-timeframe trend.
2. **Widen the stop, keep 2:1.** SL 300 / TP 600 already cut the loss by 89%.
   Longer holds reduce spread drag per unit of move.
3. **Test a different family entirely.** Mean reversion (Bollinger / RSI
   extremes) fits range-bound gold better than crossover.
4. **Do not scale lot size** until profit factor is above 1.2 on a walk-forward
   split. Scaling a negative-EDGE system accelerates the loss.

## Reproduce

```bash
cargo run -p xau-sim --bin xau-backtest -- \
  --lots 0.10 \
  --parquet data/cache/xauusd_m1.parquet \
  --fast 20 --slow 50 --sl 100 --tp 200 --spread 20 --maxdd 100
```

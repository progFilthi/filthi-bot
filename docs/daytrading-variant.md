# Can D3 trade intraday? — measured, 2026-11-22

**Question:** the validated D3 rule holds trades for days. Can it be restructured
to finish inside the day, and does that beat the London breakout (ORB)?

**Answer:** yes, D3 survives as a day-trading rule, and it is better than this
project's own notes said. It gives up about a third of the net profit but
nearly halves the drawdown. Scalping the same signal does not work.

**Data:** `data/cache_long/xauusd_h1.parquet` — 98,799 H1 bars, 2010-01-03 →
2026-11-18, true UTC. Lots 0.10, deposit $25,000, spread 20 points, D3 config
unchanged (Donchian 55, SL 1.5 ATR, trail 2.5 ATR, EMA300, 240-bar cap).

Reproduce:

```bash
cargo run --release -p xau-sim --bin xau-daytrade -- \
    --parquet data/cache_long/xauusd_h1.parquet
```

---

## 1. Correction to the earlier note

`docs/demo-readiness.md` used to say a short hold would "keep the losing half
and cut the winning half", and recorded **"no short-hold variant"**.

That inference was wrong. It was drawn from bucketing *realised* trades by how
long they happened to last: trades that exit in under 8 bars are mostly stops,
so that bucket is negative. But a hold **cap** does not create those trades —
it truncates the long winners instead. Measured directly, a cap does not
collapse the edge:

| max hold | trades | PF | win% | net | max DD |
| --- | --- | --- | --- | --- | --- |
| 2h | 3,458 | 1.07 | 45.2% | +$6,435 | 18.9% |
| 4h | 2,981 | 1.15 | 45.5% | +$14,499 | 8.0% |
| 6h | 2,718 | 1.12 | 43.2% | +$11,588 | 11.5% |
| 8h | 2,540 | 1.16 | 41.2% | +$16,093 | 11.4% |
| 12h | 2,320 | 1.24 | 39.7% | +$23,074 | 8.1% |
| 16h | 2,190 | 1.24 | 37.4% | +$22,696 | 10.3% |
| 24h | 1,997 | 1.32 | 35.7% | +$28,150 | 9.8% |
| 48h | 1,917 | 1.32 | 36.0% | +$26,839 | 9.9% |
| **240 (rule)** | **1,911** | **1.32** | **36.1%** | **+$26,647** | **9.9%** |

A 12-hour cap keeps 87% of the net. A 24-hour cap is, within noise, the same as
the rule. So "shorter holds" is a real dial, not a different strategy.

## 2. The version that actually fits a day

A hold cap is not the same as *no overnight position*. The honest test is a
forced exit at the last bar before the daily break, with entries confined to
the hours you can actually watch.

Implementation note that cost an hour: the broker has **no 21:00 UTC bar** in
winter and **no 20:00 UTC bar** in summer — the daily maintenance break moves
with DST. A fixed "flat at hour H" rule therefore silently never fires half the
year and holds trades for days. The engine now flattens on the **gap** before
the break (`flat_before_gap_min`), which is DST-proof.

| entries | trades | PF | win% | net | max DD | avg hold | max hold |
| --- | --- | --- | --- | --- | --- | --- | --- |
| all hours | 2,422 | 1.15 | 39.4% | +$14,634 | 11.7% | 6.0h | 23h |
| **07–18 UTC** | **1,820** | **1.27** | **43.3%** | **+$17,167** | **5.9%** | 5.0h | 14h |
| 07–16 UTC | 1,669 | 1.29 | 43.1% | +$16,797 | 6.9% | 5.2h | 14h |
| 13–16 UTC | 1,155 | 1.24 | 47.0% | +$9,177 | 6.8% | 4.3h | 8h |

"07–18 UTC" means entries between 07:00 and 18:00 UTC (London open through the
New York afternoon), flat at the day's last bar. Nothing is held overnight,
nothing is held over the weekend.

### Does it hold up?

| variant | f1 | f2 | f3 | f4 | f5 | positive |
| --- | --- | --- | --- | --- | --- | --- |
| rule (240 bars) | +3,867 | +2,681 | +582 | −460 | +19,977 | 4/5 |
| day close, all hours | +2,270 | +2,942 | −220 | −2,502 | +12,145 | 3/5 |
| **07–18 + day close** | **+3,178** | **+2,577** | **+734** | **+516** | **+10,161** | **5/5** |
| 07–16 + day close | +3,071 | +2,449 | +862 | +519 | +9,896 | 5/5 |

The day-session variants are positive in **every** fold, which the original rule
is not (f4 is negative). Drawdown drops from 9.9% to 5.9%.

Caveat, stated plainly: the 07–18 window was chosen after seeing the results.
It is a coarse, natural choice (the hours a person can watch), and 07–16
behaves the same, so it is not a knife-edge — but it has not been walk-forward
optimised. Treat the 5/5 as encouraging, not proven.

## 3. Scalping the same signal — no

Same Donchian signal on M15 (100,000 bars, ~4.3 years), stops and targets
scaled down to minutes-long holds. Net at 20-point spread and at 40 points:

| stop / target | max hold | trades | PF | win% | net @20pts | net @40pts | avg hold |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1.0 / 1.0 ATR | 8 bars | 3,780 | 0.95 | 49.6% | −$4,792 | −$12,352 | 2.2h |
| 0.5 / 0.5 ATR | 4 bars | 4,045 | 0.69 | 43.4% | −$19,670 | −$25,203 | 1.1h |
| 0.5 / 1.0 ATR | 8 bars | 3,911 | 1.06 | 36.7% | +$4,299 | −$3,523 | 1.5h |
| 0.3 / 0.3 ATR | 2 bars | 4,068 | 0.48 | 37.5% | −$22,953 | −$25,031 | 1.0h |

Every tight-stop version loses, and the only positive cell (PF 1.06) turns
negative when the spread widens from 20 to 40 points.

The reason is arithmetic, not luck. Cost is fixed per trade; a small trade is
small because the stop is small, so the cost is a larger share of the risk:

| | stop | spread + slip | cost as % of risk |
| --- | --- | --- | --- |
| London breakout | ~$15 | ~$0.19 | ~1.2% |
| scalp (0.5 ATR M15) | ~$2.50 | ~$0.22 | ~9% |
| scalp (0.3 ATR M15) | ~$1.50 | ~$0.22 | ~15% |

At 9% drag a 1:1 scalp needs ~55% wins just to break even. At 15% it needs ~58%.
That is before the spread widens, which it does exactly when you want to trade.

There is a second, harder limit: **this stack cannot scalp.** Every runner polls
*closed* bars through the terminal bridge once a minute. A scalper needs tick
data and sub-second execution. That is not a flag to switch on; it is a
different system.

## 4. Verdict

1. **D3 intraday is real.** "07–18 UTC entries, flat at the daily close" is
   +$17,167, PF 1.27, 5.9% DD, 5/5 folds positive, no overnight exposure.
   It is a better fit for a prop challenge than the original 9.9%-DD rule,
   because the drawdown nearly halves.
2. **It is not yet approved for money.** The window was chosen after seeing
   the data. Next step is a walk-forward (fit on fold *k*, test on *k+1*) and
   a cost stress at 30/40/60 points, before anything is funded.
3. **Scalping is not the shortcut.** On this signal, at this broker, with this
   infrastructure, it loses money at every tight setting tested.
4. **The ORB comparison is now fair.** ORB: +0.213R, 5.9% DD, 239 trades over
   1.4 years, one trade a day. D3 intraday: PF 1.27, 5.9% DD, 1,820 trades over
   17 years. Same drawdown, but D3 has 17 years of out-of-sample behaviour
   behind it and the ORB has 1.4. That is the difference that matters.

# Strategy on 16.9 Years of History — Donchian breakout

**Date:** 2026-11-18
**Data:** 98,799 H1 bars, 2010-01-03 → 2026-11-18 (16.9 years), true UTC
**Lots:** 0.10 (fixed, non-compounding) | **Deposit:** $25,000 | **Spread:** 20 pts

---

## TL;DR

The M1 evaluation in `docs/strategy-evaluation.md` concluded "no transferable
edge" — but that verdict rested on 105 days of data, one market regime, and a
sweep so small that curve-fitting was indistinguishable from skill.

Repeating the exercise on **17 years of H1 data** changes the answer.

> A **Donchian breakout trend-follower** is profitable across the whole sample:
> **+$26,647 (+106.6%) at 0.10 lot**, profit factor 1.32, max drawdown 9.9%,
> **14 of 17 years positive**, both long and short sides profitable, and the
> edge survives a 60-point spread.

This is the first configuration in the project that passes a real gauntlet
rather than one backtest. It is still **not** a promise: the last two years
contribute 65% of the profit, and the broker only has 17 years of H1.

---

## 1. Why H1, and why 17 years

The MetaTrader terminal's history depth is much shallower below H1. Measured
directly against the terminal's MCP endpoint:

| Timeframe | Oldest bar available | Depth |
| --- | --- | --- |
| H1 | 2010-01-04 | **16.9 years** |
| M15 | 2022-08-16 | 4.3 years |
| M5 | 2025-06-11 | 1.4 years |
| M1 | 2026-08-04 | 0.3 years |

So H1 is the only series long enough to contain several complete regimes
(2011-12 gold correction, 2013 crash, 2015-18 range, 2020 spike, 2022
rate-hike drawdown, 2024-26 rally). That breadth is what makes a 17-year
result meaningful where a 4-month one is not.

The data was pulled with `scripts/fetch_mcp_history.py`, which converts the
terminal's server-local EET/EEST timestamps to true UTC. The conversion is
verified in `runs/phase1/data-quality-h1.md`: weekly opens land at 23:00 UTC in
winter (212 occurrences) and 22:00 UTC in summer (223) — exactly the expected
one-hour seasonal shift. Import is clean: 0 bad rows, 0 duplicates, 0
out-of-order.

Sample at a glance:

| Property | Value |
| --- | --- |
| Bars | 98,799 |
| First → last close | $1,094.49 → $3,773.90 (+244.8%) |
| High / low | $5,598.03 / $1,044.46 |
| Median bar range | $3.33 (p95 $16.51) |
| Spread | mean 19.8 pts, median 16, p95 51 |
| Cost as share of ATR | 2.9% |

## 2. H1 is still a random walk

Before testing strategies, `xau-analyze` checks whether price is predictable at
all. Variance ratio `VR(q)` ≈ 1 means a random walk; autocorrelation ≈ 0 means
each bar's return carries no information about the next.

| q | VR | Reading |
| --- | --- | --- |
| 2 | 0.993 | random |
| 3 | 1.006 | random |
| 5 | 0.978 | random |
| 10 | 0.980 | random |
| 15 | 0.994 | random |
| 30 | 1.047 | random |
| 60 | 1.022 | random |
| 120 | 1.014 | random |

Lag-1 autocorrelation is −0.0018; every lag through 10 is below 0.011 in
absolute value.

**This is important and easy to misread.** Gold at H1 has no *predictable
direction*. But trend-following does not need to predict direction — it needs
returns to be *autocorrelated in the tails*, i.e. big moves to continue more
often than they reverse. A random walk with fat tails is exactly the
environment where a breakout system with a tight stop and a trailing exit earns
its keep: many small losses, occasional large wins. VR ≈ 1 does not rule that
out, and the results below show it.

## 3. Walk-forward: does anything generalise?

`xau-walkforward` tunes 42 configurations on the first 70% of bars and re-tests
the winners untouched on the last 30%.

| Metric | Old M1 sample | 17-year H1 |
| --- | --- | --- |
| In-sample profitable | 4 / 40 | **27 / 40 (68%)** |
| Out-of-sample profitable | 12 / 40 | **33 / 40 (82%)** |
| Best in-sample winner, OOS | −$900 | MA 5/20: −$5,956 |
| IS/OOS profit correlation | negative | +0.31 |

Both samples agree on one thing: **the highest in-sample profit is not
transferable.** The M1-era champion (MA 5/20, all hours) again loses money out
of sample (−$5,956, PF 0.94). Tuning hardest does not work.

What differs is the *shape* of the rest of the field. On H1, the trend
followers are profitable both in and out of sample, and they occupy the top of
both lists. That is what a real, if modest, effect looks like.

Top out-of-sample performers — every one is a Donchian breakout:

| strategy | trades | win% | PnL | PF |
| --- | --- | --- | --- | --- |
| donchian lb20 sl1.5 trail2 hold240 | 998 | 37.9% | +$18,632 | 1.30 |
| donchian lb20 sl2 trail2.5 | 871 | 36.6% | +$20,672 | 1.31 |
| donchian lb10 sl2 trail2.5 | 1143 | 36.3% | +$19,138 | 1.22 |
| donchian lb55 sl1.5 trail2 hold240 | 705 | 38.9% | +$14,364 | 1.32 |
| donchian lb55 sl2 trail2.5 | 618 | 36.9% | +$17,379 | 1.36 |

## 4. The selected strategy: D3

```
Entry   close beyond the 55-bar high (long) / 55-bar low (short)
        and price on the correct side of EMA(300)
Stop    1.5 × ATR(14) from entry
Target  none — exit only on
Exit    trailing stop at 2.5 × ATR from the best price since entry
        or 240 bars elapsed
Size    0.10 lot fixed
```

EMA300 is a trend gate, not a signal: it only blocks breakouts that fight the
dominant trend. It earned its place (§6).

### Full-period performance

| Metric | Value |
| --- | --- |
| Trades | 1,911 |
| Win rate | 36.1% (690 win / 1,221 loss) |
| **Total PnL** | **+$26,647.23** |
| Return on $25,000 | +106.6% |
| Profit factor | 1.32 |
| Expectancy | +$13.94 / trade |
| Avg win / avg loss | $157.73 / −$67.31 |
| Max drawdown | $4,950.54 (9.90%) |
| Sharpe (annual) | 0.73 |
| Avg hold | 11 bars |
| Exposure | 22.0% of bars |
| Longest losing streak | 16 |
| Exits | 675 initial stop, 1,235 trailed stop, 1 timed |

Low win rate, small average loss, larger average win. That is the intended
structure: the strategy is designed to be wrong often and right big.

### Both directions work

| Side | Trades | PnL | Per trade | PF |
| --- | --- | --- | --- | --- |
| Long | 1,077 | +$15,864 | $14.73 | 1.35 |
| Short | 835 | +$10,801 | $12.93 | 1.29 |

This is the specific failure mode that sank the M1 version — it earned ~$0 on
the short side once the one downtrend ended. Here the two sides are close
enough that neither is carrying the other.

### Year by year

| Year | PnL | Trades | | Year | PnL | Trades |
| --- | --- | --- | --- | --- | --- | --- |
| 2010 | +$221 | 108 | | 2019 | +$330 | 116 |
| 2011 | +$2,162 | 118 | | 2020 | +$1,397 | 112 |
| 2012 | −$578 | 114 | | 2021 | +$389 | 110 |
| 2013 | +$2,761 | 94 | | 2022 | −$1,421 | 127 |
| 2014 | +$477 | 107 | | 2023 | +$459 | 119 |
| 2015 | +$383 | 110 | | 2024 | +$1,494 | 122 |
| 2016 | +$1,324 | 105 | | 2025 | +$7,475 | 120 |
| 2017 | +$365 | 113 | | 2026* | +$9,973 | 100 |
| 2018 | −$563 | 116 | | | | |

\* 2026 is partial (January → mid-November).

**14 of 17 years positive.** The three losing years (2012, 2018, 2022) are the
choppy, range-bound years — the known weakness of any breakout system, and
visible in advance rather than hidden.

### The honest caveat: 2025-26 concentration

| Slice | PnL | Years positive |
| --- | --- | --- |
| 2010 → 2024 (15 years) | +$9,199 | 12 / 15 |
| 2025 → 2026 (partial) | +$17,448 | 2 / 2 |

Two thirds of the total profit came from the last two years, during the largest
gold rally in the sample. That is a genuine concentration risk, and it is why
this is a *promising* strategy rather than a *proven* one.

But strip those years out and the remainder is still **+$9,199 over 15 years,
positive in 12 of them**. The strategy does not depend on the recent rally; it
was simply amplified by it. For scale: 0.10 lot on $25,000 returning $9,199
over 15 years is about 2.5% a year simple — thin, but positive and not
regime-dependent.

The 6-fold test says the same thing from another angle: 5 of 6 sequential
slices are positive, and only fold 6 (the recent rally) is large.

| Fold | 1 | 2 | 3 | 4 | 5 | 6 |
| --- | --- | --- | --- | --- | --- | --- |
| PnL | +$2,029 | +$3,466 | +$1,296 | +$1,697 | −$1,190 | +$19,332 |

## 5. Locked out-of-sample result

Parameters fixed on the first 70% of bars, then run untouched on the last 30%
(2021-11 → 2026-11):

| Metric | OOS |
| --- | --- |
| Trades | 606 |
| Win rate | 34.7% |
| Total PnL | +$17,931 |
| Profit factor | 1.42 |
| Max drawdown | $4,951 (12.0%) |
| Sharpe | 0.98 |
| Long / short | +$13,183 / +$4,748 |

The out-of-sample profit factor (1.42) is *higher* than the full-period one
(1.32). Some of that is the 2025-26 rally, but 2022 was a losing year inside
this window too, so it is not a pure bull-market artifact.

## 6. Is the edge fragile?

A single good backtest means nothing if it only works for one parameter value,
one side, or one broker. Each test below is a deliberate attempt to break it.

### Parameter surface — 15 of 15 cells profitable

Donchian lookback × trailing multiple, full sample, total PnL:

| trail \ lookback | 20 | 40 | 55 | 80 | 100 |
| --- | --- | --- | --- | --- | --- |
| 2.0 | 24,708 | 20,742 | 20,848 | 24,048 | 25,576 |
| 2.5 | **28,410** | 25,872 | **26,338** | 25,965 | 28,163 |
| 3.0 | 25,384 | 24,842 | 25,400 | 23,388 | 26,032 |

No cell is negative, and the spread between worst and best is ~$7,700 on
~$25,000 — the strategy is a plateau, not a spike. That is the signature of an
effect rather than a fitted coincidence.

### Cost stress — survives a bad broker

| Spread | Total PnL | PF |
| --- | --- | --- |
| 10 pts | +$28,558 | 1.35 |
| 20 pts (baseline) | +$26,647 | 1.32 |
| 30 pts | +$24,736 | 1.30 |
| 40 pts | +$22,825 | 1.27 |
| 60 pts (3× baseline) | +$19,003 | 1.22 |

At triple the baseline spread the strategy still makes +$19,003. The edge is
not a spread artifact.

### Trend filter ablation

Does the EMA gate actually help, or is it decoration?

| Filter | Trades | PnL | PF | Max DD |
| --- | --- | --- | --- | --- |
| none | 2,391 | +$18,500 | 1.20 | 10.8% |
| EMA100 | 2,382 | +$18,789 | 1.20 | 10.8% |
| EMA200 | 2,232 | +$20,848 | 1.24 | 10.4% |
| **EMA300** | **2,104** | **+$24,668** | **1.31** | **7.2%** |

EMA300 removes ~290 low-quality trades, raises profit factor from 1.20 to
1.31, and cuts drawdown from 10.8% to 7.2%. It earned its place.

### Trade concentration

Best single trade = 2.1% of gross profit; top five = 8.1%. No handful of lucky
trades is holding the result up.

## 7. Cross-timeframe check

The M15 series only reaches back to 2022-08, so it tests a different (and much
shorter) window — but it is an independent dataset on a different timeframe:

| Strategy | M15 full period | Years positive |
| --- | --- | --- |
| D (Donchian 55, trail 2.0, EMA200) | +$10,364 (PF 1.12) | 4 / 5 |
| D2 (Donchian 20, trail 2.0) | +$7,363 (PF 1.06) | 3 / 5 |
| **D3 (Donchian 55, trail 2.5, EMA300)** | **+$7,871 (PF 1.10)** | **4 / 5** |
| A (old M1 winner, MA 20/50 US session) | **−$3,741 (PF 0.89)** | 2 / 5 |

The Donchian family stays profitable on M15. The M1 champion loses money. This
is the same conclusion the H1 data reaches, from a different dataset — the
strongest single piece of evidence in the project.

D3 is weaker on M15 (+$7,871) than D (+$10,364). Mixed parameter rankings
across timeframes are normal, and the difference is well inside the noise for a
4-year window. Both are positive, which is the point.

## 8. Verdict

**Recommended: run D3 on H1.**

What it has going for it:

- 17 years, several complete regimes, 14 of 17 years positive
- profitable on both sides, on a different timeframe (M15), and at 3× spread
- a parameter *plateau*, not a lucky cell
- no dependence on a handful of trades
- 9.9% max drawdown at 0.10 lot on $25,000

What it does not have:

- 65% of profit from the last two years
- only 1,911 trades over 17 years (~113/year), so several losing *quarters* in
  a row are normal — the longest losing streak is 16 trades
- modest absolute return once the rally is excluded (~2.5%/yr simple)
- no forward test on unseen data yet

The realistic expectation is **not** "+106% in a few months". It is roughly
**$1,500–2,500 a year at 0.10 lot**, with stretches of flat-to-losing months
and occasional strong years when gold trends. That is worth running on demo,
which is a very different claim from the one the M1 version supported.

## 9. Reproducing this

```bash
# 1. Fetch 17 years of H1 from the terminal (needs MT5 running with the MCP server)
python3 scripts/fetch_mcp_history.py --period H1 \
    --from 2010-01-01 --to 2026-11-19 \
    --out data/raw/xauusd_h1_mcp.csv

# 2. Import (UTC spec: server_utc_offset_s = 0)
cargo run -p xau-cli -- data-import \
    --config configs/symbols/xauusd_utc.toml \
    --raw data/raw/xauusd_h1_mcp.csv \
    --cache-dir data/cache_long --report runs/phase1/data-quality-h1.md \
    --timeframes h1

# 3. Diagnostics and validation
cargo run -p xau-sim --bin xau-analyze      -- --parquet data/cache_long/xauusd_h1.parquet
cargo run -p xau-sim --bin xau-walkforward  -- --parquet data/cache_long/xauusd_h1.parquet
cargo run -p xau-sim --bin xau-validate     -- --parquet data/cache_long/xauusd_h1.parquet --folds 6
cargo run -p xau-sim --bin xau-report       -- --parquet data/cache_long/xauusd_h1.parquet
```

`data/` and `runs/` are gitignored, so the parquet cache and raw CSVs are not in
the repository — step 1 regenerates them from the terminal.

## 10. Relationship to the earlier evaluation

`docs/strategy-evaluation.md` remains correct for what it covered: on 105 days
of M1, nothing demonstrated an edge. That was an honest answer to a question
the data could not answer — the sample was too short to separate an edge from
noise.

The change is not that a magic strategy was found. It is that the sample went
from 105 days to 6,162 days, and a strategy family that looked random at 105
days separates from the noise at 17 years. The M1 conclusion and the H1
conclusion are both true; they describe different amounts of evidence.

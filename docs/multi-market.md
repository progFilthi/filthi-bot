# More markets, measured — 2026-11-24

**Question:** the gold rule trades about twice a week. Would running the same
rule on other symbols give more trades without giving back the edge?

**Answer, so far: not on this broker's FX majors.** Gold is the outlier, and it
is a big one.

**Rule held constant:** Donchian 55, SL 1.5 ATR, trail 2.5 ATR, EMA300 gate,
240-bar cap, entries 07:00–18:00 UTC, flat before the daily break. Only the
contract maths changes per symbol (`--point`, `--contract`).

Reproduce:

```bash
cargo run --release -p xau-sim --bin xau-cross -- \
    --parquet data/cache_fx/eurusd_h1.parquet \
    --point 0.00001 --contract 100000 --spread-pts 3
```

## Results

| symbol | window | trades | PF | win% | net | net in R | maxDD in R | folds + |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| **XAUUSD** | 17y | 1,820 | **1.27** | 43.3% | +$17,167 | +281 | 40.5 | **5/5** |
| XAUUSD | last 3.1y | 338 | **1.43** | 47.3% | +$10,027 | +76 | 18.8 | **5/5** |
| EURUSD | 2.4y | 240 | 0.89 | 35.0% | −$303 | −17 | 31.6 | 2/5 |
| GBPUSD | 2.9y | 322 | 0.95 | 34.2% | −$217 | −11 | 30.4 | 2/5 |
| USDJPY | 2.9y | 279 | 1.02 | 34.1% | +$64 | +4 | 46.7 | 3/5 |
| US30 (Dow) | 2.9y | 186 | 0.91 | 48.4% | −$1,433 | −8 | 22.7 | 1/5 |
| US500 (S&P) | 2.9y | 209 | 1.03 | 49.8% | +$71 | +3 | 30.5 | 3/5 |

The indices were tested on Yahoo Finance hourly bars (the demo terminal only
holds two weeks of them), with the demo's live spread: 120 points on US30,
50 on US500. Both are noise at best. **The rule is gold-specific.**

Gold earns 281R where the best FX pair earns 4R, over a comparable stretch.
That is not a tuning difference.

## Why FX fails where gold works

Cost as a share of the risk taken, i.e. spread ÷ (1.5 × ATR):

| symbol | ATR (H1) | spread | cost / risk |
| --- | --- | --- | --- |
| XAUUSD | $16.07 | $0.15 | **0.62%** |
| AUDUSD | 0.00090 | 0.00002 | 1.48% |
| USDJPY | 0.201 | 0.005 | 1.66% |
| EURUSD | 0.00094 | 0.00003 | 2.14% |
| USDCHF | 0.00069 | 0.00004 | 3.86% |

FX majors pay 2.4–6× the toll gold does, and their intraday breakouts are
weaker. Both effects point the same way.

The second problem is correlation: EURUSD, GBPUSD, AUDUSD and NZDUSD are
largely one bet (USD strength). Three "extra markets" from that list would have
multiplied drawdown almost as fast as return.

## Data limits on this demo

The terminal's usable H1 history is uneven:

| symbol | earliest H1 bar | verdict |
| --- | --- | --- |
| XAUUSD | 2010 | usable |
| EURUSD | 2024-07 | 2.4 years |
| GBPUSD, USDJPY | 2024-01 | 2.9 years |
| AUDUSD | 2026-05 | 6 months |
| NZDUSD, USDCAD | 2026-11 | 3 weeks — useless |
| US30, US500, XAGUSD | 2026-11 | 2 weeks — **present but untestable** |

US30 and US500 are the interesting untested candidates: their cost-to-risk
ratio is close to gold's (US30 spread 1.2 points against an ATR of roughly 90).
They need an external history source before they can be judged.

## What this does not say

* It does not say the rule is broken. Gold, over the same 3 years, is the
  strongest it has been (PF 1.43, 5/5 folds).
* It does not close the door on indices, only on FX majors as available here.
* Two to three years is a short sample for a 34–47% win-rate system. FX could
  be flat rather than negative over a longer run. Flat is still not worth
  trading when gold is +76R over the same window.

## Sizing note that matters more than symbol choice

Gold's peak-to-trough drawdown is **40.5R over 17 years** (18.8R over the last
3). Against a 10% maximum-loss rule, 1R must stay near **0.25% of the
account** — which puts the annual return near 4%. The binding constraint on a
prop challenge is the drawdown limit, not the trade count, and no amount of
extra markets changes that arithmetic unless the extra markets are genuinely
uncorrelated.

## What a small live account would actually have done

`xau-cross --account 50 --live-lots 0.01` walks the real 17-year trade list
against a starting balance. At the smallest position the broker allows
(0.01 lot = 1 oz = $1 per $1 move):

* survives all 17 years and ends at **$1,766.70**
* worst balance **$23.66** — a **53% drawdown** from a $50 start
* worst single trade **−$99.54**, i.e. twice the starting balance

That last line is the whole problem. The account survived because the bad tail
arrived after it had grown. Had that trade come first, the account was gone.

Margin is the second wall: the demo is 1:100, and 0.01 lot of gold needs
**$36.20**. A $50 account would have $13.80 of free margin against a stop that
risks $30–40.

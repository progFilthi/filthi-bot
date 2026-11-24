# Can a $50 account scalp? — measured, 2026-11-24

**Question:** the gold rule trades twice a week, which is too slow to watch. Can
a small account make many small trades instead — and does that work at $50?

**Answer:** yes, on FX at M15, and the size fits. Not on gold. The strategy is
**Bollinger mean-reversion**, not breakout. Four years, ~4,800 trades per pair,
positive in every fold and in both directions, and it survives a 2.7× spread
widening. The catch is the shape of the payoff: 34% win rate and losing streaks
of 26 trades. That is not a bug, it is what this edge is.

**Data:** `data/cache_fx/{eurusd,gbpusd}_m15.parquet` — 99,953 M15 bars each,
2022-11-14 → 2026-11-23 (4.0 years), true UTC, imported from the demo terminal.
Cross-check on `data/cache_long/xauusd_m15.parquet` — 100,000 bars, 4.3 years.

Reproduce:

```bash
cargo run --release -p xau-sim --bin xau-scalp -- \
    --parquet data/cache_fx/eurusd_m15.parquet \
    --point 0.00001 --contract 100000 --spread-pts 3 --lots 0.10
```

---

## 1. The result

Rule: fade a close outside a 20-period, 2σ Bollinger band. Stop 0.5 ATR, target
1.5 ATR, hard time exit after 8 M15 bars (2 hours), entries 07:00–18:00 UTC,
flat before any weekend gap.

| symbol | spread | trades | PF | win% | net @0.10 lots | folds + |
| --- | --- | --- | --- | --- | --- | --- |
| EURUSD | 3 pts | 4,821 | **1.33** | 33.6% | +$4,113 | **4/4** |
| EURUSD | 6 pts | 4,821 | 1.20 | 33.6% | +$2,667 | **4/4** |
| EURUSD | 8 pts | 4,821 | 1.12 | 33.6% | +$1,703 | **4/4** |
| EURUSD | 12 pts | 4,821 | 0.99 | 33.5% | −$225 | 2/4 |
| GBPUSD | 4 pts | 4,786 | **1.23** | 32.4% | +$3,528 | **4/4** |
| GBPUSD | 6 pts | 4,786 | 1.16 | 32.4% | +$2,665 | **4/4** |
| gold | 20 pts | 4,713 | **1.32** | 34.8% | +$25,823 | **4/4** |

Breakout on the same bars, same exits, still does not work: Donchian PF
1.04–1.08 on both pairs. That confirms the earlier finding from the other
direction — it is the *signal*, not the timeframe, that was wrong.

## 2. Correction to `daytrading-variant.md` §3

That note said tight stops lose because the spread becomes 9–15% of the risk.
The arithmetic was right but the conclusion was too broad. At M15 the spread is
**10.2% of the stop** on EURUSD (3 points against a 29-point stop) — exactly
the range that note called fatal — and the rule is profitable. So cost at ~10%
is survivable; what was missing on gold H1 was an edge at that scale, not room
for the spread.

The difference is the payoff shape. Mean reversion pays 1:3 on a 34% win rate,
so the fixed cost is charged against a *large* average win. Breakout at M15 paid
roughly 1:1, where the same toll eats the whole edge.

## 3. Why I believe it

Each of these was run to try to break it:

| check | result |
| --- | --- |
| Four separate one-year folds | **4/4 positive** on both pairs |
| Buy only | EURUSD PF 1.34, GBPUSD 1.22 |
| Sell only | EURUSD PF 1.30, GBPUSD 1.30 |
| Spread widened 3 → 8 pts (2.7×) | still PF 1.12, 4/4 folds |
| Spread widened 3 → 12 pts (4×) | dies, PF 0.99 |
| Look-ahead | signal at close `i`, fill at open `i+1`; stop sized from ATR at `i-1` |
| Second instrument | gold M15 PF 1.32, 4/4 |
| Different signal family | Donchian 1.04–1.08 (no edge), RSI 1.62 but only 177 trades |

Both directions working matters most. EURUSD went 1.03 → 1.09 and GBPUSD
1.18 → 1.30 over the sample, so a long-only edge could have been drift. It is
not: sells are as good as buys.

## 4. The $50 account

At 0.01 lots, EURUSD is $0.10 per pip and the median stop is 2.9 pips — **29
cents of risk per trade**, 0.6% of $50. Walking the real 4-year trade list:

| | EURUSD @3 pts | EURUSD @8 pts | GBPUSD @4 pts |
| --- | --- | --- | --- |
| ends at | **$461.35** | $220.30 | $415.54 |
| worst balance | $48.21 | $47.86 | $44.32 |
| deepest drawdown | **11%** | 27% | 14% |
| worst single trade | −$1.41 | −$1.46 | −$1.66 |

This is the first configuration tested that actually fits $50. Compare with the
gold H1 rule, where the smallest position risks $30–40 a trade and the worst
single trade was −$99.54.

**Gold M15 does not fit**, even though the signal works there: the same test
wipes a $50 account at trade 550, and the 4-bar variant dips to $7.88 (−88%).
Gold's stop is $1.62 but its worst trade is −$29.64 and its margin is $36.20 of
the $50.

## 5. What this costs in discipline

* **34% win rate.** Two out of three trades lose, by design.
* **Longest losing streak: 26 trades.** At $0.30–0.45 each that is roughly 18%
  of a $50 account, given back in a row, without changing anything.
* **~1,200 trades a year**, about 5 a day. This is the "many small trades"
  cadence, and it is the reason the edge is small per trade.

## 6. Cost budget — the number that decides the broker

The rule survives up to about **1.0 pip** of total round-trip cost and dies at
1.2. On this demo that cost is 0.3 pip spread and **zero commission**, which
leaves a 3× margin.

A raw-spread/ECN account typically charges ~0.1 pip spread **plus ~0.7 pip
commission** per round trip. That lands at 0.8 pip — inside the surviving band,
but with almost no room. On such an account this strategy is marginal. On a
spread-only (markup) account like this demo it is comfortable.

## 7. The live runner

`xau-scalp-live` (`crates/live/src/bin/scalp_live.rs`) is a **separate binary**
from the gold D3 runner on purpose. The D3 runner is live and validated, and its
dashboard, state file and `D3-<period>` comment tag are built around one rule on
one symbol. The scalper owns `SC-<period>` instead, so the two can never claim
each other's positions, and a bug here cannot touch the gold path.

What it does, once per closed M15 bar:

| step | behaviour |
| --- | --- |
| manage first | 8-bar time exit, then flat before the weekend gap |
| stop and target | both sent with the order, so **MT5 holds them** — a 3-pip stop does not depend on the poll |
| trailing | none; the tested rule has none |
| entry | market order on the signal from the last closed bar |
| window | entries only when the next bar opens 07:00–18:00 UTC |
| governor | refuses any trade whose stop risks more than `--max-risk-pct` (2%) of equity |
| margin | refuses when free margin cannot cover the position plus 3× the risk |
| kill switch | honours the dashboard's stop file; an open position still exits on time |

Two details that are easy to get wrong and are therefore explicit in the code:

* **The weekend close.** Gold has a daily break at 23:00 server, FX does not.
  Reusing the gold helper would have flattened every night, which the backtest
  never did. `is_week_last_bar` closes only the bar that ends on Saturday 00:00.
* **Which bar is closed.** The runner drops the last bar of every response
  unconditionally, because that is the one still forming. Deciding it from the
  clock instead let an early version read a bar whose close was still moving —
  look-ahead by another name. The same fix went into the gold runner.

### The governor is the part that makes $50 safe

It is one comparison — `risk / equity * 100 <= cap` — and it is unit-tested both
ways:

| account | instrument | stop risk | verdict |
| --- | --- | --- | --- |
| $50 | EURUSD 0.01 | $0.29 | allowed (0.6%) |
| $50 | gold 0.01 | $1.62 | **refused** (3.2%) |
| $5,000 | gold 0.01 | $1.62 | allowed (0.03%) |

So the same binary is safe on $50 and would correctly refuse to trade gold
there, without either being a special case.

### Running it

```bash
./scripts/app.sh start      # dashboard + gold H1 runner + EURUSD M15 scalper
./scripts/app.sh status
./scripts/app.sh logs
```

The dashboard has a **Scalper** panel reading the runner's own JSONL, so its
trades, win rate, last risk and any refusals are visible without touching the
terminal session.

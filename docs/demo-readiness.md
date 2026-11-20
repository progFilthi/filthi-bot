# Demo Readiness — full backtest verdict

**Date:** 2026-11-18
**Strategy:** D3 — Donchian 55 breakout, SL 1.5 ATR, trail 2.5 ATR, 240-bar exit, EMA300
**Size:** 0.10 lot | **Deposit:** $25,000 | **Spread:** 20 pts
**Evidence base:** `docs/strategy-long-history.md` (17-year validation)

---

## TL;DR

**Yes — the bot is ready for demo. It is not ready for live.**

The strategy has a real, measured edge over 17 years, and the execution
machinery has already completed three live round trips. What it has never done
is run **forward**, on data it has not seen — which is exactly what demo is for.

Two numbers explain what running this system actually feels like:

> **The median month made $11.** 105 of 203 months were positive, and 10 months
> produced 92% of all the profit. The bot spent **4.3 years underwater**
> between August 2020 and November 2024.

And one number changes how we should size it:

> Planned risk per trade ranged from **$20 to $1,619** at a fixed 0.10 lot,
> because the stop is 1.5 × ATR and gold's volatility rose roughly tenfold
> over the sample. **Fixed-lot sizing makes recent years carry far more risk
> than early ones.**

---

## 1. Headline result

| Metric | Gross | Net of swap |
| --- | --- | --- |
| Total PnL | +$26,647 | **+$24,883** |
| Return on $25,000 | +106.6% | **+99.5%** |
| Profit factor | 1.32 | **1.30** |
| Max drawdown | 9.90% ($4,951) | same |
| Sharpe | 0.73 | ≈ 0.70 |
| Trades | 1,911 | 1,911 |
| Win rate | 36.1% | 36.1% |
| **Break-even win rate** | **29.9%** | same |
| Expectancy | +$13.94/trade | +$13.02/trade |
| Average hold | 11.4 bars | 11.4 bars |
| Time in market | 22.0% of bars | 22.0% |
| Years profitable | 14 of 17 | 14 of 17 |

The gross figures are what the simulator produces. The net figures deduct an
estimated **$1,764 of overnight swap** (947 of 1,911 trades were held through a
rollover — 1,459 rollover nights in total, at the broker's published −12.6
pts/day long / −4.6 pts/day short, triple on Wednesday). Swap is the one cost
the engine does not model, so it is reported separately rather than quietly
mixed in.

**Break-even win rate 29.9% against an actual 36.1%** is the cleanest single
measure of the edge: the strategy can get 6 percentage points *worse* at
picking winners and still not lose money.

Fair expectation: **about $1,470 a year at 0.10 lot**, with long flat stretches.

## 2. Where the profit comes from

### By exit type

| Exit | Trades | Total | Average | Win rate |
| --- | --- | --- | --- | --- |
| Initial stop (wrong immediately) | 675 | **−$57,062** | −$84.54 | 0% |
| Trailed stop (right, gave some back) | 1,235 | **+$83,301** | +$67.45 | 55.8% |
| End of data (still open at the end) | 1 | +$409 | — | 100% |

The entire strategy is those two rows. It loses $57k on 675 trades that never
worked and makes $83k on 1,235 that did. There is no take-profit: every winner
is closed by the trailing stop, so **the exit logic, not the entry, decides the
result.**

### By holding time

| Bars held | Trades | Total | Win rate |
| --- | --- | --- | --- |
| 1-3 | 540 | **−$39,871** | 2.8% |
| 4-8 | 396 | −$17,607 | 16.7% |
| 9-20 | 646 | +$21,189 | 48.3% |
| 21-60 | 327 | **+$62,162** | 90.2% |
| 61-240 | 2 | +$775 | 100% |

If a breakout is still alive after 21 hours it is almost always a winner
(90.2% win rate). If it dies within 3 bars it is almost always a loss (2.8%).
That is healthy: **the strategy is wrong fast and right slowly.**

### By trade size

| Trade PnL | Trades | Total |
| --- | --- | --- |
| < −$200 | 68 | −$20,823 |
| −$200 … $0 | 1,153 | −$61,366 |
| $0 … $200 | 537 | +$36,052 |
| $200 … $400 | 92 | +$25,319 |
| $400+ | 61 | **+$47,466** |

61 trades — 3.2% of all trades — produced $47k, 44% of gross profit. This is a
fat-tailed system: it needs the occasional large winner to pay for everything
else.

### By direction

| Side | Trades | PnL | Per trade | PF |
| --- | --- | --- | --- | --- |
| Long | 1,077 | +$15,864 | $14.73 | 1.35 |
| Short | 835 | +$10,801 | $12.93 | 1.29 |

Both sides carry their weight. That was the failure that sank the M1 version,
so it matters that it is fixed here.

## 3. Where the losses come from

**1,221 losing trades, −$82,189 total.**

| Exit | Trades | Total | Average | Share of losses |
| --- | --- | --- | --- | --- |
| Initial stop | 675 | −$57,062 | −$84.54 | 69.4% |
| Trailed stop (gave profit back) | 546 | −$25,127 | −$46.02 | 30.6% |

70% of all losses are the intended stop doing its job. That is the cost of
doing business, not a malfunction — but in absolute terms: **the bot must lose
about $57k over 17 years to make $83k.**

| Metric | Value |
| --- | --- |
| Longest losing streak | **16 trades** (2023-07-18 → 2023-08-28, 41 days) |
| Average loss | −$67.31 |
| Worst month | −$1,134 |
| Worst single trade | −$925 (see §4 — it was *not* an anomaly) |

## 4. The risk-adjusted truth — the most important section

Dollars are misleading across this sample. Gold went from $1,094 to $3,774 and
ATR from roughly $5 to $60, so a **fixed 0.10 lot risks wildly different
amounts in different years:**

| Year | Trades | Mean planned risk | PnL | **R per trade** |
| --- | --- | --- | --- | --- |
| 2010 | 109 | $57.92 | +$178 | +0.028 |
| 2011 | 117 | $86.01 | +$2,205 | +0.219 |
| 2012 | 114 | $71.12 | −$578 | −0.071 |
| 2013 | 94 | $73.68 | +$2,761 | **+0.399** |
| 2014 | 107 | $50.71 | +$477 | +0.088 |
| 2015 | 110 | $47.69 | +$383 | +0.073 |
| 2016 | 105 | $56.97 | +$1,324 | +0.221 |
| 2017 | 113 | $38.09 | +$365 | +0.085 |
| 2018 | 116 | $36.81 | −$563 | −0.132 |
| 2019 | 116 | $46.97 | +$330 | +0.061 |
| 2020 | 112 | $94.98 | +$1,397 | +0.131 |
| 2021 | 111 | $71.85 | +$382 | +0.048 |
| 2022 | 126 | $78.19 | −$1,414 | −0.144 |
| 2023 | 119 | $70.79 | +$459 | +0.054 |
| 2024 | 122 | $96.01 | +$1,494 | +0.128 |
| 2025 | 120 | **$171.62** | +$7,475 | +0.363 |
| 2026* | 100 | **$353.69** | +$9,973 | +0.282 |

\* partial year.

`R` is PnL divided by the planned risk (1.5 × ATR at entry), so it measures the
edge independently of how volatile gold was that year.

**This changes the story for the better.** In dollars, 2025-26 produced 65% of
the profit. In R terms, **2013 (+0.399) was the best year in the sample**, and
2025 (+0.363) and 2026 (+0.282) are merely good — comparable to 2011 (+0.219)
and 2016 (+0.221). The recent dominance is largely a **position-sizing
artifact**, not the edge getting better.

Aggregate statistics across all 1,911 trades:

| Metric | Value |
| --- | --- |
| Mean R per trade | **+0.12** |
| Median R per trade | **−0.54** |
| Best trade | +17.57R |
| Worst trade | −1.12R |
| Planned risk range | $20.39 – $1,618.87 |

A median of **−0.54R** with a mean of **+0.12R** is the signature of a
trend-follower: the typical trade loses about half its risk, and a handful of
+10R to +17R trades pull the average positive. Every one of the five best
trades in 17 years was a short or long that ran for 12-56 bars.

### The gap-risk caveat

Zero trades lost more than −1.12R — **but that is a property of the simulator,
not the market.** The engine fills every stop exactly at the stop price, so it
physically cannot produce a gap-through-stop. The −$925 trade was not a gap; it
was a normal stop with a $1,619 planned risk.

Real gaps will exceed −1.12R. This is the single largest unmodelled risk and it
is why the demo checklist measures it directly.

## 5. The shape of the equity curve — the honest part

### Monthly distribution (203 months)

| Statistic | Value |
| --- | --- |
| Mean month | +$131 |
| **Median month** | **+$11** |
| Profitable months | 105 of 203 (51.7%) |
| 10th percentile | −$343 |
| 25th percentile | −$207 |
| 75th percentile | +$233 |
| 90th percentile | +$598 |
| Best month | +$4,867 (2026-01) |
| Worst month | −$1,134 (2026-05) |
| Best 10 months combined | +$24,459 (**92% of total profit**) |
| Worst 10 months combined | −$6,701 |

Roughly half of all months lose money, and the median month is eleven dollars.
Ten months out of 203 did essentially all the work.

In R terms it is far more even (§4), but **the dollar experience is what a
trader actually lives through** — and in dollars this is a lumpy, patient
system.

### Underwater periods

| Start of drawdown | Depth | Trough | Recovered | Time underwater |
| --- | --- | --- | --- | --- |
| 2020-08-19 | $3,701 (10.7%) | 2023-09-21 | 2024-11-22 | **1,555 days (4.3 yr)** |
| 2026-02-02 | $4,951 (9.9%) | 2026-02-20 | 2026-03-23 | 49 days |
| 2026-04-01 | $3,316 (6.4%) | 2026-07-29 | 2026-11-18 | 231 days |
| 2025-11-13 | $2,213 (5.1%) | 2026-01-06 | 2026-01-20 | 68 days |
| 2025-05-21 | $1,753 (4.5%) | 2025-08-28 | 2025-09-09 | 111 days |

**The bot made no new equity high for 4.3 years.** It kept trading (~500 trades)
and went nowhere, then 2025-26 paid for everything.

That is not a reason to reject the strategy. It is a reason to size it small
and to know what you are signing up for. On real money, most people would have
switched it off in 2022.

## 6. What the backtest cannot tell us

1. **Execution is not modelled.** Every fill is assumed at the bar open with a
   fixed 20-point spread. No slippage, no requotes, no news widening, no
   latency — and stops always fill at the stop price.
2. **Gap risk is unmeasured** (§4). Live losses can exceed 1.12R.
3. **Fixed lot distorts the dollar record.** 2026 carried ~10× the risk of 2018
   for the same lot size.
4. **Only one long dataset.** H1 is the only series with real depth. The M15
   check (2022-08 → 2026-11) is a different timeframe but a similar era, so it
   is corroboration, not independence.
5. **Swap is estimated**, ≈$1,764 over 17 years.
6. **Parameter selection used all the data.** The grid and the EMA300 filter
   were chosen by looking at the full sample. Walk-forward and the M15 check
   blunt this, but selection bias is never fully removed.

### Patterns that are probably noise — do not trade them

The breakdown shows Friday entries earning +$14,701 (55% of net profit) and
hours 04:00-05:00 and 23:00 UTC losing money. **Do not add a weekday or hour
filter based on this.** Those slices came from the same data that produced the
result; splitting 1,911 trades into 24 hourly buckets leaves fewer than 100
trades each, and the apparent effects are inside noise. The high-volatility
bucket "earning 86% of profit" is just the 2025-26 rally again.

Mining filters from the sample that produced the result is how a good strategy
gets over-fitted into a bad one.

## 7. Is it ready for demo? — Yes

**Demo is the correct next step, and the bot is ready for it.**

What demo can prove:

- **Mechanics.** The strategy moves its stop every bar. Confirm on real fills
  that the trailing stop sits where the model says, that the EA does not
  double-enter, and that restarts do not orphan positions.
- **Cost reality.** Actual H1 gold spread, actual slippage, and actual swap
  (model predicts ≈$1.2 per rollover night).
- **Gap risk.** The one thing the backtest cannot measure. Record every stop
  fill versus its intended price.
- **Loss profile.** Confirm losing trades land near −$67 to −$85 and that the
  win rate sits near 36%.
- **Operational reliability.** Bridge stability, reconnect behaviour, logging,
  watchdog.

What demo cannot prove:

- **That it makes money.** At ~9.4 trades/month, three months of demo is ~28
  trades. With a 36% win rate and most profit in rare months, 28 trades says
  almost nothing about profitability. **Demo validates the machine, not the
  edge.**

### The demo runner (added 2026-11-18)

The demo session is driven by `scripts/xau_live/runner.py`, a Python port of
the Rust engine that talks to the terminal over MCP. Nothing about the strategy
is re-decided in Python: `strategy.py` was verified numerically against
`crates/simulator/src/bin/probe.rs` (all 11 indicator values identical) before
the first order.

What it does each newly closed H1 bar:

1. reconcile with the broker (adopt an orphan, or clear state if the stop filled)
2. if in a position: time exit at 240 bars, else trail to 2.5 × ATR from the
   best price (the broker holds the stop, so intrabar fills behave like the
   engine)
3. if flat: enter at market on a Donchian-55 breakout that agrees with EMA300,
   stop at 1.5 × ATR, **no take profit**, no opposite-signal exit
4. persist state to `runs/live/d3_state.json`, append to `runs/live/d3_runner.log`

Safety properties: one position at a time, refuses to run on a non-demo
account, halts at −25% equity, and never sends an order without a stop loss.

```bash
cd scripts/xau_live
python3 preflight.py            # read-only gate; must print PREFLIGHT OK
python3 runner.py --once --dry-run
python3 runner.py               # leave this running
```

**MCP trading permission.** For most of 2026-11-18 every `trade_*` call was
rejected with `trading is not permitted`, while reads and even
`trade_order_calc_*` worked. It was not the MT5 AutoTrading button, not the
account, and not the broker: it was the per-server trade permission in
MT5's MCP/Assistant settings. Once that was granted, the same calls returned
`retcode 0 / 10009 Done` on unchanged code. Note the endpoint/token split —
`127.0.0.1:22346` (terminal) and `127.0.0.1:22345` (MetaEditor) are different
servers; the token that works on 22346 is the one in `scripts/mcp.sh`.

**AutoTrading still matters.** After a terminal restart the toolbar AutoTrading
button comes back OFF and `experts_trade_allowed` reads `false`. Order sends
need it on; it cannot be set through the API, so a terminal restart needs a
human to press Ctrl+E. The runner logs the failure and retries rather than
pretending it traded.

Execution verified on the demo account at 0.01 lot before the loop was left
running: entry `10009`, stop landed exactly at 1.5 × ATR with TP `None`, a
trailing `trade_modify_sl_tp` moved the stop `3745.92 → 3746.07` (`10009`), and
close `10009`. Net cost of the three round trips: a few cents.

### The Rust runner — `xau-live` (added 2026-11-18)

The Python runner above is a *re-implementation* of the engine. It was
cross-checked on one bar (11 identical values), but a port can drift. `xau-live`
removes that risk by calling `xau_sim::strategy` directly: the code that places
orders is the code that produced the 17-year numbers.

```bash
cargo run -p xau-live -- --scan                     # read-only market read, all timeframes
cargo run -p xau-live -- --period H4 --lots 0.1 --once --dry-run
cargo run -p xau-live -- --period H4 --lots 0.1     # leave this running
```

`--scan` prints one line per timeframe — close, 55-bar channel, EMA300, ATR,
and either the live signal with the stop it would place, or the distance price
still needs to break out. It is the fastest way to answer "is there a trade
here?" without touching an order.

Design decisions worth knowing:

- **One clock, no DST maths.** Bar labels arrive in the broker's clock and
  `trade_server_last_known_time` is in the same clock, so "has this bar closed?"
  compares two values from the same source. No UTC conversion, so no
  spring/autumn hour bug.
- **Adoption starts the clock at handover.** If the process restarts while a
  position is open it adopts it, but the boundary is written to state and bars
  that closed *before* adoption are excluded from the trail's best price and
  from the 240-bar time exit. `best` re-seeds at the entry price, exactly as
  `engine::backtest` seeds it.
- **Ownership tags.** Orders are commented `D3-<PERIOD>`, matched exactly. A
  runner ignores positions tagged for another timeframe — trailing an H1 trade
  with H4 ATR would put the stop in the wrong place — and ignores untagged ones
  entirely. It only ever modifies its own ticket. `--adopt-any` overrides.
- **Order payloads are logged** before they are sent, so every live decision has
  an audit trail in `runs/live/rust_<period>_runner.log`.

Handover on 2026-11-18: the Python manager was stopped mid-position and
`xau-live` adopted `59394653039` (`D3-H4`, 0.10 lot, stop 3814.09) without
sending or modifying anything. Verified by diffing the broker position before
and after.

### Operational notes (2026-11-20)

Learned the hard way over the first unattended weekend:

- **The MCP token rotates on every terminal restart.** Symptom: `HTTP 401`
  on *everything*, reads included, and it looks like the account broke. The
  token now lives in `runs/live/mcp_token` (gitignored, `chmod 600`); both
  `xau-live` and `client.py` read it, and `MT5_MCP_TOKEN` overrides it. Never
  hard-code it again — it was briefly committed, and old tokens are in history.
- **A terminal restart kills the runners.** They are ordinary processes, not
  services. Nothing re-starts them, and the broker-side stop is the only thing
  still working in the gap.
- **Ownership is the `D3-<PERIOD>` comment, and only exact matches count.**
  A position without that tag was opened by a human or another system: it is
  never adopted, trailed or closed, just logged once per change. The first
  version refused to run at all while an untagged position existed, which made
  the bot useless exactly when it was needed.
- **Only our own ticket is ever modified or closed.** Until 2026-11-20 the
  trail and the time exit acted on `positions[0]`, i.e. whatever happened to be
  first on the account. With a manual trade open that would have moved or closed
  someone else's position.
- **`M1` has to be a valid period.** Entries read the live price from M1; while
  M1 was missing from the period table every entry died with
  `unsupported period M1` at the instant the signal fired. Caught by
  dry-running a live signal before letting it trade.

**Incident, 2026-11-18.** The handover trade `59394653039` (`D3-H4`) was closed
from the MT5 mobile app at 13:46 server time for **+$10.10**, 14 minutes after
`xau-live` adopted it. One minute later a naked 0.1 short `59395314229` was
opened from mobile: no stop, no target, no comment. Both runners were already
dead from a server restart, so nothing was watching. Gold then fell ~$184 and
the untagged short carried ~$1,866 of unrealised profit with no protection. It
was given the strategy's trail stop (`best low 3514.45 + 2.5 × ATR 41.38 =
3617.90`), locking ~$1,428. This is the honest lesson: a bot that manages one
track does not protect the account from a human opening a second one.

### Demo acceptance checklist

Run for **at least 8-12 weeks** (~75-110 trades) and require:

| Check | Pass condition |
| --- | --- |
| Trades executed | ≥ 60 |
| Order failures / rejects | 0 unhandled |
| Double entries or orphaned positions | 0 |
| Average slippage vs model | ≤ 5 pts per side |
| Observed spread | median ≤ 25 pts |
| Swap charged vs estimate | within ±30% |
| **Stop slippage (gap-through)** | **record every case; a few are normal** |
| Max drawdown | ≤ 12% |
| Equity curve | need not be positive, but no unexplained loss |

**If any mechanical check fails, stop and fix it before drawing any conclusion
about the strategy.** A negative demo PnL over 75 trades is not, by itself,
evidence against the strategy. A bot that fills wrongly is.

## 8. The gate to live

Live should follow demo only when all of these are true:

1. The full demo checklist passes.
2. The demo loss distribution matches the backtest — average loss near its
   planned risk, win rate near 36%. If real losses are systematically larger,
   execution is eating the edge.
3. **Unattended operation without a human pressing Ctrl+E.** MCP order sends
   need the terminal's AutoTrading button, which resets to OFF on every
   terminal restart and cannot be set through the API (the EA path fails with
   retcode 10027 for the same reason). Until that is automated — or the
   terminal is left running untouched — every restart needs a human.
4. **Position sizing is volatility-normalised**, not a fixed lot. See §9.
5. A kill-switch exists: a maximum drawdown at which the bot stops and a human
   decides (suggested: 12%).

Start live at **0.01 lot** — a tenth of the tested size — for 1-2 months before
considering 0.10.

## 9. Sizing — fix this before live

The backtest used a fixed 0.10 lot, which means the risk per trade grew with
volatility:

| Period | Mean planned risk at 0.10 lot | As % of $25,000 |
| --- | --- | --- |
| 2017-2018 (calm) | $37 | 0.15% |
| 2024 | $96 | 0.38% |
| 2025 | $172 | 0.69% |
| 2026 | $354 | **1.41%** |

Trading 0.10 lot *today* is roughly **ten times** the risk of the "0.10 lot"
that produced the 2017 results. For live, size from risk instead:

```
lots = (0.5% of equity) / (1.5 x ATR x 100)
```

At $25,000 and 0.5% risk ($125), with ATR $60 that gives ≈0.014 lot; with ATR
$5 it gives ≈0.17 lot. This makes every trade risk the same fraction of the
account and would substantially flatten the dollar equity curve shown in §5.

**For demo, fixed 0.01-0.10 lot is fine** — demo is about mechanics, not
returns. Volatility-normalised sizing becomes mandatory before live.

## 10. Recommended configuration

Already written to `configs/phase0.toml`:

```toml
[strategy]
name = "donchian"
lookback = 55
trend_ma = 300
lots = 0.10
atr_period = 14
sl_atr = 1.5
tp_atr = 0.0
trail_atr = 2.5
max_hold_bars = 240
allowed_hours_utc = []
spread_pts = 20
paper_only = true
```

`paper_only = true` stays until the demo checklist above is satisfied.

## 11. Reproducing this report

```bash
cargo run -p xau-sim --bin xau-report     -- --parquet data/cache_long/xauusd_h1.parquet
cargo run -p xau-sim --bin xau-validate   -- --parquet data/cache_long/xauusd_h1.parquet --folds 6
cargo run -p xau-sim --bin xau-walkforward -- --parquet data/cache_long/xauusd_h1.parquet
cargo run -p xau-sim --bin xau-breakdown  -- --parquet data/cache_long/xauusd_h1.parquet
cargo run -p xau-sim --bin xau-tpsweep    -- --parquet data/cache_long/xauusd_h1.parquet
cargo run -p xau-sim --bin xau-resample   -- --in data/cache_long/xauusd_h1.parquet \
                                            --out data/cache_long/xauusd_h4.parquet --secs 14400
cargo run -p xau-sim --bin xau-tpsweep    -- --parquet data/cache_long/xauusd_h4.parquet
```

Outputs behind this document are in `runs/backtest/` (`report-h1.txt`,
`validate-h1.txt`, `walkforward-h1.txt`, `analyze-h1.txt`, `breakdown-h1.txt`,
plus the `*-m15.txt` equivalents). `runs/` is gitignored — rerun the commands
above to regenerate.

The demo runner and its logs live in `scripts/xau_live/` and `runs/live/`; see
§7 "The demo runner".

## 12. Two questions answered with data

### 12.1 Does a take profit help?

`xau-tpsweep` re-runs the whole engine with `tp_atr` set to a fixed multiple.
Same data, same stop, same trail — only the target changes.

**H1, 17 years (base: +$26,647, PF 1.32, 9.90% DD):**

| TP (ATR) | PnL | PF | Max DD | Trades | TP exits |
| --- | --- | --- | --- | --- | --- |
| **none** | **+$26,647** | **1.32** | **9.90%** | 1,911 | 0 |
| 1.5 | +$8,250 | 1.06 | 25.48% | 3,253 | 1,590 |
| 2.0 | +$12,297 | 1.09 | 16.71% | 2,956 | 1,209 |
| 3.0 | +$17,978 | 1.16 | 14.99% | 2,537 | 702 |
| 4.0 | +$22,206 | 1.21 | 15.60% | 2,333 | 454 |
| 6.0 | +$24,717 | 1.26 | 10.20% | 2,103 | 209 |
| 8.0 | +$26,443 | 1.29 | 10.20% | 2,009 | 103 |
| 10.0 | +$26,337 | 1.31 | 10.06% | 1,961 | 55 |

Every take profit loses money on H1, and the tighter it is the worse it gets —
1.5 ATR cuts PnL by 69% and *triples* the drawdown relative to no target. On H4
the result is a wash at best (best TP, 4 ATR: +$28,464 vs +$27,907, inside
noise). **No take profit is a measured decision, not an omission.** The trade
ends on the trail, the original stop, or the 240-bar clock.

### 12.2 Does the strategy work on H4?

Asked because a live H4 signal appeared on 2026-11-18 and an untested timeframe
should not carry size. H1 history was resampled to H4 (`xau-resample`, 26,832
bars) and D3 re-run unchanged.

| H4, 16 years | Result |
| --- | --- |
| Total PnL @ 0.10 lot | +$27,907 (+111.6%) |
| Profit factor | 1.81 |
| Max drawdown | 8.10% |
| Trades | 531 (41.4% win) |
| Avg win / avg loss | $282.59 / $110.17 |
| Expectancy | $52.56/trade |
| Years profitable | 15 of 17 (2014 −$669, 2017 −$82) |
| Out-of-sample (last 30%) | PF 2.13 |
| Exits | trail 382, stop 148, time 1 |

Two caveats that matter: an H4 bar is an aggregate, so the order in which the
stop, the trail and the close occurred inside it is unknown — H4 results are
therefore *less* trustworthy than H1's, not more. And this is one timeframe on
one instrument, not independent evidence of a new edge.

**Live consequence:** the 2026-11-18 sell was taken on H4 at 0.10 lot ($504.90
risk, 2.02% of equity) and is managed by `xau-live --period H4`. It is a
*separate track* from the H1 demo record and is labelled as such (`D3-H4`), so
it cannot be mistaken for evidence about the headline H1 strategy.

## 13. Bottom line

| Question | Answer |
| --- | --- |
| Does the strategy have a measured edge? | **Yes** — 17 years, 14/17 positive, mean +0.12R/trade, both sides, survives 3× spread |
| Is the edge concentrated in 2025-26? | **Less than it looks** — in R terms 2013 was the best year |
| Is the bot mechanically ready? | **Yes** — MCP entry/SL/modify/close all verified on the demo account, 38 tests green |
| Is it ready for demo? | **Yes — start it** |
| Is it ready for live? | **Not yet** — demo checklist, gap data, volatility sizing, Algo Trading fix |
| What should we expect? | ~$1,470/yr at fixed 0.10 lot; lumpy, with multi-year flat periods |
| What would make us wrong? | Real fills losing more than planned risk, or the regime never repeating |

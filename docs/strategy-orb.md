# London / New York opening-range breakout

**Verdict: the rule as described does not survive out of sample. One filtered
version is modestly positive in both halves but is not statistically
significant, so it is a paper-trade candidate, not a funded-account plan.**
This page records what was built, what the data said, and what would have to
be true to change the verdict.

## 1. What was tested

The transcript's rules, mechanized:

| Rule (spoken) | Mechanization |
|---|---|
| Box = London high/low from London open to New York open | window in **local London time**, DST-aware (UTC winter / UTC+1 summer) |
| Wait for the **first** break after the New York open | first bar after the box that trades through either edge; only the first side counts, one trade per UTC day |
| Enter as soon as it breaks (no close confirmation) | fill at the box edge + spread + 2 pts slippage; `close` confirmation is tested as a variant |
| Stop beyond the "most recent major swing" | min low / max high of the N minutes before the entry bar (5–90 min tested), or the box edge |
| Target 2R | fixed 2× the stop distance; 1R–3R and "no target" tested |
| Close around 2 pm ET | hard flat at 19:00 London (14:00 ET), so nothing is held overnight |
| Risk 0.5–0.9% | fixed-fractional sizing on the running balance, 0.01-lot floor |
| One trade a day, max two | one |

Cost model is the pessimistic one used across this repo: the OHLC series is
the **bid**; a buy pays the spread (lifting the ask); a sell receives the bid.
Stops pay extra slippage; targets get none. In a bar that touches both stop and
target, the stop fills first. On the trigger bar only a **close** through the
stop counts, so the wick before the break cannot fake a stop-out.

Code: `crates/simulator/src/orb.rs`, study binary `crates/simulator/src/bin/orb.rs`.

## 2. Data

| Series | Range | Bars | Use |
|---|---|---|---|
| `data/cache_long/xauusd_m5.parquet` | 2025-06-13 → 2026-11-18 | 99,472 | main test, 1.43 years, 448 sessions |
| `data/cache/xauusd_m1.parquet` | 2026-08-03 → 2026-11-17 | 100,000 | pilot, the timeframe the transcript actually trades |

The terminal caps history at ~100k bars per timeframe, so 1.4 years is the
longest M5 series available and M1 can only give ~3.5 months. M15 goes back
4.3 years but a 15-minute bar cannot resolve a "break" the way the strategy
needs, so it was not used.

## 3. What the data said

### 3.1 The rule as described: no reliable edge

M5, box 09:00–14:00 London (his stated 4 am–9 am ET), 45-min swing stop, 2R:

| Period | n | win | PF | expR | maxDD |
|---|---|---|---|---|---|
| 2025-06 → 2026-02 (train) | 172 | 48.3% | 1.33 | +0.129 | 6.8% |
| 2026-03 → 2026-11 (**test**) | 166 | 42.2% | 0.77 | **−0.099** | 17.7% |

The second half is the honest test, and it loses money with a drawdown that
would fail any challenge. The window was not the problem — every exit variant
failed there:

| Exit | train expR | test expR |
|---|---|---|
| 2R target | +0.129 | −0.099 |
| 3R target | +0.218 | −0.111 |
| no target (ride to the cut-off) | +0.183 | −0.129 |

So the *signal* — "first break of the London box after 09:00" — carries no
out-of-sample information on this data, whatever the exit.

### 3.2 The one filter that held up: only the first hour

Restricting entries to the **first 60 minutes after the box** (a classic
"opening drive" rule) changes the picture:

| Period | n | win | PF | expR | t | maxDD |
|---|---|---|---|---|---|---|
| full 2025-06 → 2026-11 | 239 | 44.8% | 1.33 | +0.213 | 2.41 | 5.9% |
| train (first half) | 116 | 47.4% | 1.54 | +0.301 | — | 5.9% |
| **test (second half)** | 123 | 42.3% | 1.22 | **+0.131** | **1.08** | 4.5% |

Box 08:00–13:00 London, break 13:00–14:00, 45-min swing stop, 2R, flat 19:00.

The same rules on **M1** (an independent, and weaker, period — Aug–Nov 2026):

| n | win | PF | expR | t | maxDD | net |
|---|---|---|---|---|---|---|
| 45 | 46.7% | 1.52 | +0.290 | 1.39 | 2.9% | +7.4% |

Cost stress on the M5 config (spread + slippage multiplied):

| spread | 15 | 30 | 45 | 60 |
|---|---|---|---|---|
| expR | +0.213 | +0.197 | +0.188 | +0.179 |

Monthly R (M5, filtered) — 10 of 18 months positive, worst month −0.44R:

```
2025-07 +11.8   2025-12 +2.6    2026-05 -2.1    2026-10 +3.9
2025-08  +4.4   2026-01 -6.1    2026-06 -0.4    2026-11 -1.4
2025-09  +5.6   2026-02 +7.2    2026-07 +9.0
2025-10  +7.6   2026-03 +1.8    2026-08 -3.3
2025-11  +1.0   2026-04 +3.0    2026-09 +5.6
```

### 3.3 Why this is still not a green light

- **The t-statistic out of sample is 1.08.** That is indistinguishable from
  luck. The full-sample t of 2.41 is meaningless because the config was chosen
  after looking at the data.
- **The window is knife-edge.** With the 60-minute filter, changing the box end
  from 13:00 to 13:30 London drops test expectancy from +0.131R to +0.025R;
  moving it to 14:00 makes it −0.114R. A real effect usually degrades smoothly
  across neighbouring parameters.
- **Roughly 60 configurations were examined.** At n≈120 per half, a per-trade
  standard deviation of ~0.9R gives a standard error of ~0.08R. Picking the
  best of 60 is more than enough to manufacture +0.13R from noise.
- **One trade a day means little room to recover.** A bad month is 15–20
  trades; the sample is effectively much smaller than the trade count suggests.

## 4. Prop-firm arithmetic — why this matters more than the edge

A challenge needs +8–10% with a 6% total-drawdown limit. That is a
**return-to-drawdown** problem, and the two strategies are far apart:

| | D3 (H1, 16 years) | ORB filtered (1.43 years) |
|---|---|---|
| total return | +106.6% | +29.6% |
| max drawdown | 9.9% | 5.9% |
| return per year per unit of DD | ~0.7 | ~3.5 |
| trades | ~120/year | ~170/year |
| scaled to a 6% DD limit | ~2.9%/year → 10% target takes ~3.5 years | ~30%/year → target in ~2 months |

Even if D3's edge is real, it is the wrong instrument for a challenge: it is
built to compound over a decade, not to survive a 6% line. That is the actual
argument for an intraday strategy, independent of whether this particular one
is the right one.

## 5. Decision

1. **Do not fund a challenge on this.** The evidence is a maybe, not a yes.
2. **Do not replace D3.** D3 remains the reference strategy and the only
   thing here with a decade of out-of-sample behaviour.
3. **Paper-trade the filtered version on demo** for 2–3 months, with the exact
   rules above. The missing evidence is forward data, and demo costs nothing.
   Re-run this study when it has 40–60 forward trades. The logger that does
   this is described in §6 and is running now.
4. If it fails forward, the next question is not "tune the box", it is "is
   there any intraday signal on gold that survives 2× costs", and that needs a
   longer M1 history than the terminal can provide.

## 6. Paper-trading logger

Since the backtest cannot settle the question, the filtered rule runs on demo
as a **signal logger**: `xau-orb-paper` (in `crates/live`). It evaluates the
same `xau_sim::orb` code the backtest uses against closed M5 bars from the
terminal and writes two files. It cannot place an order — there is no order
code in the binary.

```
runs/orb/journal.jsonl   one line per signal, one per result
runs/orb/board.json      latest session + forward record (read by the dashboard)
```

Start it (token resolved the same way as the D3 runner):

```bash
cargo run --release -p xau-live --bin xau-orb-paper -- --poll 60
# or, built:
./target/release/xau-orb-paper --poll 60
```

It logs the configuration that survived both halves — box 08:00–13:00
London, entries for 60 minutes, 45-minute swing stop, 2R, flat 19:00 — and
nothing else, so there is no tuning after the fact.

Two details that matter:

- **Time conversion is per bar.** The broker clock is EET/EEST (UTC+2, +1h
  in summer), so each bar is converted to UTC using the same DST dates. The
  study window is knife-edge on an hour, so a fixed shift would silently turn
  this into the configuration that tested at −0.21R.
- **Live vs backfilled.** A line is marked `"live": true` only when it was
  written within 30 minutes of the event; anything reconstructed after a
  restart is `"live": false`. The board counts them separately, because only
  live trades are forward evidence.

The dashboard shows it under **Paper log — London breakout** (`/api/orb`).
It displays the session phase, box, plan, result and the forward record with
paper equity at 0.7% risk on $5,000. Nothing on that card can send an order.

**Validation done before trusting the log:** the logger and the backtest were
run over the same six sessions (2026-11-11 → 2026-11-20) and produced
identical results — 6 trades, −$118.39, −0.517R, 2.67% max drawdown. The two
paths differ completely (live MCP bars vs imported parquet), so this checks
the plumbing rather than the rule.

The forward record needs ~40 live trades before it is worth reading. Until
then every number on that card is noise.

## 7. Reproduce

```bash
# main study (1.4 years of M5)
cargo run --release -p xau-sim --bin xau-orb -- \
    --parquet data/cache_long/xauusd_m5.parquet \
    --box-start 8 --box-end 13 --swing-min 45 --max-wait 60 --mode detail

# train / test halves
cargo run --release -p xau-sim --bin xau-orb -- \
    --parquet data/cache_long/xauusd_m5.parquet \
    --box-start 8 --box-end 13 --swing-min 45 --max-wait 60 \
    --from 2026-03-01 --to 2026-11-19 --mode detail

# M1 pilot
cargo run --release -p xau-sim --bin xau-orb -- \
    --parquet data/cache/xauusd_m1.parquet \
    --box-start 8 --box-end 13 --swing-min 45 --max-wait 60 --mode detail

# parameter sensitivity, cost stress
... --mode sweep
... --mode costs
```

Flags: `--box-start` / `--box-end` / `--cutoff` in **local hours**,
`--tz eu|us|none`, `--swing-min`, `--swing N` (bars), `--stop swing|box`,
`--confirm break|close`, `--rr`, `--max-wait`, `--ema N`, `--spread`,
`--entry-slip`, `--stop-slip`, `--risk`, `--deposit`, `--from`, `--to`.

The M5 series was fetched with `scripts/fetch_mcp_history.py --period M5` and
imported with `xau data import --config configs/symbols/xauusd_utc.toml`.

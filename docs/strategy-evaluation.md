# Strategy Evaluation — what actually works on XAUUSD

**Date:** 2026-11-18
**Data:** 100,000 M1 bars, 2026-08-03 → 2026-11-17 (~105 days, the full sample we have)
**Lots:** 0.10 | **Deposit:** $25,000 | **Spread:** 20 pts

---

## TL;DR

Five strategy families were built, tuned in-sample, and validated out-of-sample.

**Result: no strategy produced a demonstrated, transferable edge.**

One configuration looked profitable on the M1 sample (+8.3%, profit factor 1.16),
but when the same logic was run on M5, M15 and H1 it lost money, and almost all
of its profit came from the short side during a four-month downtrend. That is a
directional bet on one market regime, not an edge.

The honest state of the bot: **the execution machinery is solid; the signal
generator is not yet worth real money.**

---

## 1. What the data is made of

Diagnostics from `cargo run -p xau-sim --bin xau-analyze`.

| Property | Value | Meaning |
| --- | --- | --- |
| Span | 105 days, 100,000 M1 bars | 4 months, one regime |
| Price path | $4,036 → peak $4,695 (bar 20k) → $3,864 | 20% rise, then 80% decline |
| Bar range | mean $2.01, median $1.66, p95 $4.39 | volatile instrument |
| ATR(1m) | $2.02 | average move per minute |
| Spread | mean 18.4 pts, median 17, p95 33 | ~8.4% of ATR |
| Kurtosis | 62.8 | fat tails, real-market-like |
| Skew | −0.95 | crash risk, real-market-like |
| Up / down bars | 49.5% / 50.1% | symmetric |

Session structure is textbook real gold data: volume and range peak at
13:00–16:00 UTC (US session), spread is tightest then (~15 pts) and widest at
23:00–00:00 UTC rollover (~32 pts). The dataset is authentic.

### The decisive statistic: variance ratio

`VR(q) > 1` means momentum, `VR(q) < 1` means mean reversion, `VR(q) ≈ 1` means
a random walk.

| q (bars) | VR | Reading |
| --- | --- | --- |
| 2 | 0.997 | random |
| 5 | 0.985 | random |
| 10 | 0.999 | random |
| 30 | 0.950 | random |
| 60 | 0.942 | random |
| 120 | 0.936 | random |

Return autocorrelations at lags 1–10 are all between −0.023 and +0.010.

**The series behaves very close to a random walk.** That is expected of a liquid
market and it is the core reason no strategy worked. In a random walk, the
expected profit of any trading rule before costs is zero, and after costs it is
negative.

---

## 2. Strategies built

Implemented in `crates/simulator/src/strategy.rs`, executed by the no-look-ahead
engine in `crates/simulator/src/engine.rs`.

| Family | Logic | Why it was a candidate |
| --- | --- | --- |
| `ma-cross` | Fast MA crosses slow MA | classic trend following |
| `donchian` | Close breaks N-bar high/low, optional EMA trend filter | turtle-style breakout |
| `bollinger` | Fade 3-sigma band extremes | mean reversion |
| `pullback` | Trend EMA + momentum re-entry | trend with cheaper entries |
| `rsi` | RSI extremes aligned with trend | classic reversion |

Engine features: ATR-scaled stops and targets, trailing stops, time exits,
session-hour filters, pessimistic fill rules (stop assumed to fill before target
when both are touched in one bar) and spread charged per round trip.

---

## 3. Walk-forward test (42 configurations)

In-sample = first 70%, out-of-sample = last 30%. Parameters selected on
in-sample only.

```
Configurations tested:            40
Profitable in-sample:              4  (10%)
Profitable out-of-sample:         12  (30%)
IS/OOS profit correlation:    +0.49
```

The single best in-sample configuration was the 20/50 MA cross with no session
filter: **+$1,666 in-sample, −$900 out-of-sample.** Textbook curve-fitting.

Only two configurations were profitable in *both* halves:

| Config | In-sample | Out-of-sample |
| --- | --- | --- |
| MA 20/50, 13–16 UTC only | +$899 | +$786 |
| Bollinger k=3, 3-ATR stop, 120-bar exit | +$346 | +$1,191 |

---

## 4. Robustness of those two

### Per-fold PnL (5 sequential slices)

| Strategy | f1 | f2 | f3 | f4 | f5 | Total | Positive |
| --- | --- | --- | --- | --- | --- | --- | --- |
| MA 20/50, US session | −536 | +1,103 | −63 | +813 | +367 | **+1,685** | 3/5 |
| Bollinger k3 | −538 | +351 | +553 | +182 | +990 | **+1,538** | 4/5 |
| Donchian 55 + trail | −472 | −2,422 | −537 | −2,072 | +1,502 | −4,001 | 1/5 |

### The killer test: which side makes the money?

| Strategy | Long side | Short side |
| --- | --- | --- |
| MA 20/50, US session | 224 trades, **−$366** | 207 trades, **+$2,447** |
| Bollinger k3 | 251 trades, **−$21** | 237 trades, **+$1,558** |

Every dollar of profit came from shorts. In a sample where gold fell 4.3% from
start to finish and spent 80% of its bars declining, "sell during the US session"
is a trend bet, not a strategy.

### Rising leg vs falling leg

The data peaks at $4,695 on bar 20,101 and then falls for the remaining 80%.

| Strategy | Rising leg | Falling leg |
| --- | --- | --- |
| MA 20/50 | **−$313** | **+$2,394** |
| Bollinger k3 | **−$614** | **+$2,151** |

Both lose money when gold rises and make it when gold falls. That is the whole
"edge".

### Cross-timeframe test

Same parameters, same date range, different bar size:

| Strategy | M1 | M5 | M15 | H1 |
| --- | --- | --- | --- | --- |
| MA 20/50, US session | **+$2,081** | −$2,957 | −$1,387 | −$239 |
| Bollinger k3 | +$1,537 | −$2,225 | **+$8,360** | −$9 |

A real edge does not vanish when you resample the same market. The MA result is
M1-only, which points to minute-bar noise rather than a market behaviour.

The M15 Bollinger figure of +$8,360 looks spectacular until you look at
concentration: **85% of the out-of-sample profit came from the five best
trades**, and the single best trade was 25% of gross profit. That is luck, not
edge.

---

## 5. Cost stress

The MA candidate survives a worse broker, which is its one genuinely good
property:

| Spread | 15 pts | 20 | 25 | 30 | 40 | 50 |
| --- | --- | --- | --- | --- | --- | --- |
| MA 20/50 PnL | $2,297 | $2,081 | $1,866 | $1,650 | $1,219 | $788 |

It is not cost-fragile. It is regime-fragile, which is worse, because you cannot
see it coming.

---

## 6. Verdict

| Question | Answer |
| --- | --- |
| Is the backtest engine trustworthy? | Yes. No look-ahead, pessimistic fills, costs modelled, 11 tests. |
| Did any strategy survive out-of-sample? | Two, but only in this sample. |
| Is the result explainable by something other than edge? | Yes — a short-side bet on a falling market. |
| Does it transfer to other timeframes? | No. |
| Is it concentrated in a few lucky trades? | Yes on M15; moderately on M1. |
| Should this trade real money? | **No.** |

**Nothing here justifies risking capital.** The most likely outcome of trading
the MA configuration live is roughly what the rising leg did: a slow bleed
punctuated by a good month, with the good months being luck.

---

## 7. What would actually be needed

The blocker is data, not code. 105 days of one regime cannot distinguish skill
from luck, and this sample is dominated by a single downtrend.

1. **Get 5–10 years of history**, ideally including 2020 (crash), 2021–22
   (inflation trend), 2023–24 (range). Only then can regime dependence be
   measured rather than guessed.
2. **Test on multiple instruments** (EURUSD, GBPUSD, indices, oil). An edge that
   only exists on one instrument over one quarter is not an edge.
3. **Only then** tune parameters, and always hold out a final untouched slice.
4. **Paper trade** the chosen configuration forward for 100+ trades and compare
   the live win rate and average win/loss to the backtest. If they diverge, the
   backtest was wrong.
5. **Accept that most retail strategies have no edge.** That is the base rate.
   The professional response is to keep the infrastructure, keep searching with
   better data, and not deploy capital on hope.

---

## 8. Tooling left behind

| Command | What it does |
| --- | --- |
| `cargo run -p xau-sim --bin xau-analyze` | Random-walk / regime statistics for any bar file |
| `cargo run -p xau-sim --bin xau-walkforward` | In-sample optimisation + out-of-sample validation |
| `cargo run -p xau-sim --bin xau-validate` | Multi-fold robustness, parameter and cost sensitivity |
| `cargo run -p xau-sim --bin xau-report` | Full report: monthly, concentration, regime split |
| `cargo run -p xau-sim --bin xau-backtest` | Single-config historical backtest |

All accept `--parquet <path>`. All run offline; MT5 is not required.

The strategy configuration lives in `configs/phase0.toml` under `[strategy]`
with `paper_only = true`. It should stay `true` until the requirements in
section 7 are met.

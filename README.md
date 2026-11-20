# XAUUSD Algo Trading System (Rust + MT5)

A Rust trading engine for gold (XAUUSD) with an event-driven backtester, a risk layer, and a thin MQL5 Expert Advisor that bridges to MetaTrader 5 (running under Wine on macOS). Demo account first. Live only after the acceptance gates in section 10 are met.

## 0. Instructions to the AI agent (read first)

1. Build in the phases of section 11, in order. Do not skip ahead. Each phase ends with passing tests and a short report.
2. **Do not invent strategies and declare them profitable.** Implement the strategies specified in section 6, run them through the validation pipeline in section 7, and report results honestly, including failures.
3. **No look-ahead bias, ever.** A strategy may only use data available at the close of the bar it is deciding on. Orders fill at the next bar's open (or the next tick) plus costs. Add tests that enforce this.
4. **All costs are modeled** in every backtest: spread (real, time-varying), slippage, commission, swap.
5. **The same strategy code runs in backtest and live.** Only the `DataSource` and `Executor` implementations are swapped.
6. Never tune parameters on the out-of-sample set (section 7). Report every parameter combination tried so the number of trials is known.
7. Risk limits (section 8) are hard-coded gates that strategies cannot bypass.
8. Default to **demo mode**. Live mode requires an explicit config flag and a typed confirmation at startup.
9. Prefer small, well-tested modules. Use `cargo clippy -- -D warnings` and `cargo fmt`. Ask me when a requirement is ambiguous.

## 1. Goals and non-goals

**Goals**
- Positive expectancy after costs, with controlled drawdown.
- Fast research loop: millions of parameter/walk-forward runs via `rayon`.
- Reproducible results: fixed seeds, versioned configs, logged runs.
- Safe execution: idempotent orders, reconnect handling, kill switch.

**Non-goals**
- Maximizing win rate. Optimize expectancy, profit factor, max drawdown, Sharpe/Sortino. A high win rate with rare large losses is a red flag.
- Predicting price with end-to-end ML (out of scope until section 12).
- HFT. Targeting M1/M5 and above; latency is dominated by the network and broker, not code.

## 2. Architecture

```
┌──────────────────────────┐        TCP 127.0.0.1:5555        ┌─────────────────────────────┐
│ MT5 (Wine, macOS)        │  <-- NDJSON (bars, ticks, acks) --│ Rust engine (native macOS)  │
│  MQL5 EA "Bridge.mq5"    │  --- NDJSON (orders, modify) --> │  - DataSource (live/hist)   │
│  - streams ticks/bars    │                                   │  - Strategy trait           │
│  - executes orders       │                                   │  - Risk manager             │
│  - reports fills/account │                                   │  - Executor (live/sim)      │
└──────────────────────────┘                                   │  - Backtester, optimizer    │
                                                               │  - Logging, metrics         │
                                                               └─────────────────────────────┘
```

The EA is the only non-Rust piece and stays dumb: it contains no strategy logic. The EA acts as the TCP **client** and connects to the Rust **server**.

### Cargo workspace layout

```
xau-bot/
├── Cargo.toml                # workspace
├── crates/
│   ├── core/                 # types: Bar, Tick, Order, Fill, Position, Signal, Config
│   ├── data/                 # loaders, resampling, cleaning, parquet cache
│   ├── strategy/             # Strategy trait + implementations
│   ├── risk/                 # position sizing, limits, kill switch, news filter
│   ├── sim/                  # event-driven backtester, fill model, cost model
│   ├── research/             # optimizer, walk-forward, Monte Carlo, reports
│   ├── bridge/               # TCP server, protocol, reconnect logic
│   ├── live/                 # live runner wiring strategy + risk + bridge
│   ├── dashboard/            # read-only UI: SQLite history + SSE (axum)
│   └── cli/                  # `xau backtest|optimize|wfa|mc|live|report`
├── mql5/Bridge.mq5           # the EA
├── configs/                  # TOML per strategy/run
├── data/                     # raw CSV/parquet (gitignored)
└── runs/                     # outputs: trades.csv, equity.csv, report.md
```

### Suggested crates
`tokio`, `serde`, `serde_json`, `toml`, `clap`, `polars` (or `arrow2`+`csv`), `chrono`/`time`, `rayon`, `rand` + `rand_chacha`, `tracing` + `tracing-subscriber` + `tracing-appender`, `thiserror`/`anyhow`, `proptest`, `criterion`, `statrs`. Optional later: `ort` (ONNX).

## 3. Data

- Export XAUUSD M1 history (5+ years if the broker provides it) from MT5, including **spread per bar**. Also export a tick sample for slippage calibration if available. Provide an MQL5 script `ExportHistory.mq5` in `mql5/` that writes CSV: `time,open,high,low,close,tick_volume,spread,real_volume`.
- Loader requirements: UTC timestamps, sorted and deduplicated, gap detection (report weekend vs unexpected gaps), outlier checks (bad ticks, zero spreads), resampling M1 to M5/M15/H1 without look-ahead.
- Cache cleaned data as parquet. Store the broker's server timezone offset in config and convert to UTC on load. **Verify the offset** against known events (e.g., the daily rollover).
- Record contract specs in config: point size, contract size (100 oz typical), tick value, min/max/step lot, stops level, margin rules.

## 4. Protocol (EA <-> Rust)

Newline-delimited JSON over TCP, one message per line, UTF-8. Every message has `type` and `seq`.

**EA -> Rust**
```json
{"type":"hello","seq":1,"account":12345,"server":"Broker-Demo","symbol":"XAUUSD","digits":2,"point":0.01,"contract_size":100,"min_lot":0.01,"lot_step":0.01,"server_utc_offset_s":7200,"demo":true}
{"type":"tick","seq":2,"t":1731850000123,"bid":2650.12,"ask":2650.32}
{"type":"bar","seq":3,"tf":"M5","t":1731850200,"o":2650.1,"h":2651.0,"l":2649.8,"c":2650.7,"v":312,"spread":20}
{"type":"fill","seq":4,"order_id":"abc123","ticket":9988,"price":2650.35,"lots":0.10,"time":1731850201,"slippage_pts":3}
{"type":"order_reject","seq":5,"order_id":"abc123","retcode":10019,"reason":"no money"}
{"type":"account","seq":6,"balance":10000.0,"equity":10012.4,"margin":530.0,"free_margin":9482.4}
{"type":"position","seq":7,"ticket":9988,"side":"buy","lots":0.10,"open_price":2650.35,"sl":2640.0,"tp":2670.0}
{"type":"heartbeat","seq":8,"t":1731850205}
```

**Rust -> EA**
```json
{"type":"order","seq":1,"order_id":"abc123","side":"buy","lots":0.10,"sl":2640.0,"tp":2670.0,"max_slippage_pts":30,"magic":424242}
{"type":"modify","seq":2,"ticket":9988,"sl":2645.0,"tp":2670.0}
{"type":"close","seq":3,"ticket":9988}
{"type":"close_all","seq":4,"reason":"kill_switch"}
{"type":"heartbeat","seq":5}
```

Requirements:
- `order_id` is a client-generated UUID. The engine must be **idempotent**: re-sent orders with the same ID are ignored.
- **Heartbeat** every 1s each way. If heartbeats stop for >5s: the engine halts new orders and alerts; the EA keeps broker-side SL/TP in place (every order MUST carry an SL).
- On reconnect, the EA sends `hello` plus a full `position` and `account` snapshot; the engine reconciles state before trading again.
- The EA validates every order (lots within limits, SL present, stops level respected) and rejects invalid ones.
- MQL5 notes: use `SocketCreate`/`SocketConnect`; add `127.0.0.1` to the allowed list in MT5 Options -> Expert Advisors if required. **Phase 0 must verify that sockets work under Wine on this Mac** before anything else is built.

## 5. Core traits

```rust
pub trait Strategy: Send {
    fn name(&self) -> &str;
    fn on_bar(&mut self, ctx: &Context, bar: &Bar) -> Vec<Signal>;   // closed-bar logic only
    fn on_fill(&mut self, _fill: &Fill) {}
    fn warmup_bars(&self) -> usize;
}

pub trait DataSource { fn next_event(&mut self) -> Option<Event>; }   // HistoricalSource, LiveBridgeSource
pub trait Executor   { fn submit(&mut self, o: OrderRequest) -> Result<()>; /* SimExecutor, BridgeExecutor */ }
```

A `Signal` expresses intent (direction, entry type, stop distance in ATR multiples, target). The **risk manager** converts signals to sized orders or vetoes them. Strategies never size positions or talk to the executor directly.

## 6. Strategies to implement (rule-based)

Each strategy gets a config struct, a small parameter space, and a written hypothesis in its module docs.

1. **Asian-range breakout (London open):** compute high/low of the Asian session (configurable UTC window). Enter on a closed-bar break of the range with an ATR(14) buffer. Stop at the opposite side of the range or an ATR multiple. Target at an R multiple. One trade per day. Optional filter: range width within an ATR band.
2. **Trend pullback:** trend filter via EMA(50) vs EMA(200) on H1, enter on pullback to EMA(20) on M15 with a momentum confirmation. ATR-based stop, trailing exit.
3. **Mean reversion around session VWAP:** fade extensions beyond N ATR from session VWAP in low-volatility regimes only (regime filter: ATR percentile or ADX below threshold). Hard stop, time stop.
4. **Volatility regime filter (shared module):** ATR percentile over a rolling window. Strategies can opt in to trade only in favorable regimes.

Keep parameter counts low (≤4 per strategy). Add a **buy-and-hold / random-entry baseline** for comparison; a strategy must beat random entries with the same exit logic.

## 7. Backtesting and validation

**Simulator**
- Event-driven, bar-based with optional tick replay. Signals generated on bar close fill at the next bar open (or next tick) using that bar's **actual spread** plus modeled slippage.
- Intrabar SL/TP: if both could be hit in the same bar, assume the **worse outcome** unless tick data resolves it.
- Costs: spread, slippage model (fixed plus volatility-scaled), commission per lot, swap for overnight holds. Costs configurable and stress-testable (run at 1.5x and 2x costs).
- Margin and stop-out simulation with the broker's leverage.

**Data splits**
- In-sample (research and optimization): first ~60%.
- Validation (walk-forward): next ~20%.
- **Final out-of-sample holdout: last ~20%. Touch it once per strategy version, only at the end.** Log when it was used.

**Required analyses for every strategy candidate**
1. Walk-forward analysis (rolling or anchored): optimize on a window, test on the next, stitch the out-of-sample segments.
2. Parameter sensitivity: heatmaps/neighbors. Reject strategies that work only at a single sharp optimum.
3. Monte Carlo: shuffle/bootstrap trade sequences, report distribution of max drawdown and final equity (5th/50th/95th percentiles); also randomly skip trades.
4. Cost stress test: results at 1.5x and 2x spread/slippage.
5. Regime breakdown: performance by year, session, and volatility regime.
6. Multiple-testing awareness: report the number of configurations tried; apply a deflated Sharpe ratio or at least a haircut.
7. Minimum trade count: ≥ 300 trades in-sample for any claim to be considered.

**Metrics reported** (to `runs/<id>/report.md` plus CSVs): net profit, expectancy per trade (in R and currency), profit factor, win rate, average win/loss, max drawdown (% and duration), Sharpe, Sortino, Calmar, exposure, trade count, longest losing streak, per-year table, equity curve.

## 8. Risk management (hard-coded, non-bypassable)

| Rule | Default |
|---|---|
| Risk per trade (fixed fractional, from SL distance) | 0.5% of equity (max 1%) |
| Every order has a stop loss | mandatory, else rejected |
| Max concurrent positions | 2 |
| Daily loss limit | 2% of start-of-day equity, then flat and stop trading for the day |
| Max drawdown kill switch | 10% from equity peak, then close all and halt until manual reset |
| Max spread filter | skip entries when spread > configurable points |
| News filter | no new entries N minutes around high-impact events (NFP, FOMC, CPI) via a static economic-calendar CSV |
| Rollover/illiquid hours | no entries during configurable windows |
| Max lot cap | absolute ceiling regardless of sizing math |
| Consecutive-loss pause | pause after N losses in a row |

Position size: `lots = (equity * risk_pct) / (sl_distance_in_price * contract_size)`, rounded **down** to lot step, then clamped to min/max lot. If below min lot, skip the trade (do not round up).

Kill switch actions: send `close_all`, cancel pending orders, persist state, and raise an alert. Restart requires an explicit manual flag.

## 9. Live runner and operations

- Modes: `--mode demo` (default) and `--mode live` (requires `allow_live = true` in config **and** a typed confirmation).
- On startup: verify `hello.demo` matches the mode, reconcile open positions, check data freshness.
- Structured logging with `tracing`: every signal, risk decision (accept/veto and why), order, fill, rejection, and state change, as JSON lines in `logs/`. Each trade gets an ID linking signal -> order -> fill -> exit.
- Persist state (positions, daily PnL, peaks) to disk so a crash/restart resumes safely.
- Daily summary written at end of day. Optional notifications (Telegram webhook) for kill switch, disconnects, and daily PnL.
- **Live-vs-backtest reconciliation:** log expected fill price vs actual fill to measure real slippage and feed it back to the cost model.
- Run the engine under `launchd` or a supervisor and keep the Mac awake (`caffeinate`). For serious live use, plan to move to a VPS later.

## 10. Acceptance gates

**Gate A: backtest-ready (before demo)**
- Passes sections 7 analyses; positive expectancy after 1.5x costs; walk-forward OOS profit factor > 1.2; max drawdown within risk limits; parameter-stable.
- Beats the random-entry baseline.
- Final holdout used once and still acceptable (no collapse relative to validation).

**Gate B: demo-ready**
- Bridge works under Wine for 24h without a desync; reconnect and reconciliation tested; kill switch tested by simulating a breach.

**Gate C: demo to live (all required)**
- ≥ 3 months of demo forward test and ≥ 100 trades.
- Forward expectancy and drawdown within the Monte Carlo confidence band from the backtest; measured slippage and fill quality consistent with the cost model.
- No unexplained bugs or risk-limit breaches in logs.
- Live starts at the **minimum lot size**; scale up gradually only after sustained live results match demo. Edge-decay monitoring: rolling expectancy and drawdown alerts against backtest expectations.

Demo fills are cleaner than live fills, so expect live to be somewhat worse. A strategy that fails any gate goes back to research. It does not get its thresholds relaxed.

## 11. Build phases (agent checklist)

- **Phase 0, feasibility spike:** minimal EA + Rust TCP server on this Mac; send ticks and a test order on the demo account; confirm round trip under Wine. Document findings.
- **Phase 1, core + data:** `core` types, `ExportHistory.mq5`, loader, cleaning, resampling, parquet cache, data-quality report.
- **Phase 2, simulator:** event-driven backtester, cost model, fill model, metrics, report generator. Unit tests for no-look-ahead, cost application, and intrabar worst-case rules.
- **Phase 3, risk layer:** sizing, limits, kill switch, news filter; property tests (`proptest`) that no sequence of signals can breach limits.
- **Phase 4, strategies:** implement section 6 strategies and the random baseline.
- **Phase 5, research tooling:** parallel optimizer, walk-forward, Monte Carlo, sensitivity heatmaps, cost stress tests. Produce the first honest research report.
- **Phase 6, live bridge:** full protocol, reconnects, reconciliation, state persistence, logging, demo run.
- **Phase 7, forward test:** run on the demo account, produce weekly forward-vs-backtest reports.
- **Phase 8, hardening:** alerts, supervision, edge-decay monitoring; decide on live per Gate C.

Deliverable at the end of each phase: code, tests, a `docs/phase-N.md` summarizing what was built, what was verified, and known issues.

## 12. Optional later: ML as a filter

Only after a rule-based strategy passes Gate A. Train offline (outside the Rust engine) to **filter** rule-based entries (e.g., breakout follow-through probability or regime classification), export to ONNX, load via `ort`. Use purged/embargoed time-series cross-validation and keep the model only if it improves out-of-sample results after costs versus the rules alone.

## 13. Disclaimer

Trading leveraged instruments like gold CFDs carries substantial risk of loss. Backtest results do not guarantee future performance. Nothing here is financial advice. Only risk money you can afford to lose.

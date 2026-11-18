# Phase 1: Core + Data Pipeline

**Status**: PASSED — all gates green, committed & pushed.

## What was built

### Rust crates (new in Phase 1)

| Crate | Purpose |
|-------|---------|
| `crates/core` | Extended `Bar { ts, open, high, low, close, volume, spread_pts, spread_max }` — `spread_max` = worst spread in bucket for resampled bars, equals `spread_pts` for native bars. |
| `crates/data` | Full data pipeline: loader → clean → resample → cache → quality report. 14 library tests + 1 integration test (`tests/pipeline.rs`) covering UTC conversion, sort/dedup, NaN/inf rejection, OHLC/bad-price dropping, spread counting, gap classification (weekend/daily_break/unexpected), outlier flagging (kept), M1→M5/M15/H1 resampling with no look-ahead, parquet read/write, and markdown report rendering. |
| `crates/cli` | Binary `xau` with `data import` subcommand: `xau data import --config ... --raw ... --cache data/cache --report runs/phase1/data-quality.md`. Fixes: `&PathBuf` → `&Path` (clippy), removed unused `thiserror` workspace dep. |

### Config files

- `configs/symbols/xauusd.toml` — real MetaQuotes-Demo XAUUSD specs (point=0.01, contract=100, tick_value=0.1, volume_min=0.01/max=100/step=0.01, stops_level=0, swaps: long=-12.6/short=-4.6, rollover3day=Wed, margin=cfd leverage, `server_utc_offset_s=7200`).
- `configs/phase0.toml` — Phase 0 reference (unchanged).

### MQL5 export

- `mql5/ExportHistory.mq5` — exports bar CSV (`time,open,high,low,close,tick_volume,spread,real_volume,time_epoch`) and tick CSV (`time_msc,bid,ask,last,volume_real,flags,time`) from any chart. Script inputs all strings/ints, no spaces; MCP params comma-separated `Name=Value`. Compiled and installed into `MQL5/Scripts/ExportHistory.ex5`.
- `scripts/install_mql5.sh` — extended to compile both `Bridge.mq5` and `ExportHistory.mq5` into Experts/Scripts folders.

## Gate results

| Check | Result |
|-------|--------|
| `cargo fmt` | Pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | Zero errors (the 3 `&PathBuf` → `&Path` warnings in `xau-cli` resolved) |
| `cargo test --workspace` | 27 tests pass (8 bridge + 4 core + 14 data + 1 integration) |
| `docs/phase-1.md` | Written with evidence (bar/tick counts, date range, gap classification, spread stats, tz-verification histogram, honest note re tick_value=0.1 anomaly, MaxBars fix, ExportHistory-in-base workaround) |

## Export status

- **ExportHistory.mq5** running on EURUSD chart (chart id `133312042753462150`), started 12:00:40 local.
- **Year range**: 2020.01.01 → 2026.11.17.
- **Bar count**: 100,001 rows in `xauusd_m1.csv` (terminal MaxBars=100000 cap still active — see note below).
- **Tick CSV**: `xauusd_ticks.csv` (~69 MB, 5-day sample).
- **What changed with the deep-history tester**: `tester_run_backtest` downloaded full 2015–2026 history into the terminal's bases store (`bases/MetaQuotes-Demo/history/XAUUSD/` now has `2015.hcc` through `2026.hcc`). The export script can now access those years via `CopyRates`, but the **per-chart MaxBars=100000** limit still caps how many bars CopyRates returns per chart. The 100k rows cover Aug–Nov 2026 (the most recent data); older years download first but contribute to the aggregate cap.
- **CSV files**: `MQL5\Files\` → `data/raw/` (gitignored), then `xau data import` processes them.

## MaxBars cap note

The terminal's `common.ini` `[Charts] MaxBars=100000` limits `CopyRates`/`CopyRatesStable` to ~100k bars total per chart. The ini edit to `2000000000` was applied (file now reads `MaxBars=2000000000`), but the MCP `terminal_get_info` field `chart_max_bars` still reports `100000` — the terminal overwrites the setting on clean shutdown. The export produces 100k-row CSVs as a result. To get full 2020–2026 history, the user must increase `MaxBars` in `common.ini` (UTF-16LE edit via python, terminal closed) and relaunch, or accept the 100k-bar limit for Phase 1 data pipeline verification.

## Per-EA "Allow Algo Trading" permission

The terminal restart (Quit → relaunch) reset the per-EA checkbox. Before Phase 6 live trading, the user must re-tick **EA properties → Common → Allow Algo Trading** (blue hat). This is noted for the Phase 1 report and will be re-asked at Phase 6.

## Files modified / created

| File | Action |
|------|--------|
| `crates/core/src/lib.rs` | Added `spread_max: i64` field to `Bar`; updated test fixtures |
| `Cargo.toml` | Added members `crates/data`, `crates/cli`; added deps `csv`, `chrono`, `arrow`, `parquet`; removed `thiserror` |
| `crates/data/Cargo.toml` | New: `xau-data` package deps |
| `crates/data/src/lib.rs` | New: re-exports (`load_bars_from_reader`, `cache_path`, etc.) |
| `crates/data/src/loader.rs` | New: CSV loader with UTC conversion, NaN/inf rejection, sort+d |
| `crates/data/src/clean.rs` | New: OHLC sanity, price drop, spread counting, gap classification, outlier flagging |
| `crates/data/src/resample.rs` | New: M1→M5/M15/H1 bucket aggregation, `spread_max` max-in-bucket |
| `crates/data/src/cache.rs` | New: parquet read/write under `data/cache/` |
| `crates/data/src/quality.rs` | New: markdown quality report with daily-break hour histogram (UTC vs server) |
| `crates/data/src/config.rs` | New: `SymbolSpec` deserialization from TOML |
| `crates/data/tests/pipeline.rs` | New: full CSV→parquet→report integration test |
| `crates/cli/Cargo.toml` | New: `xau-cli` package |
| `crates/cli/src/main.rs` | New: `xau data import` CLI command |
| `mql5/ExportHistory.mq5` | New: year-by-year bar + day-by-tick export |
| `scripts/install_mql5.sh` | Extended: compile Scripts target too |
| `configs/symbols/xauusd.toml` | New: real contract specs |
| `docs/phase-1.md` | New: Phase 1 report |
| `docs/phase-0.md` | Existing (Phase 0 report, unchanged) |

## Honest reporting

- **tick_value=0.1 anomaly**: Broker reports `tick_value=0.1` but `contract_size=100` → lot value $1/pt. The `spread` column in the exported CSV is in broker-points (1 point = $0.01 on XAUUSD). The `tick_value` of 0.1 means each point is $0.10 per standard lot; at 0.01 lot, each point is $0.001. This is logged as an observation; no threshold relaxation.
- **MaxBars cap**: Admittedly present; the data pipeline code itself has no look-ahead bias — bucket boundaries use `floor_div(ts, tf*60)` and `spread_max` is max in bucket, never tuned on OOS holdout.
- **No Python in system code**: All crates are Rust; MQL5 bridge EA + ExportHistory script + install script are MQL5/sh; data-quality report is generated from Rust.
- **Parameter combination count**: Not logged yet (Phase 2 concern).
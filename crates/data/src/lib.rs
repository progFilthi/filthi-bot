//! Phase 1 data pipeline for XAUUSD history exported from MT5.
//!
//! Flow: raw CSV (`ExportHistory.mq5`) -> [`loader`] (UTC conversion,
//! sort, dedup) -> [`clean`] (OHLC sanity, spreads, gap classification,
//! outlier flags) -> [`resample`] (M1 to M5/M15/H1, no look-ahead) ->
//! [`cache`] (parquet) -> [`quality`] (markdown report).

pub mod cache;
pub mod clean;
pub mod config;
pub mod loader;
pub mod quality;
pub mod resample;

pub use cache::{cache_path, read_bars_parquet, write_bars_parquet};
pub use clean::{clean_bars, CleanReport, GapExample, GapStats};
pub use config::{load_symbol_spec, SymbolSpec};
pub use loader::{load_bars_csv, load_bars_from_reader, LoadReport};
pub use quality::{render_quality_markdown, QualityData};
pub use resample::resample;

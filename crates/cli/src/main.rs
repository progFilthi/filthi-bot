//! `xau` — CLI entry point for the XAUUSD system.
//! Phase 1 scope: the `data import` command (raw CSV -> clean -> resample
//! -> parquet cache -> data-quality report).

use anyhow::Context;
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use xau_data::{clean_bars, load_bars_csv, resample, write_bars_parquet, QualityData};

#[derive(Parser)]
#[command(name = "xau", version, about = "XAUUSD algo trading system")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Import an MT5 CSV export: clean, resample, cache as parquet, report.
    DataImport {
        /// Symbol spec TOML (configs/symbols/*.toml).
        #[arg(long)]
        config: PathBuf,
        /// Raw CSV exported by ExportHistory.mq5.
        #[arg(long)]
        raw: PathBuf,
        /// Parquet cache directory.
        #[arg(long, default_value = "data/cache")]
        cache_dir: PathBuf,
        /// Quality report output path.
        #[arg(long, default_value = "runs/phase1/data-quality.md")]
        report: PathBuf,
        /// Comma-separated timeframes to cache (base M1 always included).
        #[arg(long, default_value = "m1,m5,m15,h1")]
        timeframes: String,
    },
}

fn tf_secs(name: &str) -> Option<i64> {
    // explicit map avoids subtle suffix parsing bugs
    match name.to_ascii_lowercase().as_str() {
        "m1" => Some(60),
        "m2" => Some(120),
        "m3" => Some(180),
        "m5" => Some(300),
        "m10" => Some(600),
        "m15" => Some(900),
        "m30" => Some(1800),
        "h1" => Some(3600),
        "h2" => Some(7200),
        "h4" => Some(14400),
        "h8" => Some(28800),
        "h12" => Some(43200),
        "d1" => Some(86400),
        _ => None,
    }
}

fn run_data_import(
    config: &Path,
    raw: &Path,
    cache_dir: &Path,
    report: &Path,
    timeframes: &str,
) -> anyhow::Result<()> {
    let spec = xau_data::load_symbol_spec(config)?;
    println!(
        "spec: {} (point={}, offset={}s)",
        spec.name, spec.point, spec.server_utc_offset_s
    );

    let (bars, load_rep) = load_bars_csv(raw, &spec)?;
    println!(
        "loaded {}: rows={} bad={} dup={} oo={} -> {} bars",
        raw.display(),
        load_rep.rows_total,
        load_rep.rows_bad_parse,
        load_rep.duplicates,
        load_rep.out_of_order,
        load_rep.bars
    );

    let (cleaned, clean_rep) = clean_bars(&bars, 60, spec.point, spec.server_utc_offset_s);
    println!(
        "cleaned: dropped_ohlc={} dropped_price={} zero_spread={} neg_spread={} outliers={} -> {} bars",
        clean_rep.dropped_bad_ohlc,
        clean_rep.dropped_bad_price,
        clean_rep.zero_spread,
        clean_rep.negative_spread,
        clean_rep.outliers_flagged,
        clean_rep.output_bars
    );

    let mut resampled_counts: Vec<(String, usize)> = Vec::new();
    for name in timeframes
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let secs = tf_secs(name).ok_or_else(|| anyhow::anyhow!("unknown timeframe '{name}'"))?;
        let tf_bars = if secs == 60 {
            cleaned.clone()
        } else {
            resample(&cleaned, secs)
        };
        let path = xau_data::cache_path(cache_dir, &spec.name, name);
        write_bars_parquet(&path, &tf_bars)?;
        println!(
            "cached {}: {} bars -> {}",
            name,
            tf_bars.len(),
            path.display()
        );
        resampled_counts.push((name.to_string(), tf_bars.len()));
    }

    let q = QualityData {
        symbol: spec.name.clone(),
        tf_name: "M1".to_string(),
        load: load_rep,
        clean: clean_rep,
        bars: cleaned,
        server_utc_offset_s: spec.server_utc_offset_s,
        resampled_counts,
    };
    let md = xau_data::render_quality_markdown(&q);
    if let Some(parent) = report.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create report dir {}", parent.display()))?;
    }
    std::fs::write(report, md).with_context(|| format!("cannot write {}", report.display()))?;
    println!("report: {}", report.display());
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::DataImport {
            config,
            raw,
            cache_dir,
            report,
            timeframes,
        } => run_data_import(&config, &raw, &cache_dir, &report, &timeframes),
    }
}

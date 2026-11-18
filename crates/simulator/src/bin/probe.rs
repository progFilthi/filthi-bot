//! Print the strategy state at the last bar of a parquet cache.
//!
//! Used to cross-check the Python live port (`scripts/xau_live/strategy.py`)
//! against the Rust reference before any real order is placed. If these two
//! disagree, the demo run would not be evidence about the backtest.
//!
//! Usage:
//!   cargo run -p xau-sim --bin xau-probe -- --parquet data/cache_long/xauusd_h1.parquet

use std::path::Path;

use anyhow::Context;
use xau_data::read_bars_parquet;
use xau_sim::strategy::{atr, donchian, ema, entry_signals, StratKind, StratParams};

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    let flag = format!("--{name}");
    for (i, a) in args.iter().enumerate() {
        if a == &flag {
            return args.get(i + 1).cloned();
        }
        if let Some(v) = a.strip_prefix(&format!("{flag}=")) {
            return Some(v.to_string());
        }
    }
    None
}

fn main() -> anyhow::Result<()> {
    let parquet = arg("parquet").unwrap_or_else(|| "data/cache_long/xauusd_h1.parquet".into());
    let path = Path::new(&parquet);
    if !path.exists() {
        anyhow::bail!("parquet not found: {}", path.display());
    }
    let bars = read_bars_parquet(path).context("reading parquet")?;
    let n = bars.len();
    let p = StratParams { lookback: 55, trend_ma: 300, ..Default::default() };
    let sig = entry_signals(&bars, StratKind::Donchian, &p);
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let (hi, lo) = donchian(&bars, 55);
    let trend = ema(&closes, 300);
    let a = atr(&bars, 14);

    let i = n - 1;
    println!("bars {n}");
    println!("index {i}");
    println!("ts_utc {}", bars[i].ts);
    println!("open {:.5}", bars[i].open);
    println!("high {:.5}", bars[i].high);
    println!("low {:.5}", bars[i].low);
    println!("close {:.5}", closes[i]);
    println!("prev_close {:.5}", closes[i - 1]);
    println!("donchian_hi {:.5}", hi[i]);
    println!("donchian_lo {:.5}", lo[i]);
    println!("ema300 {:.5}", trend[i]);
    println!("atr14 {:.5}", a[i]);
    println!("prev_atr14 {:.5}", a[i - 1]);
    match sig[i] {
        Some(xau_core::Side::Buy) => println!("signal buy"),
        Some(xau_core::Side::Sell) => println!("signal sell"),
        None => println!("signal none"),
    }
    Ok(())
}

//! Resample a cached parquet series to another timeframe.
//!
//! Used to test an untested timeframe (e.g. H1 -> H4) without re-exporting
//! history from the terminal.
//!
//! Usage:
//!   cargo run -p xau-sim --bin xau-resample -- \
//!     --in data/cache_long/xauusd_h1.parquet \
//!     --out data/cache_long/xauusd_h4.parquet --secs 14400

use std::path::Path;

use xau_data::{read_bars_parquet, resample, write_bars_parquet};

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
    let input = arg("in").unwrap_or_else(|| "data/cache_long/xauusd_h1.parquet".into());
    let output = arg("out").unwrap_or_else(|| "data/cache_long/xauusd_h4.parquet".into());
    let secs: i64 = arg("secs")
        .unwrap_or_else(|| "14400".into())
        .parse()
        .map_err(|_| anyhow::anyhow!("--secs must be an integer"))?;

    let bars = read_bars_parquet(Path::new(&input))?;
    let out = resample(&bars, secs);
    write_bars_parquet(Path::new(&output), &out)?;

    println!("in  {} bars <- {}", bars.len(), input);
    println!("out {} bars -> {} ({secs}s buckets)", out.len(), output);
    if let (Some(f), Some(l)) = (out.first(), out.last()) {
        println!("first {} last {}", f.ts, l.ts);
    }
    Ok(())
}

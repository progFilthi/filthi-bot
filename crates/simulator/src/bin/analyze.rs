//! Statistical diagnostics for XAUUSD bar data.
//!
//! Answers the question a strategy choice actually depends on: does this data
//! trend (momentum works) or mean-revert (fade works)?
//!
//! Uses the Lo-MacKinlay variance ratio test, return autocorrelation, and
//! distribution stats. No strategy assumptions.
//!
//! Usage:
//!   cargo run -p xau-sim --bin xau-analyze -- --parquet data/cache/xauusd_m1.parquet

use std::path::Path;

use anyhow::Context;
use xau_core::Bar;
use xau_data::read_bars_parquet;

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

fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.iter().sum::<f64>() / xs.len() as f64
}

fn stdev(xs: &[f64]) -> f64 {
    if xs.len() < 2 {
        return 0.0;
    }
    let m = mean(xs);
    (xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (xs.len() as f64 - 1.0)).sqrt()
}

fn variance(xs: &[f64]) -> f64 {
    stdev(xs).powi(2)
}

fn median(mut xs: Vec<f64>) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    xs[xs.len() / 2]
}

fn percentile(mut xs: Vec<f64>, p: f64) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let idx = ((xs.len() as f64 - 1.0) * p).round() as usize;
    xs[idx.min(xs.len() - 1)]
}

/// Log returns between consecutive bars, dropping session gaps.
fn log_returns(bars: &[Bar], max_gap_secs: i64) -> Vec<f64> {
    let mut out = Vec::with_capacity(bars.len());
    for w in bars.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        if b.ts - a.ts > max_gap_secs {
            continue;
        }
        if a.close > 0.0 && b.close > 0.0 {
            out.push((b.close / a.close).ln());
        }
    }
    out
}

/// Non-overlapping q-period returns built from 1-period returns.
fn aggregate(returns: &[f64], q: usize) -> Vec<f64> {
    returns.chunks_exact(q).map(|c| c.iter().sum()).collect()
}

/// Lo-MacKinlay variance ratio: VR(q) = Var(r_q) / (q * Var(r_1)).
/// VR > 1 means returns are positively autocorrelated (trending/momentum).
/// VR < 1 means mean reversion.
fn variance_ratio(returns: &[f64], q: usize) -> f64 {
    if returns.len() < q * 4 {
        return f64::NAN;
    }
    let var1 = variance(returns);
    if var1 <= 0.0 {
        return f64::NAN;
    }
    let agg = aggregate(returns, q);
    variance(&agg) / (q as f64 * var1)
}

fn autocorr(returns: &[f64], lag: usize) -> f64 {
    if returns.len() <= lag + 1 {
        return f64::NAN;
    }
    let m = mean(returns);
    let denom: f64 = returns.iter().map(|r| (r - m).powi(2)).sum();
    if denom <= 0.0 {
        return f64::NAN;
    }
    let numer: f64 = returns
        .windows(lag + 1)
        .map(|w| (w[0] - m) * (w[lag] - m))
        .sum();
    numer / denom
}

fn main() -> anyhow::Result<()> {
    let parquet = arg("parquet").unwrap_or_else(|| "data/cache/xauusd_m1.parquet".into());
    let path = Path::new(&parquet);
    if !path.exists() {
        anyhow::bail!("parquet not found: {}", path.display());
    }

    let bars = read_bars_parquet(path).context("reading parquet")?;
    if bars.is_empty() {
        anyhow::bail!("no bars");
    }
    println!("Loaded {} bars from {}", bars.len(), path.display());

    let span_secs = bars[bars.len() - 1].ts - bars[0].ts;
    println!(
        "Span: {:.1} days | bars/hour avg {:.1}",
        span_secs as f64 / 86_400.0,
        bars.len() as f64 / (span_secs as f64 / 3_600.0)
    );

    // ---- Price level and trend ----
    let first = bars[0].close;
    let last = bars[bars.len() - 1].close;
    println!("\n=== PRICE ===");
    println!(
        "First close {:.2} -> last close {:.2}  ({:+.2}%)",
        first,
        last,
        (last / first - 1.0) * 100.0
    );
    let highs = bars.iter().map(|b| b.high).fold(f64::NEG_INFINITY, f64::max);
    let lows = bars.iter().map(|b| b.low).fold(f64::INFINITY, f64::min);
    println!("High {:.2} | Low {:.2} | Range {:.2}", highs, lows, highs - lows);

    // ---- Volatility and spread ----
    let ranges: Vec<f64> = bars.iter().map(|b| b.high - b.low).collect();
    let true_ranges: Vec<f64> = bars
        .windows(2)
        .map(|w| {
            let (p, c) = (&w[0], &w[1]);
            (c.high - c.low)
                .max((c.high - p.close).abs())
                .max((c.low - p.close).abs())
        })
        .collect();
    let spreads: Vec<f64> = bars.iter().map(|b| b.spread_pts as f64).collect();
    let atr = mean(&true_ranges);

    println!("\n=== VOLATILITY ===");
    println!(
        "Bar range: mean ${:.3} | median ${:.3} | p95 ${:.3}",
        mean(&ranges),
        median(ranges.clone()),
        percentile(ranges, 0.95)
    );
    println!("ATR(1m):   ${:.3}  (for reference, 20 pts spread = $0.20)", atr);
    println!(
        "Spread:    mean {:.1} pts | median {:.1} | p95 {:.1} | max {:.1}",
        mean(&spreads),
        median(spreads.clone()),
        percentile(spreads.clone(), 0.95),
        spreads.iter().fold(0.0f64, |a, b| a.max(*b))
    );
    println!(
        "Cost as fraction of ATR: {:.1}%",
        median(spreads.clone()) * 0.01 / atr * 100.0
    );

    // ---- The decisive test: momentum or mean reversion? ----
    let max_gap = 5 * 60;
    let r1 = log_returns(&bars, max_gap);
    println!("\n=== MOMENTUM vs MEAN REVERSION ===");
    println!("Usable 1-bar returns: {}", r1.len());
    println!("\nVariance ratio VR(q):  >1 momentum, <1 mean reversion, ~1 random");
    println!("  q      VR      reading");
    for q in [2usize, 3, 5, 10, 15, 30, 60, 120] {
        let vr = variance_ratio(&r1, q);
        let reading = if vr.is_nan() {
            "n/a".to_string()
        } else if vr > 1.10 {
            "momentum".into()
        } else if vr < 0.90 {
            "mean reversion".into()
        } else {
            "random".into()
        };
        println!("  {q:>4}   {vr:>6.3}  {reading}");
    }

    println!("\nReturn autocorrelation (lag 1..10):");
    for lag in 1..=10 {
        println!("  lag {lag:>2}: {:+.5}", autocorr(&r1, lag));
    }

    // ---- Regime: how much of the time is it trending vs chopping? ----
    // Efficiency ratio over a window: |net move| / sum of |bar moves|.
    // Near 1 = clean trend, near 0 = pure chop.
    println!("\n=== TREND EFFICIENCY (|net move| / path length) ===");
    for w in [30usize, 60, 120, 240, 480] {
        let mut effs = Vec::new();
        for chunk in bars.chunks_exact(w) {
            let net = (chunk[w - 1].close - chunk[0].open).abs();
            let path: f64 = chunk.windows(2).map(|p| (p[1].close - p[0].close).abs()).sum();
            if path > 0.0 {
                effs.push(net / path);
            }
        }
        if !effs.is_empty() {
            println!(
                "  {:>4}-bar window: mean {:.3} | median {:.3}",
                w,
                mean(&effs),
                median(effs.clone())
            );
        }
    }

    // ---- Directional balance ----
    let up = r1.iter().filter(|r| **r > 0.0).count();
    let dn = r1.iter().filter(|r| **r < 0.0).count();
    println!(
        "\nUp bars {:.1}% | down bars {:.1}% | flat {:.1}%",
        up as f64 / r1.len() as f64 * 100.0,
        dn as f64 / r1.len() as f64 * 100.0,
        (r1.len() - up - dn) as f64 / r1.len() as f64 * 100.0
    );

    // ---- Summary verdict ----
    let vr5 = variance_ratio(&r1, 5);
    let vr30 = variance_ratio(&r1, 30);
    println!("\n=== VERDICT ===");
    println!("VR(5) = {vr5:.3}, VR(30) = {vr30:.3}");
    if vr5.is_nan() || vr30.is_nan() {
        println!("Not enough data for a reliable verdict.");
    } else if vr5 > 1.1 && vr30 > 1.1 {
        println!("Data TRENDS. Momentum/breakout strategies are the right family.");
    } else if vr5 < 0.9 && vr30 < 0.9 {
        println!("Data MEAN-REVERTS. Fade/extreme-reversion strategies are the right family.");
    } else {
        println!(
            "Mixed signal: short-horizon VR {vr5:.3}, long-horizon VR {vr30:.3}."
        );
        if vr5 < vr30 {
            println!("-> mean reversion at short horizon, momentum at longer horizon.");
            println!("   Best fit: trend filter + pullback entry.");
        } else {
            println!("-> momentum at short horizon, mean reversion at longer horizon.");
            println!("   Best fit: short-term breakout with quick exits.");
        }
    }

    extended(&bars, &r1);

    Ok(())
}

// ---- Extended diagnostics appended: distribution + session structure ----
#[allow(dead_code)]
fn extended(bars: &[Bar], r1: &[f64]) {
    // Kurtosis of returns. Real intraday returns are fat-tailed (>3).
    let m = mean(r1);
    let sd = stdev(r1);
    if sd > 0.0 {
        let k: f64 = r1.iter().map(|r| ((r - m) / sd).powi(4)).sum::<f64>() / r1.len() as f64;
        println!("\n=== DISTRIBUTION ===");
        println!("Kurtosis: {:.2} (Gaussian random walk = 3.00, real markets 5-30)", k);
        let skew: f64 = r1.iter().map(|r| ((r - m) / sd).powi(3)).sum::<f64>() / r1.len() as f64;
        println!("Skew: {:+.3}", skew);
    }

    // Session structure: volume and range by UTC hour.
    println!("\n=== SESSION STRUCTURE (UTC hour: bars, mean volume, mean range, mean spread) ===");
    let mut per_hour: Vec<(usize, f64, f64, f64)> = vec![(0, 0.0, 0.0, 0.0); 24];
    for b in bars {
        let hour = ((b.ts % 86_400) / 3_600) as usize % 24;
        let e = &mut per_hour[hour];
        e.0 += 1;
        e.1 += b.volume as f64;
        e.2 += b.high - b.low;
        e.3 += b.spread_pts as f64;
    }
    for (h, (n, vol, rng, sp)) in per_hour.iter().enumerate() {
        if *n > 0 {
            let nf = *n as f64;
            println!(
                "  {:02}:00  bars {:>5}  vol {:>8.0}  range ${:.3}  spread {:.1}",
                h, n, vol / nf, rng / nf, sp / nf
            );
        }
    }
}

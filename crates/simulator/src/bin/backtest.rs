//! Full historical backtest runner for XAUUSD.
//!
//! Loads cached parquet bars, runs the MA-crossover simulator at a chosen lot
//! size, and prints a full performance report.
//!
//! Usage:
//!   cargo run -p xau-sim --bin xau-backtest -- --lots 0.10 --parquet data/cache/xauusd_m1.parquet

use std::path::Path;
use std::time::Instant;

use anyhow::Context;
use xau_data::read_bars_parquet;
use xau_sim::{run_simulation, CostModel, SimConfig};

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
    let parquet = arg("parquet").unwrap_or_else(|| "data/cache/xauusd_m1.parquet".into());
    let lots: f64 = arg("lots").and_then(|v| v.parse().ok()).unwrap_or(0.10);
    let initial: f64 = arg("deposit").and_then(|v| v.parse().ok()).unwrap_or(25_000.0);
    let sl: i64 = arg("sl").and_then(|v| v.parse().ok()).unwrap_or(100);
    let tp: i64 = arg("tp").and_then(|v| v.parse().ok()).unwrap_or(200);
    let spread: i64 = arg("spread").and_then(|v| v.parse().ok()).unwrap_or(20);
    let fast: usize = arg("fast").and_then(|v| v.parse().ok()).unwrap_or(20);
    let slow: usize = arg("slow").and_then(|v| v.parse().ok()).unwrap_or(50);
    let dd: f64 = arg("maxdd").and_then(|v| v.parse().ok()).unwrap_or(20.0);

    let path = Path::new(&parquet);
    if !path.exists() {
        anyhow::bail!("parquet not found: {}", path.display());
    }

    println!("Loading bars from {} ...", path.display());
    let t0 = Instant::now();
    let bars = read_bars_parquet(path).context("reading parquet")?;
    println!("Loaded {} bars in {:.2?}", bars.len(), t0.elapsed());
    if bars.is_empty() {
        anyhow::bail!("no bars loaded");
    }
    let first = &bars[0];
    let last = &bars[bars.len() - 1];
    println!(
        "Range: ts {} .. {} | price {:.2} .. {:.2}",
        first.ts,
        last.ts,
        bars.iter().map(|b| b.low).fold(f64::INFINITY, f64::min),
        bars.iter().map(|b| b.high).fold(f64::NEG_INFINITY, f64::max),
    );

    let config = SimConfig {
        initial_deposit: initial,
        max_drawdown_pct: dd,
        seed: 123,
        tick_replay: false,
        lots,
        sl_pts: sl,
        tp_pts: tp,
        fast_ma: fast,
        slow_ma: slow,
    };
    let cost = CostModel {
        spread_pts: spread,
        fixed_slippage_pts: 0,
        vol_scaled_slippage_pts: 0.0,
        commission_per_lot: 0.0,
        swap_pts_per_day: 0.0,
    };

    println!();
    println!("=== BACKTEST CONFIG ===");
    println!("  Lot size:        {:.2}", lots);
    println!("  Initial deposit: ${:.2}", initial);
    println!("  SL / TP:         {sl} pts / {tp} pts");
    println!("  Spread:          {spread} pts");
    println!("  Strategy:        MA crossover {fast} / {slow}");
    println!("  Max drawdown:    {dd:.0}%");
    println!("  $ per pt (this lot): ${:.2}", 100.0 * lots * 0.01);
    println!();

    let t1 = Instant::now();
    let report = run_simulation(&config, &bars, &cost, None)?;
    let elapsed = t1.elapsed();

    let wins = report.gross_profit;
    let losses = report.gross_loss.abs();
    let win_rate = if report.total_trades > 0 {
        report.winning_trades as f64 / report.total_trades as f64 * 100.0
    } else {
        0.0
    };

    let profit_factor = if losses > 0.0 {
        wins / losses
    } else if wins > 0.0 {
        f64::INFINITY
    } else {
        0.0
    };
    let ev = if report.total_trades > 0 {
        report.total_pnl / report.total_trades as f64
    } else {
        0.0
    };

    println!("=== RESULTS ===");
    println!("  Bars processed:    {}", bars.len());
    println!("  Trades:            {}", report.total_trades);
    println!(
        "  Wins / Losses:     {} / {}",
        report.winning_trades, report.losing_trades
    );
    println!("  Win rate:          {:.2}%", win_rate);
    println!("  Total PnL:         ${:.2}", report.total_pnl);
    println!("  Gross profit:      ${:.2}", report.gross_profit);
    println!("  Gross loss:        ${:.2}", report.gross_loss);
    println!("  Final equity:      ${:.2}", report.final_equity);
    println!("  Max drawdown:      ${:.2}", report.max_drawdown);
    println!("  Profit factor:     {:.3}", profit_factor);
    println!("  Sharpe (trade):    {:.3}", report.sharpe_ratio);
    println!("  EV per trade:      ${:.2}", ev);
    println!("  Return on deposit: {:.2}%", report.total_pnl / initial * 100.0);
    println!("  Compute time:      {:.2?}", elapsed);
    println!();

    // Verdict
    println!("=== VERDICT ===");
    if report.total_trades == 0 {
        println!("  ⚠️  No trades generated. Strategy produced no signals on this data.");
        println!("     Try shorter MAs (--fast 5 --slow 20) or different data.");
    } else if report.total_pnl > 0.0 {
        println!("  ✅ PROFITABLE: +${:.2} over {} trades.", report.total_pnl, report.total_trades);
        println!("     Profit factor {:.2} (>1.0 = edge).", profit_factor);
    } else {
        println!(
            "  ❌ NOT PROFITABLE: -${:.2} over {} trades.",
            report.total_pnl.abs(),
            report.total_trades
        );
        println!("     Profit factor {:.2} (<1.0 = no edge).", profit_factor);
    }

    Ok(())
}

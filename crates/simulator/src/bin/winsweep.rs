//! Does trading fewer, higher-quality breakouts raise the win rate without
//! destroying the edge?
//!
//! A longer Donchian window means price has to travel further to trigger, so
//! signals are rarer but stronger. That is the only *entry-side* lever that can
//! plausibly improve accuracy without capping winners (exit-side levers like a
//! take profit raise the win rate and lose money -- see `xau-tpsweep`).
//!
//! Judged on expectancy and profit factor, never on win rate alone.
//!
//! Usage:
//!   cargo run -p xau-sim --bin xau-winsweep -- --parquet data/cache_long/xauusd_h1.parquet

use std::path::Path;

use xau_data::read_bars_parquet;
use xau_sim::engine::{backtest, EngineConfig};
use xau_sim::strategy::{entry_signals, StratKind, StratParams};

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
    let bars = read_bars_parquet(Path::new(&parquet))?;

    let base = EngineConfig {
        initial_deposit: 25_000.0,
        lots: 0.10,
        atr_period: 14,
        sl_atr: 1.5,
        tp_atr: 0.0,
        trail_atr: 2.5,
        max_hold_bars: 240,
        exit_on_opposite: false,
        allowed_hours: Vec::new(),
        spread_pts: 20.0,
        commission_per_lot: 0.0,
        max_drawdown_pct: 100.0,
    };

    println!("{}  ({} bars)\n", parquet, bars.len());

    // ---- 1. breakout window -------------------------------------------------
    println!("LOOKBACK SWEEP (EMA300 trend gate, everything else unchanged)");
    println!(
        "{:<10}{:<9}{:<9}{:<9}{:<11}{:<9}{:<9}{:<10}",
        "lookback", "trades", "win%", "PF", "expectancy", "PnL", "maxDD%", "avgW/avgL"
    );
    println!("{}", "-".repeat(76));
    for lb in [20usize, 30, 40, 55, 70, 90, 120, 150] {
        let params = StratParams { lookback: lb, trend_ma: 300, ..Default::default() };
        let signals = entry_signals(&bars, StratKind::Donchian, &params);
        let m = &backtest(&bars, &signals, &base).metrics;
        println!(
            "{:<10}{:<9}{:<9.1}{:<9.2}{:<11.2}{:<9.0}{:<9.2}{:.2}/{:.2}",
            if lb == 55 { format!("{lb} *") } else { format!("{lb}") },
            m.trades,
            m.win_rate,
            m.profit_factor,
            m.expectancy,
            m.total_pnl,
            m.max_drawdown_pct,
            m.avg_win,
            m.avg_loss
        );
    }

    // ---- 2. trend gate ------------------------------------------------------
    println!("\nTREND GATE SWEEP (lookback 55)");
    println!(
        "{:<10}{:<9}{:<9}{:<9}{:<11}{:<9}{:<9}",
        "ema", "trades", "win%", "PF", "expectancy", "PnL", "maxDD%"
    );
    println!("{}", "-".repeat(66));
    for ma in [0usize, 50, 100, 200, 300, 400, 500] {
        // trend_ma = 0 is not valid for the EMA; use 2 (effectively always on).
        let gate = if ma == 0 { 2 } else { ma };
        let params = StratParams { lookback: 55, trend_ma: gate, ..Default::default() };
        let signals = entry_signals(&bars, StratKind::Donchian, &params);
        let m = &backtest(&bars, &signals, &base).metrics;
        println!(
            "{:<10}{:<9}{:<9.1}{:<9.2}{:<11.2}{:<9.0}{:<9.2}",
            if ma == 0 { "none".to_string() } else { ma.to_string() },
            m.trades,
            m.win_rate,
            m.profit_factor,
            m.expectancy,
            m.total_pnl,
            m.max_drawdown_pct
        );
    }
    Ok(())
}

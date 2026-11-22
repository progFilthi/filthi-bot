//! Does adding a take profit help D3? Sweep `tp_atr` against the trail.
//!
//! The live bot currently exits on the trail or the time clock only. This
//! answers, with the same engine the backtest uses, whether capping winners
//! at a fixed ATR multiple would have helped or hurt.
//!
//! Usage:
//!   cargo run -p xau-sim --bin xau-tpsweep -- --parquet data/cache_long/xauusd_h4.parquet

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
    let parquet = arg("parquet").unwrap_or_else(|| "data/cache_long/xauusd_h4.parquet".into());
    let bars = read_bars_parquet(Path::new(&parquet))?;
    let params = StratParams { lookback: 55, trend_ma: 300, ..Default::default() };
    let signals = entry_signals(&bars, StratKind::Donchian, &params);

    let base = EngineConfig {
        initial_deposit: 25_000.0,
        lots: 0.10,
        atr_period: 14,
        sl_atr: 1.5,
        tp_atr: 0.0,
        trail_atr: 2.5,
        max_hold_bars: 240,
        exit_on_opposite: false,
        flat_before_gap_min: None,
        allowed_hours: Vec::new(),
        spread_pts: 20.0,
        commission_per_lot: 0.0,
        max_drawdown_pct: 100.0,
    };

    println!("{}  ({} bars)", parquet, bars.len());
    println!(
        "{:<10}{:<12}{:<10}{:<10}{:<10}{:<9}{:<9}{:<8}",
        "tp_atr", "pnl", "ret%", "PF", "maxDD%", "trades", "win%", "tp_exits"
    );
    println!("{}", "-".repeat(78));

    for tp in [0.0, 1.5, 2.0, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0] {
        let cfg = EngineConfig { tp_atr: tp, ..base.clone() };
        let r = backtest(&bars, &signals, &cfg);
        let m = &r.metrics;
        println!(
            "{:<10}{:<12.0}{:<10.1}{:<10.2}{:<10.2}{:<9}{:<9.1}{:<8}",
            if tp == 0.0 { "none".to_string() } else { format!("{tp:.1}") },
            m.total_pnl,
            m.return_pct,
            m.profit_factor,
            m.max_drawdown_pct,
            m.trades,
            m.win_rate,
            m.tp_exits
        );
    }
    Ok(())
}

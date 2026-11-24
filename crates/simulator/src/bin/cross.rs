//! "Does the intraday D3 rule survive on another symbol?" — one row per market.
//!
//! Same rule as live: Donchian 55, SL 1.5 ATR, trail 2.5 ATR, EMA300 gate,
//! 240-bar cap, entries 07:00–18:00 UTC, flat before the daily break. The only
//! things that change per symbol are the contract maths: `--point` and
//! `--contract`. Gold is 0.01 / 100; a 5-digit FX pair is 0.00001 / 100,000.
//!
//! The point of the R-multiple column is sizing. A prop account is limited by
//! drawdown, not by trade count, so the number that matters is how deep the
//! worst run went in units of the average loss.
//!
//! Usage:
//!   cargo run --release -p xau-sim --bin xau-cross -- \
//!       --parquet data/cache_fx/eurusd_h1.parquet \
//!       --point 0.00001 --contract 100000 --spread-pts 3

use std::path::Path;

use anyhow::Context;

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

fn num(name: &str, default: f64) -> f64 {
    arg(name).and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn main() -> anyhow::Result<()> {
    let parquet = arg("parquet").context("--parquet is required")?;
    let path = Path::new(&parquet);
    if !path.exists() {
        anyhow::bail!("parquet not found: {}", path.display());
    }
    let mut bars = read_bars_parquet(path).context("reading parquet")?;
    // Compare symbols over the same recent window, not the same calendar file.
    if let Some(n) = arg("last-bars").and_then(|v| v.parse::<usize>().ok()) {
        if n > 0 && n < bars.len() {
            bars = bars[bars.len() - n..].to_vec();
        }
    }
    if bars.len() < 400 {
        anyhow::bail!("{} bars is too few to test a 300-period trend filter", bars.len());
    }

    let cfg = EngineConfig {
        initial_deposit: num("deposit", 25_000.0),
        lots: num("lots", 0.10),
        atr_period: 14,
        sl_atr: 1.5,
        tp_atr: 0.0,
        trail_atr: 2.5,
        max_hold_bars: 240,
        exit_on_opposite: false,
        allowed_hours: (7..=18).collect(),
        flat_before_gap_min: Some(90),
        spread_pts: num("spread-pts", 20.0),
        point: num("point", 0.01),
        contract: num("contract", 100.0),
        commission_per_lot: 0.0,
        max_drawdown_pct: 100.0,
    };

    let params = StratParams { lookback: 55, trend_ma: 300, ..Default::default() };
    let sig = entry_signals(&bars, StratKind::Donchian, &params);
    let rep = backtest(&bars, &sig, &cfg);
    let m = rep.metrics;

    let first = bars.first().map(|b| b.ts).unwrap_or(0);
    let last = bars.last().map(|b| b.ts).unwrap_or(0);
    let years = (last - first) as f64 / (365.25 * 86_400.0);

    println!("symbol   {parquet}");
    println!(
        "bars     {} over {:.1} years | point {} | contract {} | spread {} pts",
        bars.len(),
        years,
        cfg.point,
        cfg.contract,
        cfg.spread_pts
    );
    println!(
        "result   trades {} | PF {:.2} | win {:.1}% | net {:+.0} | maxDD {:.1}%",
        m.trades, m.profit_factor, m.win_rate, m.total_pnl, m.max_drawdown_pct
    );
    println!(
        "per trade avg win {:+.2} | avg loss {:+.2} | expectancy {:+.2} | streak {}",
        m.avg_win, m.avg_loss, m.expectancy, m.max_losing_streak
    );

    // Drawdown in R, where 1R is the average loss. This is the number that
    // decides whether the rule fits inside a fixed max-loss limit.
    let r = m.avg_loss.abs();
    if r > 0.0 {
        println!(
            "in R      maxDD {:.1}R | net {:+.1}R | {:.0} trades/year",
            m.max_drawdown / r,
            m.total_pnl / r,
            m.trades as f64 / years.max(0.1)
        );
    }

    let folds = num("folds", 5.0).max(2.0) as usize;
    let mut pnls = vec![0.0f64; folds];
    let mut counts = vec![0usize; folds];
    let span = bars.len().max(1) as f64;
    for t in &rep.trades {
        let idx = ((t.entry_bar as f64 / span) * folds as f64) as usize;
        let idx = idx.min(folds - 1);
        pnls[idx] += t.pnl;
        counts[idx] += 1;
    }
    print!("folds    ");
    for i in 0..folds {
        print!("f{} {:+.0} ({}t)  ", i + 1, pnls[i], counts[i]);
    }
    println!();
    let positive = pnls.iter().filter(|p| **p > 0.0).count();
    println!("         {positive}/{folds} folds positive");
    Ok(())
}

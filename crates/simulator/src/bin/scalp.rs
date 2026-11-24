//! "Can a small account scalp?" — a grid over signal family, stop, target and
//! hold, with the spread charged on every round trip.
//!
//! The point of the grid is not to find a setting that looks good. It is to
//! see whether *any* region of it survives when the spread is realistic and
//! the result is split into folds. One good cell out of ninety is noise; a
//! broad positive region is a result.
//!
//! Usage:
//!   cargo run --release -p xau-sim --bin xau-scalp -- \
//!       --parquet data/cache_fx/eurusd_m15.parquet \
//!       --point 0.00001 --contract 100000 --spread-pts 3

use std::path::Path;

use anyhow::Context;
use xau_core::Side;
use xau_data::read_bars_parquet;
use xau_sim::engine::{backtest, EngineConfig, ExitReason};
use xau_sim::orb::ymd_hms;
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
    let bars = read_bars_parquet(path).context("reading parquet")?;
    if bars.len() < 2_000 {
        anyhow::bail!("{} bars is too few for a scalping grid", bars.len());
    }

    let point = num("point", 0.01);
    let contract = num("contract", 100.0);
    let spread = num("spread-pts", 20.0);
    let deposit = num("deposit", 25_000.0);
    let lots = num("lots", 0.10);
    let min_trades = num("min-trades", 100.0) as usize;

    let kinds = [
        ("donchian", StratKind::Donchian),
        ("bollinger", StratKind::Bollinger),
        ("rsi", StratKind::RsiReversion),
        ("pullback", StratKind::TrendPullback),
        ("macross", StratKind::MaCross),
    ];

    println!(
        "{} | {} bars | point {} | contract {} | spread {} pts\n",
        parquet,
        bars.len(),
        point,
        contract,
        spread
    );

    // Stop distance in points decides everything about a scalper: the spread is
    // a fixed tax, so what matters is the tax as a share of the stop.
    let atrs = xau_sim::strategy::atr(&bars, 14);
    let mut sorted: Vec<f64> = atrs.iter().copied().filter(|a| a.is_finite()).collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let med = sorted.get(sorted.len() / 2).copied().unwrap_or(0.0);
    println!(
        "ATR14 median {:.5} ({:.0} pts) | stop 0.5 ATR = {:.0} pts | target 1.5 ATR = {:.0} pts | spread = {:.1}% of that stop\n",
        med,
        med / point,
        med * 0.5 / point,
        med * 1.5 / point,
        spread / (med * 0.5 / point) * 100.0
    );
    println!(
        "{:<10}{:>6}{:>6}{:>6}{:>7}{:>7}{:>7}{:>10}{:>8}{:>7}",
        "kind", "sl", "tp", "hold", "trades", "PF", "win%", "net", "maxDD%", "folds"
    );
    println!("{}", "-".repeat(80));

    let mut rows: Vec<(f64, String)> = Vec::new();

    // ---- single-config mode: walk a real balance -------------------------
    // `--account 50` answers the only question that matters for a small live
    // account: what would this actually have done to the balance?
    if let Some(start) = arg("account").and_then(|v| v.parse::<f64>().ok()) {
        let kind = StratKind::parse(&arg("kind").unwrap_or_else(|| "bollinger".into()))
            .context("unknown --kind")?;
        let sl = num("sl", 0.5);
        let tp = num("tp", 1.5);
        let hold = num("hold", 8.0) as usize;
        let live_lots = num("live-lots", 0.01);
        let cfg = EngineConfig {
            initial_deposit: deposit,
            lots,
            atr_period: 14,
            sl_atr: sl,
            tp_atr: tp,
            trail_atr: 0.0,
            max_hold_bars: hold,
            exit_on_opposite: false,
            allowed_hours: (7..=18).collect(),
            flat_before_gap_min: Some(90),
            spread_pts: spread,
            point,
            contract,
            commission_per_lot: 0.0,
            max_drawdown_pct: 100.0,
        };
        let sig = entry_signals(&bars, kind, &StratParams::default());
        // Direction split: an edge that only exists one way in a pair that
        // trended one way over the sample is drift, not mean reversion.
        let mut sig = sig;
        if let Some(side) = arg("side") {
            let keep = if side.to_ascii_lowercase().starts_with('b') {
                Side::Buy
            } else {
                Side::Sell
            };
            for s in sig.iter_mut() {
                if *s != Some(keep) {
                    *s = None;
                }
            }
        }
        let rep = backtest(&bars, &sig, &cfg);
        let m = rep.metrics;
        let scale = live_lots / cfg.lots;

        println!(
            "{:?} sl {} tp {} hold {} | trades {} | PF {:.2} | win {:.1}% | avg hold {:.1} bars",
            kind, sl, tp, hold, m.trades, m.profit_factor, m.win_rate, m.avg_bars_held
        );
        println!(
            "exits    SL {} | TP {} | time {} | flat {} | other {}",
            m.sl_exits,
            m.tp_exits,
            rep.trades.iter().filter(|t| t.reason == ExitReason::TimeExit).count(),
            rep.trades.iter().filter(|t| t.reason == ExitReason::SessionClose).count(),
            m.other_exits
        );
        println!(
            "per trade at {:.2} lots: avg win {:+.2} | avg loss {:+.2} | expectancy {:+.2} | worst {:+.2} | max losing streak {}",
            lots,
            m.avg_win,
            m.avg_loss,
            m.expectancy,
            rep.trades.iter().map(|t| t.pnl).fold(0.0f64, f64::min),
            m.max_losing_streak
        );

        let mut balance = start;
        let mut peak = start;
        let mut worst_dd = 0.0f64;
        let mut worst_balance = start;
        let mut killed_at = None;
        for (i, t) in rep.trades.iter().enumerate() {
            balance += t.pnl * scale;
            peak = peak.max(balance);
            worst_balance = worst_balance.min(balance);
            worst_dd = worst_dd.max((peak - balance) / peak);
            if balance <= 0.0 {
                let (y, mo, d, h, mi) = ymd_hms(bars[t.entry_bar].ts);
                killed_at = Some((i + 1, format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}")));
                break;
            }
        }
        println!(
            "\n${start:.0} account at {live_lots} lots: {}",
            match killed_at {
                Some((n, when)) => format!("wiped on trade {n} ({when} UTC)"),
                None => format!("ends at ${balance:.2} ({:+.0}%)", (balance / start - 1.0) * 100.0),
            }
        );
        println!(
            "         worst balance ${worst_balance:.2} | deepest drawdown {:.0}% | worst single trade {:+.2}",
            worst_dd * 100.0,
            rep.trades.iter().map(|t| t.pnl * scale).fold(0.0f64, f64::min)
        );
        print!("         first 12 trades:");
        let mut b = start;
        for t in rep.trades.iter().take(12) {
            b += t.pnl * scale;
            print!(" {:+.2}→{:.2}", t.pnl * scale, b);
        }
        println!();
        return Ok(());
    }

    for (name, kind) in kinds {
        let params = StratParams::default();
        let sig = entry_signals(&bars, kind, &params);
        let mut best: Option<(f64, String)> = None;
        for sl in [0.5f64, 1.0, 1.5] {
            for tp in [0.5f64, 1.0, 1.5] {
                for hold in [4usize, 8, 16] {
                    let cfg = EngineConfig {
                        initial_deposit: deposit,
                        lots,
                        atr_period: 14,
                        sl_atr: sl,
                        tp_atr: tp,
                        trail_atr: 0.0,
                        max_hold_bars: hold,
                        exit_on_opposite: false,
                        allowed_hours: (7..=18).collect(),
                        flat_before_gap_min: Some(90),
                        spread_pts: spread,
                        point,
                        contract,
                        commission_per_lot: 0.0,
                        max_drawdown_pct: 100.0,
                    };
                    let rep = backtest(&bars, &sig, &cfg);
                    let m = rep.metrics;
                    if m.trades < min_trades {
                        continue;
                    }
                    // Folds: a setting is only interesting if it is not one
                    // lucky stretch.
                    let folds = 4usize;
                    let mut pnls = vec![0.0f64; folds];
                    let span = bars.len().max(1) as f64;
                    for t in &rep.trades {
                        let i = ((t.entry_bar as f64 / span) * folds as f64) as usize;
                        pnls[i.min(folds - 1)] += t.pnl;
                    }
                    let pos = pnls.iter().filter(|p| **p > 0.0).count();
                    let line = format!(
                        "{:<10}{:>6.1}{:>6.1}{:>6}{:>7}{:>7.2}{:>7.1}{:>10.0}{:>8.1}{:>7}",
                        name, sl, tp, hold, m.trades, m.profit_factor, m.win_rate,
                        m.total_pnl, m.max_drawdown_pct, format!("{pos}/{folds}")
                    );
                    if best.as_ref().map_or(true, |b| m.profit_factor > b.0) {
                        best = Some((m.profit_factor, line.clone()));
                    }
                    if m.profit_factor > 1.05 {
                        rows.push((m.profit_factor, line));
                    }
                }
            }
        }
        if let Some((_, line)) = best {
            println!("{line}");
        }
    }

    rows.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    if rows.is_empty() {
        println!("\nno setting in the grid reached PF 1.05");
    } else {
        println!("\nevery cell above PF 1.05 ({} of {}):", rows.len(), 5 * 27);
        for (_, line) in rows.iter().take(15) {
            println!("{line}");
        }
    }
    Ok(())
}

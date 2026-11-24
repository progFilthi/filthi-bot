//! Robustness validation: multi-fold testing + parameter sensitivity.
//!
//! A strategy is only believable if it survives several independent time
//! slices and a neighbourhood of nearby parameter values. This tool tests
//! exactly that, plus cost sensitivity.
//!
//! Usage:
//!   cargo run -p xau-sim --bin xau-validate -- --parquet data/cache/xauusd_m1.parquet

use std::path::Path;

use anyhow::Context;
use xau_core::{Bar, Side};
use xau_data::read_bars_parquet;
use xau_sim::engine::{backtest, EngineConfig, Metrics, CONTRACT, POINT};
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

fn cfg_base() -> EngineConfig {
    EngineConfig {
        initial_deposit: 25_000.0,
        lots: 0.10,
        atr_period: 14,
        sl_atr: 2.0,
        tp_atr: 4.0,
        trail_atr: 0.0,
        max_hold_bars: 0,
        exit_on_opposite: true,
        flat_before_gap_min: None,
        allowed_hours: Vec::new(),
        spread_pts: 20.0,
        point: POINT,
        contract: CONTRACT,
        commission_per_lot: 0.0,
        max_drawdown_pct: 100.0,
    }
}

/// The two configurations that were profitable in BOTH halves of the
/// walk-forward test, plus close neighbours.
fn shortlist() -> Vec<(String, StratKind, StratParams, EngineConfig)> {
    let liquid = vec![13u32, 14, 15, 16];
    let mut out = Vec::new();

    // --- Candidate A: 20/50 MA cross, US session only ---
    let mut c = cfg_base();
    c.allowed_hours = liquid.clone();
    out.push((
        "A  ma20/50 liquid".into(),
        StratKind::MaCross,
        StratParams { fast: 20, slow: 50, ..Default::default() },
        c.clone(),
    ));

    // --- Candidate A2: same, with a trailing stop instead of fixed target ---
    let mut c2 = cfg_base();
    c2.allowed_hours = liquid.clone();
    c2.tp_atr = 0.0;
    c2.trail_atr = 3.0;
    out.push((
        "A2 ma20/50 liquid trail3".into(),
        StratKind::MaCross,
        StratParams { fast: 20, slow: 50, ..Default::default() },
        c2,
    ));

    // --- Candidate B: Bollinger fade, wide band, time exit ---
    let mut c3 = cfg_base();
    c3.sl_atr = 3.0;
    c3.tp_atr = 0.0;
    c3.max_hold_bars = 120;
    out.push((
        "B  boll k3 sl3 hold120".into(),
        StratKind::Bollinger,
        StratParams { lookback: 20, band_k: 3.0, ..Default::default() },
        c3,
    ));

    // --- Candidate B2: same with trailing stop ---
    let mut c4 = cfg_base();
    c4.sl_atr = 3.0;
    c4.tp_atr = 0.0;
    c4.trail_atr = 2.0;
    c4.max_hold_bars = 120;
    out.push((
        "B2 boll k3 sl3 trail2 hold120".into(),
        StratKind::Bollinger,
        StratParams { lookback: 20, band_k: 3.0, ..Default::default() },
        c4,
    ));

    // --- Candidate D3: refined Donchian (EMA300 + 2.5 ATR trail) ---
    let mut c6 = cfg_base();
    c6.sl_atr = 1.5;
    c6.tp_atr = 0.0;
    c6.trail_atr = 2.5;
    c6.max_hold_bars = 240;
    c6.exit_on_opposite = false;
    out.push((
        "D3 donchian55 trail2.5 ema300".into(),
        StratKind::Donchian,
        StratParams { lookback: 55, trend_ma: 300, ..Default::default() },
        c6,
    ));

    // --- Candidate C: Donchian long-horizon with trail (best OOS trend) ---
    let mut c5 = cfg_base();
    c5.sl_atr = 1.5;
    c5.tp_atr = 0.0;
    c5.trail_atr = 2.0;
    c5.max_hold_bars = 240;
    out.push((
        "C  donchian55 trail2 hold240".into(),
        StratKind::Donchian,
        StratParams { lookback: 55, trend_ma: 200, ..Default::default() },
        c5,
    ));

    out
}

fn run(bars: &[Bar], sig: &[Option<Side>], cfg: &EngineConfig) -> Metrics {
    backtest(bars, sig, cfg).metrics
}

fn main() -> anyhow::Result<()> {
    let parquet = arg("parquet").unwrap_or_else(|| "data/cache/xauusd_m1.parquet".into());
    let folds: usize = arg("folds").and_then(|v| v.parse().ok()).unwrap_or(5);
    let path = Path::new(&parquet);
    if !path.exists() {
        anyhow::bail!("parquet not found: {}", path.display());
    }
    let bars = read_bars_parquet(path).context("reading parquet")?;
    let n = bars.len();
    let fold_size = n / folds;

    println!("=== ROBUSTNESS VALIDATION ===");
    println!("Bars: {} | Folds: {} | Bars per fold: ~{}", n, folds, fold_size);
    println!("Lots: 0.10 | Deposit: $25,000 | Spread: 20 pts\n");

    let list = shortlist();
    let sigs: Vec<Vec<Option<Side>>> = list
        .iter()
        .map(|(_, k, p, _)| entry_signals(&bars, *k, p))
        .collect();

    // ---- Fold-by-fold PnL matrix ----
    println!("=== PER-FOLD PnL (each fold is an independent time slice) ===");
    print!("{:<30}", "strategy");
    for f in 0..folds {
        print!("{:>11}", format!("fold{}", f + 1));
    }
    println!("{:>11}{:>8}", "TOTAL", "pos/f");
    println!("{}", "-".repeat(30 + 11 * folds + 19));

    let mut summary: Vec<(String, f64, usize, usize, f64)> = Vec::new();
    for (ci, (label, _, _, cfg)) in list.iter().enumerate() {
        let sig = &sigs[ci];
        print!("{:<30}", label);
        let mut total = 0.0;
        let mut positive = 0;
        let mut total_trades = 0;
        let mut worst_dd = 0.0f64;
        for f in 0..folds {
            let start = f * fold_size;
            let end = if f == folds - 1 { n } else { (f + 1) * fold_size };
            let m = run(&bars[start..end], &sig[start..end], cfg);
            total += m.total_pnl;
            total_trades += m.trades;
            if m.total_pnl > 0.0 {
                positive += 1;
            }
            worst_dd = worst_dd.max(m.max_drawdown_pct);
            print!("{:>11.2}", m.total_pnl);
        }
        println!(
            "{:>11.2}{:>5}/{}{:>8.1}%",
            total, positive, folds, worst_dd
        );
        summary.push((label.clone(), total, positive, total_trades, worst_dd));
    }

    // ---- Parameter sensitivity for the MA candidate ----
    println!("\n=== PARAMETER SENSITIVITY: MA cross, US session only ===");
    println!("(a real edge should hold across a neighbourhood, not one lucky pair)");
    println!(
        "{:<10}{:>10}{:>10}{:>10}{:>10}",
        "slow\\fast", "fast=10", "fast=15", "fast=20", "fast=30"
    );
    for slow in [40usize, 50, 60, 100] {
        print!("{:<10}", format!("slow={}", slow));
        for fast in [10usize, 15, 20, 30] {
            let mut cfg = cfg_base();
            cfg.allowed_hours = vec![13, 14, 15, 16];
            let p = StratParams { fast, slow, ..Default::default() };
            let sig = entry_signals(&bars, StratKind::MaCross, &p);
            let m = run(&bars, &sig, &cfg);
            print!("{:>10.0}", m.total_pnl);
        }
        println!();
    }

    // ---- Session window sensitivity ----
    println!("\n=== SESSION WINDOW SENSITIVITY: MA 20/50 ===");
    let windows: Vec<(&str, Vec<u32>)> = vec![
        ("all hours", vec![]),
        ("12-16 UTC", vec![12, 13, 14, 15, 16]),
        ("13-16 UTC", vec![13, 14, 15, 16]),
        ("13-17 UTC", vec![13, 14, 15, 16, 17]),
        ("14-17 UTC", vec![14, 15, 16, 17]),
        ("08-12 UTC", vec![8, 9, 10, 11, 12]),
    ];
    let p = StratParams { fast: 20, slow: 50, ..Default::default() };
    let sig = entry_signals(&bars, StratKind::MaCross, &p);
    for (name, hours) in windows {
        let mut cfg = cfg_base();
        cfg.allowed_hours = hours;
        let m = run(&bars, &sig, &cfg);
        println!(
            "  {:<12} trades {:>5}  pnl {:>10.2}  PF {:>5.2}  win {:>5.1}%  ret {:>6.2}%",
            name, m.trades, m.total_pnl, m.profit_factor, m.win_rate, m.return_pct
        );
    }

    // ---- Bollinger sensitivity ----
    println!("\n=== PARAMETER SENSITIVITY: Bollinger fade ===");
    println!("{:<10}{:>12}{:>12}{:>12}", "hold\\k", "k=2.75", "k=3.00", "k=3.25");
    for hold in [60usize, 120, 240] {
        print!("{:<10}", format!("hold={}", hold));
        for k in [2.75f64, 3.0, 3.25] {
            let mut cfg = cfg_base();
            cfg.sl_atr = 3.0;
            cfg.tp_atr = 0.0;
            cfg.max_hold_bars = hold;
            let p = StratParams { lookback: 20, band_k: k, ..Default::default() };
            let sig = entry_signals(&bars, StratKind::Bollinger, &p);
            let m = run(&bars, &sig, &cfg);
            print!("{:>12.0}", m.total_pnl);
        }
        println!();
    }

    // ---- Donchian sensitivity: the leading family on long history ----
    println!("\n=== PARAMETER SENSITIVITY: Donchian breakout (SL 1.5 ATR, hold 240, EMA200) ===");
    println!("total PnL over the full sample; a real edge should fill the grid, not one cell");
    print!("{:<14}", "trail\\lookback");
    for lb in [20usize, 40, 55, 80, 100] {
        print!("{:>10}", format!("lb{}", lb));
    }
    println!();
    for trail in [2.0f64, 2.5, 3.0] {
        print!("{:<14}", format!("trail={}", trail));
        for lb in [20usize, 40, 55, 80, 100] {
            let mut cfg = cfg_base();
            cfg.sl_atr = 1.5;
            cfg.tp_atr = 0.0;
            cfg.trail_atr = trail;
            cfg.max_hold_bars = 240;
            cfg.exit_on_opposite = false;
            let p = StratParams { lookback: lb, trend_ma: 200, ..Default::default() };
            let sig = entry_signals(&bars, StratKind::Donchian, &p);
            let m = run(&bars, &sig, &cfg);
            print!("{:>10.0}", m.total_pnl);
        }
        println!();
    }

    // ---- Trend filter: is the EMA200 gate actually helping? ----
    println!("\n=== TREND FILTER ABLATION: Donchian 55, trail 2, hold 240 ===");
    for (name, trend) in [("no filter", 1usize), ("EMA100", 100), ("EMA200", 200), ("EMA300", 300)] {
        let mut cfg = cfg_base();
        cfg.sl_atr = 1.5;
        cfg.tp_atr = 0.0;
        cfg.trail_atr = 2.0;
        cfg.max_hold_bars = 240;
        cfg.exit_on_opposite = false;
        let p = StratParams { lookback: 55, trend_ma: trend, ..Default::default() };
        let sig = entry_signals(&bars, StratKind::Donchian, &p);
        let m = run(&bars, &sig, &cfg);
        println!(
            "  {:<10} trades {:>5}  pnl {:>11.2}  PF {:>5.2}  win {:>5.1}%  maxDD {:>5.1}%",
            name, m.trades, m.total_pnl, m.profit_factor, m.win_rate, m.max_drawdown_pct
        );
    }

    // ---- Cost sensitivity on the best candidate ----
    println!("\n=== COST SENSITIVITY (does the edge survive a worse broker?) ===");
    for (i, (label, _, _, cfg0)) in list.iter().enumerate() {
        print!("{:<30}", label);
        for spread in [15.0f64, 20.0, 25.0, 30.0, 40.0] {
            let mut cfg = cfg0.clone();
            cfg.spread_pts = spread;
            let m = run(&bars, &sigs[i], &cfg);
            print!("  {:>4.0}pts:{:>9.0}", spread, m.total_pnl);
        }
        println!();
    }

    // ---- Verdict ----
    println!("\n=== VERDICT ===");
    let mut any_robust = false;
    for (label, total, positive, trades, dd) in &summary {
        let robust = *positive >= (folds as f64 * 0.6) as usize && *total > 0.0 && *trades >= 50;
        if robust {
            any_robust = true;
            println!(
                "  ROBUST: {:<30} total {:>9.2} | {}/{} folds positive | {} trades | worst DD {:.1}%",
                label, total, positive, folds, trades, dd
            );
        } else {
            println!(
                "  fragile: {:<29} total {:>9.2} | {}/{} folds positive | {} trades",
                label, total, positive, folds, trades
            );
        }
    }
    if !any_robust {
        println!("\n  No configuration is robust across time slices. Do not trade this live.");
    }

    Ok(())
}

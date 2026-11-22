//! "Can D3 be turned into a day-trading rule?" — measured, not asserted.
//!
//! Takes the validated D3 configuration unchanged (Donchian 55, SL 1.5 ATR,
//! trail 2.5 ATR, EMA300 trend gate, 240-bar cap) and re-runs it with
//! progressively shorter maximum holds. On H1 a 4-bar cap means no trade may
//! last longer than four hours, which is what "hours, not days" means in
//! practice.
//!
//! It also runs a session-restricted variant: entries only inside the
//! London/NY window, with the same caps. That is the closest thing to
//! "trade the day and be flat by the evening" the engine can express without
//! changing the rule.
//!
//! Usage:
//!   cargo run --release -p xau-sim --bin xau-daytrade -- \
//!       --parquet data/cache_long/xauusd_h1.parquet

use std::path::Path;

use anyhow::Context;
use xau_core::{Bar, Side};
use xau_data::read_bars_parquet;
use xau_sim::engine::{backtest, EngineConfig, Metrics};
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

/// The validated D3 rule, byte for byte.
fn d3_config() -> EngineConfig {
    EngineConfig {
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
    }
}

fn run(bars: &[Bar], sig: &[Option<Side>], cfg: &EngineConfig) -> Metrics {
    backtest(bars, sig, cfg).metrics
}

fn main() -> anyhow::Result<()> {
    let parquet = arg("parquet").unwrap_or_else(|| "data/cache_long/xauusd_h1.parquet".into());
    let path = Path::new(&parquet);
    if !path.exists() {
        anyhow::bail!("parquet not found: {}", path.display());
    }
    let bars = read_bars_parquet(path).context("reading parquet")?;
    let params = StratParams { lookback: 55, trend_ma: 300, ..Default::default() };
    let sig = entry_signals(&bars, StratKind::Donchian, &params);

    println!("D3 as a day-trading rule");
    println!(
        "bars {} | lots 0.10 | deposit $25,000 | spread 20 pts | same entries, only the exit changes\n",
        bars.len()
    );

    println!("{:<14}{:>8}{:>10}{:>8}{:>7}{:>12}{:>9}", "max hold", "hours", "trades", "PF", "win%", "net", "maxDD%");
    println!("{}", "-".repeat(68));

    let control = {
        let cfg = d3_config();
        run(&bars, &sig, &cfg)
    };

    for hold in [2usize, 4, 6, 8, 12, 16, 24, 48, 240] {
        let mut cfg = d3_config();
        cfg.max_hold_bars = hold;
        let m = run(&bars, &sig, &cfg);
        let label = if hold == 240 { "240 (rule)".to_string() } else { format!("{hold} bars") };
        println!(
            "{:<14}{:>8}{:>10}{:>8.2}{:>7.1}{:>12.0}{:>9.2}",
            label,
            if hold == 240 { "—".to_string() } else { hold.to_string() },
            m.trades,
            m.profit_factor,
            m.win_rate,
            m.total_pnl,
            m.max_drawdown_pct
        );
    }

    println!("\nentries restricted to 07:00–16:00 UTC (London + New York), same exits:");
    println!("{:<14}{:>8}{:>10}{:>8}{:>7}{:>12}{:>9}", "max hold", "hours", "trades", "PF", "win%", "net", "maxDD%");
    println!("{}", "-".repeat(68));
    for hold in [4usize, 8, 12, 240] {
        let mut cfg = d3_config();
        cfg.max_hold_bars = hold;
        cfg.allowed_hours = (7..=15).collect();
        let m = run(&bars, &sig, &cfg);
        println!(
            "{:<14}{:>8}{:>10}{:>8.2}{:>7.1}{:>12.0}{:>9.2}",
            if hold == 240 { "240 (rule)" } else { "cap" },
            if hold == 240 { "—".to_string() } else { hold.to_string() },
            m.trades,
            m.profit_factor,
            m.win_rate,
            m.total_pnl,
            m.max_drawdown_pct
        );
    }

    println!("\nflat at the last bar before the daily break, so nothing is carried overnight:");
    println!(
        "{:<22}{:>8}{:>8}{:>7}{:>12}{:>9}{:>9}{:>9}",
        "entries / flat", "trades", "PF", "win%", "net", "maxDD%", "avg hld", "max hld"
    );
    println!("{}", "-".repeat(84));
    let flat_rows: [(&str, Vec<u32>); 4] = [
        ("all hours", Vec::new()),
        ("07–18 UTC", (7..=18).collect()),
        ("07–16 UTC", (7..=16).collect()),
        ("13–16 UTC", (13..=16).collect()),
    ];
    for (label, hours) in flat_rows {
        let mut cfg = d3_config();
        cfg.allowed_hours = hours;
        cfg.flat_before_gap_min = Some(90);
        let rep = backtest(&bars, &sig, &cfg);
        let m = rep.metrics;
        let max_hold = rep.trades.iter().map(|t| t.bars_held).max().unwrap_or(0);
        println!(
            "{:<22}{:>8}{:>8.2}{:>7.1}{:>12.0}{:>9.2}{:>9.1}{:>9}",
            label,
            m.trades,
            m.profit_factor,
            m.win_rate,
            m.total_pnl,
            m.max_drawdown_pct,
            m.avg_bars_held,
            max_hold
        );
    }

    println!("\nsplit in half by trade entry (does the day version survive both halves?)");
    println!("{:<26}{:>10}{:>12}{:>10}{:>12}", "variant", "trades", "first half", "trades", "second half");
    println!("{}", "-".repeat(70));
    let variants: Vec<(&str, Box<dyn Fn(&mut EngineConfig)>)> = vec![
        ("rule (240 bars)", Box::new(|c: &mut EngineConfig| { c.max_hold_bars = 240; })),
        ("cap 4h", Box::new(|c: &mut EngineConfig| { c.max_hold_bars = 4; })),
        ("cap 12h", Box::new(|c: &mut EngineConfig| { c.max_hold_bars = 12; })),
        ("flat at day close", Box::new(|c: &mut EngineConfig| { c.flat_before_gap_min = Some(90); })),
        ("07-16 + day close", Box::new(|c: &mut EngineConfig| {
            c.allowed_hours = (7..=16).collect();
            c.flat_before_gap_min = Some(90);
        })),
    ];
    let mid = bars.len() / 2;
    for (label, tweak) in variants {
        let mut cfg = d3_config();
        tweak(&mut cfg);
        let rep = backtest(&bars, &sig, &cfg);
        let mut a = 0.0;
        let mut b = 0.0;
        let mut na = 0usize;
        let mut nb = 0usize;
        for t in &rep.trades {
            if t.entry_bar < mid {
                a += t.pnl;
                na += 1;
            } else {
                b += t.pnl;
                nb += 1;
            }
        }
        println!("{:<26}{:>10}{:>12.0}{:>10}{:>12.0}", label, na + nb, a, nb, b);
    }

    println!("\nfive equal time slices (a rule that only works in one slice is not a rule):");
    let fold_variants: Vec<(&str, Box<dyn Fn(&mut EngineConfig)>)> = vec![
        ("rule (240 bars)", Box::new(|c: &mut EngineConfig| { c.max_hold_bars = 240; })),
        ("day close", Box::new(|c: &mut EngineConfig| { c.flat_before_gap_min = Some(90); })),
        ("07-18 + day close", Box::new(|c: &mut EngineConfig| {
            c.allowed_hours = (7..=18).collect();
            c.flat_before_gap_min = Some(90);
        })),
        ("07-16 + day close", Box::new(|c: &mut EngineConfig| {
            c.allowed_hours = (7..=16).collect();
            c.flat_before_gap_min = Some(90);
        })),
    ];
    let folds = 5usize;
    let fold_size = bars.len() / folds;
    print!("{:<22}", "variant");
    for f in 0..folds {
        print!("{:>10}", format!("f{}", f + 1));
    }
    println!("{:>10}", "positive");
    for (label, tweak) in fold_variants {
        let mut cfg = d3_config();
        tweak(&mut cfg);
        let rep = backtest(&bars, &sig, &cfg);
        let mut sums = vec![0.0f64; folds];
        for t in &rep.trades {
            let f = (t.entry_bar / fold_size).min(folds - 1);
            sums[f] += t.pnl;
        }
        let pos = sums.iter().filter(|v| **v > 0.0).count();
        print!("{:<22}", label);
        for v in &sums {
            print!("{:>10.0}", v);
        }
        println!("{:>7}/{}", pos, folds);
    }

    println!("\nwalk-forward: pick the entry window on earlier folds, trade the next");
    let grid: [(u32, u32); 6] = [(6, 17), (7, 16), (7, 18), (8, 19), (9, 20), (13, 18)];
    let pnl_by_fold = |from: u32, to: u32| -> Vec<f64> {
        let mut cfg = d3_config();
        cfg.allowed_hours = (from..=to).collect();
        cfg.flat_before_gap_min = Some(90);
        let rep = backtest(&bars, &sig, &cfg);
        let mut sums = vec![0.0f64; folds];
        for t in &rep.trades {
            let f = (t.entry_bar / fold_size).min(folds - 1);
            sums[f] += t.pnl;
        }
        sums
    };
    let grid_pnl: Vec<Vec<f64>> = grid.iter().map(|(a, b)| pnl_by_fold(*a, *b)).collect();
    let mut oos_total = 0.0;
    let mut oos_folds = 0usize;
    for k in 1..folds {
        let best = (0..grid.len())
            .max_by(|a, b| {
                let sa: f64 = grid_pnl[*a][..k].iter().sum();
                let sb: f64 = grid_pnl[*b][..k].iter().sum();
                sa.partial_cmp(&sb).unwrap()
            })
            .unwrap();
        let (f, t) = grid[best];
        let got = grid_pnl[best][k];
        oos_total += got;
        if got > 0.0 {
            oos_folds += 1;
        }
        println!(
            "  test fold {}: picked {:02}–{:02} UTC on folds 1–{} → {:+8.0}",
            k + 1, f, t, k, got
        );
    }
    println!(
        "  out-of-sample total {:+0.0} across {} folds, {} positive",
        oos_total,
        folds - 1,
        oos_folds
    );

    // ---- Scalping: the same signal, tiny stops and targets ----
    let m15 = arg("m15").unwrap_or_else(|| "data/cache_long/xauusd_m15.parquet".into());
    let m15_path = Path::new(&m15);
    if m15_path.exists() {
        let mb = read_bars_parquet(m15_path).context("reading M15 parquet")?;
        let msig = entry_signals(&mb, StratKind::Donchian, &params);
        println!(
            "\n--- scalping the same signal on M15 ({} bars) ---",
            mb.len()
        );
        println!("a tight stop/target is the only thing that makes a trade \"small\"");
        println!(
            "{:<22}{:>8}{:>8}{:>8}{:>7}{:>10}{:>10}{:>9}",
            "stop / target", "hold", "trades", "PF", "win%", "net@20pts", "net@40pts", "avg hold"
        );
        println!("{}", "-".repeat(84));
        for (sl, tp, hold) in [
            (1.0f64, 1.0f64, 8usize),
            (0.5, 0.5, 4),
            (0.5, 1.0, 8),
            (0.3, 0.3, 2),
        ] {
            let mut cfg = d3_config();
            cfg.sl_atr = sl;
            cfg.tp_atr = tp;
            cfg.trail_atr = 0.0;
            cfg.max_hold_bars = hold;
            let m20 = run(&mb, &msig, &cfg);
            cfg.spread_pts = 40.0;
            let m40 = run(&mb, &msig, &cfg);
            println!(
                "{:<22}{:>8}{:>8}{:>8.2}{:>7.1}{:>10.0}{:>10.0}{:>9.1}",
                format!("{sl:.1} / {tp:.1} ATR"),
                hold,
                m20.trades,
                m20.profit_factor,
                m20.win_rate,
                m20.total_pnl,
                m40.total_pnl,
                m20.avg_bars_held
            );
        }
    }

    let kept = |h: usize| {
        let mut cfg = d3_config();
        cfg.max_hold_bars = h;
        run(&bars, &sig, &cfg).total_pnl / control.total_pnl * 100.0
    };
    println!("\nshare of the rule's net kept, by cap:");
    for h in [4usize, 8, 12, 24] {
        println!("  {h:>2}h cap: {:>6.1}% of +${:.0}", kept(h), control.total_pnl);
    }

    Ok(())
}

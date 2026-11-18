//! Final performance report for the candidate strategies on long history.
//!
//! Beyond headline PnL this checks the things that separate an edge from luck:
//!   * yearly consistency (not one lucky year)
//!   * long vs short symmetry
//!   * trade concentration
//!   * cost sensitivity
//!   * out-of-sample slice with locked parameters
//!
//! Usage:
//!   cargo run -p xau-sim --bin xau-report -- --parquet data/cache_long/xauusd_h1.parquet

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context;
use xau_core::{Bar, Side};
use xau_data::read_bars_parquet;
use xau_sim::engine::{backtest, EngineConfig, Report};
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

fn base_cfg() -> EngineConfig {
    EngineConfig {
        initial_deposit: 25_000.0,
        lots: 0.10,
        atr_period: 14,
        sl_atr: 1.5,
        tp_atr: 0.0,
        trail_atr: 2.0,
        max_hold_bars: 240,
        exit_on_opposite: false,
        allowed_hours: Vec::new(),
        spread_pts: 20.0,
        commission_per_lot: 0.0,
        max_drawdown_pct: 100.0,
    }
}

struct Spec {
    label: &'static str,
    kind: StratKind,
    params: StratParams,
    cfg: EngineConfig,
}

fn specs() -> Vec<Spec> {
    // D: the leading trend-following candidate.
    let d_cfg = base_cfg();
    // D2: a shorter breakout window, for comparison.
    let d2_cfg = base_cfg();
    // A: the M1 winner carried over, US session only, fixed 2:1.
    let mut a_cfg = EngineConfig {
        sl_atr: 2.0,
        tp_atr: 4.0,
        trail_atr: 0.0,
        max_hold_bars: 0,
        exit_on_opposite: true,
        allowed_hours: vec![13, 14, 15, 16, 17],
        ..base_cfg()
    };
    a_cfg.exit_on_opposite = true;

    vec![
        Spec {
            label: "D   Donchian 55, SL 1.5 ATR, trail 2 ATR, 240-bar exit, EMA200 filter",
            kind: StratKind::Donchian,
            params: StratParams { lookback: 55, trend_ma: 200, ..Default::default() },
            cfg: d_cfg,
        },
        Spec {
            label: "D2  Donchian 20, SL 1.5 ATR, trail 2 ATR, 240-bar exit, EMA200 filter",
            kind: StratKind::Donchian,
            params: StratParams { lookback: 20, trend_ma: 200, ..Default::default() },
            cfg: d2_cfg,
        },
        Spec {
            label: "D3  Donchian 55, SL 1.5 ATR, trail 2.5 ATR, 240-bar exit, EMA300 filter",
            kind: StratKind::Donchian,
            params: StratParams { lookback: 55, trend_ma: 300, ..Default::default() },
            cfg: {
                let mut c = base_cfg();
                c.trail_atr = 2.5;
                c
            },
        },
        Spec {
            label: "A   MA 20/50, US session 13-17 UTC, 2:1 R:R (previous M1 winner)",
            kind: StratKind::MaCross,
            params: StratParams { fast: 20, slow: 50, ..Default::default() },
            cfg: a_cfg,
        },
    ]
}

/// Civil year from a Unix timestamp.
fn year_of(ts: i64) -> i32 {
    let z = ts.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 { (y + 1) as i32 } else { y as i32 }
}

fn print_report(title: &str, bars: &[Bar], rep: &Report, initial: f64) {
    let m = &rep.metrics;
    println!("\n{}", "=".repeat(84));
    println!("{}", title);
    println!("{}", "=".repeat(84));
    println!(
        "Trades {:>6} | Wins {:>5} / Losses {:>5} | Win rate {:>6.2}%",
        m.trades, m.wins, m.losses, m.win_rate
    );
    println!(
        "Total PnL ${:>12.2} | Return {:>7.2}% | Final equity ${:.2}",
        m.total_pnl, m.return_pct, m.final_equity
    );
    println!(
        "Profit factor {:>5.2} | Expectancy ${:>7.2}/trade | Avg win ${:.2} / avg loss ${:.2}",
        m.profit_factor, m.expectancy, m.avg_win, m.avg_loss
    );
    println!(
        "Max drawdown ${:.2} ({:.2}%) | Sharpe {:.2} | Avg hold {:.0} bars | Exposure {:.1}%",
        m.max_drawdown, m.max_drawdown_pct, m.sharpe, m.avg_bars_held, m.exposure_pct
    );
    println!(
        "Max losing streak {} | Exits: SL {} | TP {} | TRAIL {} | other {}",
        m.max_losing_streak, m.sl_exits, m.tp_exits, m.trail_exits, m.other_exits
    );

    let mut long_pnl = 0.0;
    let mut long_n = 0usize;
    let mut short_pnl = 0.0;
    let mut short_n = 0usize;
    for t in &rep.trades {
        match t.side {
            Side::Buy => {
                long_pnl += t.pnl;
                long_n += 1;
            }
            Side::Sell => {
                short_pnl += t.pnl;
                short_n += 1;
            }
        }
    }
    println!(
        "Long  {:>5} trades {:>12.2} ({:>7.2}/trade) | Short {:>5} trades {:>12.2} ({:>7.2}/trade)",
        long_n,
        long_pnl,
        if long_n > 0 { long_pnl / long_n as f64 } else { 0.0 },
        short_n,
        short_pnl,
        if short_n > 0 { short_pnl / short_n as f64 } else { 0.0 }
    );

    let mut wins: Vec<f64> = rep.trades.iter().map(|t| t.pnl).filter(|p| *p > 0.0).collect();
    wins.sort_by(|a, b| b.partial_cmp(a).unwrap());
    let gross: f64 = wins.iter().sum();
    if gross > 0.0 && !wins.is_empty() {
        let top1 = wins[0] / gross * 100.0;
        let top5: f64 = wins.iter().take(5).sum::<f64>() / gross * 100.0;
        println!(
            "Profit concentration: best trade {:.1}% of gross profit, top 5 {:.1}%",
            top1, top5
        );
    }

    // ---- Yearly consistency ----
    let mut yearly: BTreeMap<i32, (f64, usize)> = BTreeMap::new();
    for t in &rep.trades {
        let key = year_of(bars[t.exit_bar.min(bars.len() - 1)].ts);
        let e = yearly.entry(key).or_insert((0.0, 0));
        e.0 += t.pnl;
        e.1 += 1;
    }
    if !yearly.is_empty() {
        println!("\nYearly PnL:");
        print!("  ");
        let mut pos = 0;
        let mut total = 0;
        for (y, (p, n)) in &yearly {
            print!("{y}:{p:>8.0}({n:>3})  ");
            total += 1;
            if *p > 0.0 {
                pos += 1;
            }
            if total % 4 == 0 {
                print!("\n  ");
            }
        }
        println!();
        println!("  {} of {} years profitable", pos, total);
    }

    let peak = rep.equity_curve.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    println!(
        "Equity: ${:.0} -> ${:.0} | peak ${:.0}",
        initial,
        rep.equity_curve.last().copied().unwrap_or(initial),
        peak
    );
}

/// Run a signal list restricted to one side, to expose directional dependence.
fn side_only(sig: &[Option<Side>], keep: Side) -> Vec<Option<Side>> {
    sig.iter()
        .map(|s| if *s == Some(keep) { *s } else { None })
        .collect()
}

fn main() -> anyhow::Result<()> {
    let parquet = arg("parquet").unwrap_or_else(|| "data/cache_long/xauusd_h1.parquet".into());
    let split_pct: f64 = arg("split").and_then(|v| v.parse().ok()).unwrap_or(70.0);
    let path = Path::new(&parquet);
    if !path.exists() {
        anyhow::bail!("parquet not found: {}", path.display());
    }
    let bars = read_bars_parquet(path).context("reading parquet")?;
    let n = bars.len();
    let split = (n as f64 * split_pct / 100.0) as usize;

    println!("XAUUSD STRATEGY REPORT");
    println!(
        "{} bars | lots 0.10 | deposit $25,000 | spread 20 pts | OOS split {:.0}/{:.0}",
        n,
        split_pct,
        100.0 - split_pct
    );
    println!(
        "Span: {} -> {} ({} years)",
        year_of(bars[0].ts),
        year_of(bars[n - 1].ts),
        year_of(bars[n - 1].ts) - year_of(bars[0].ts)
    );

    for s in specs() {
        let sig = entry_signals(&bars, s.kind, &s.params);

        let full = backtest(&bars, &sig, &s.cfg);
        print_report(&format!("{}  [FULL PERIOD]", s.label), &bars, &full, 25_000.0);

        // Directional check on the full period.
        let long_rep = backtest(&bars, &side_only(&sig, Side::Buy), &s.cfg);
        let short_rep = backtest(&bars, &side_only(&sig, Side::Sell), &s.cfg);
        println!(
            "\n  Dir-only full period: long ${:.2} ({} trades, PF {:.2}) | short ${:.2} ({} trades, PF {:.2})",
            long_rep.metrics.total_pnl,
            long_rep.metrics.trades,
            long_rep.metrics.profit_factor,
            short_rep.metrics.total_pnl,
            short_rep.metrics.trades,
            short_rep.metrics.profit_factor
        );

        // Locked out-of-sample.
        let oos = backtest(&bars[split..], &sig[split..], &s.cfg);
        print_report(
            &format!("{}  [OUT-OF-SAMPLE, last {:.0}%]", s.label, 100.0 - split_pct),
            &bars[split..],
            &oos,
            25_000.0,
        );

        println!("\nCost stress (spread pts -> total PnL, full period):");
        for spread in [10.0f64, 15.0, 20.0, 25.0, 30.0, 40.0, 60.0] {
            let mut cfg = s.cfg.clone();
            cfg.spread_pts = spread;
            let r = backtest(&bars, &sig, &cfg);
            println!(
                "  {:>4.0} pts: {:>12.2}  PF {:.2}",
                spread, r.metrics.total_pnl, r.metrics.profit_factor
            );
        }
    }

    Ok(())
}

//! Final performance report for the two validated configurations.
//!
//! Goes beyond headline PnL: checks monthly consistency, trade concentration
//! (is the edge one lucky outlier?), long vs short asymmetry, and out-of-sample
//! behaviour with a locked, never-tuned parameter set.
//!
//! Usage:
//!   cargo run -p xau-sim --bin xau-report -- --parquet data/cache/xauusd_m1.parquet

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
        sl_atr: 2.0,
        tp_atr: 4.0,
        trail_atr: 0.0,
        max_hold_bars: 0,
        exit_on_opposite: true,
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
    // A: 20/50 MA cross, entry only during the 13:00-17:00 UTC window.
    let mut a = base_cfg();
    a.allowed_hours = vec![13, 14, 15, 16, 17];

    // B: Bollinger fade, 3 sigma, 3-ATR stop, 120-bar time exit.
    let mut b = base_cfg();
    b.sl_atr = 3.0;
    b.tp_atr = 0.0;
    b.max_hold_bars = 120;

    vec![
        Spec {
            label: "A  MA 20/50, US session (13-17 UTC)",
            kind: StratKind::MaCross,
            params: StratParams { fast: 20, slow: 50, ..Default::default() },
            cfg: a,
        },
        Spec {
            label: "B  Bollinger 3.0 sigma fade, 3-ATR stop, 120-bar exit",
            kind: StratKind::Bollinger,
            params: StratParams { lookback: 20, band_k: 3.0, ..Default::default() },
            cfg: b,
        },
    ]
}

fn ym(ts: i64) -> String {
    // Days since epoch -> civil date (Howard Hinnant's algorithm).
    let z = ts.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}", y, m)
}

fn print_report(title: &str, bars: &[Bar], rep: &Report, initial: f64) {
    let m = &rep.metrics;
    println!("\n{}", "=".repeat(78));
    println!("{}", title);
    println!("{}", "=".repeat(78));
    println!("Trades:            {}", m.trades);
    println!("Wins / Losses:     {} / {}", m.wins, m.losses);
    println!("Win rate:          {:.2}%", m.win_rate);
    println!("Total PnL:         ${:.2}", m.total_pnl);
    println!("Return:            {:.2}%", m.return_pct);
    println!("Profit factor:     {:.3}", m.profit_factor);
    println!("Expectancy:        ${:.2}/trade", m.expectancy);
    println!("Avg win:           ${:.2}", m.avg_win);
    println!("Avg loss:          ${:.2}", m.avg_loss);
    println!("Max drawdown:      ${:.2} ({:.2}%)", m.max_drawdown, m.max_drawdown_pct);
    println!("Sharpe (annual):   {:.2}", m.sharpe);
    println!("Avg bars held:     {:.0}", m.avg_bars_held);
    println!("Exposure:          {:.1}%", m.exposure_pct);
    println!("Max losing streak: {}", m.max_losing_streak);
    println!(
        "Exits:             SL {} | TP {} | TRAIL {} | other {}",
        m.sl_exits, m.tp_exits, m.trail_exits, m.other_exits
    );

    // ---- Long vs short asymmetry ----
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
        "Long side:         {} trades, ${:.2} ({:.2}/trade)",
        long_n,
        long_pnl,
        if long_n > 0 { long_pnl / long_n as f64 } else { 0.0 }
    );
    println!(
        "Short side:        {} trades, ${:.2} ({:.2}/trade)",
        short_n,
        short_pnl,
        if short_n > 0 { short_pnl / short_n as f64 } else { 0.0 }
    );

    // ---- Trade concentration ----
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
        if top5 > 60.0 {
            println!("  !! WARNING: profit is concentrated in a handful of trades.");
        }
    }

    // ---- Monthly consistency ----
    let mut monthly: BTreeMap<String, (f64, usize)> = BTreeMap::new();
    for t in &rep.trades {
        let key = ym(bars[t.exit_bar.min(bars.len() - 1)].ts);
        let e = monthly.entry(key).or_insert((0.0, 0));
        e.0 += t.pnl;
        e.1 += 1;
    }
    println!("\nMonthly breakdown:");
    println!("  {:<9}{:>8}{:>12}", "month", "trades", "pnl$");
    let mut pos_months = 0;
    let mut total_months = 0;
    for (k, (p, n)) in &monthly {
        println!("  {:<9}{:>8}{:>12.2}", k, n, p);
        total_months += 1;
        if *p > 0.0 {
            pos_months += 1;
        }
    }
    println!(
        "  {} of {} months profitable",
        pos_months, total_months
    );

    // ---- Equity curve sanity ----
    let peak = rep.equity_curve.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    println!(
        "\nEquity: start ${:.0} -> end ${:.0} | peak ${:.0}",
        initial,
        rep.equity_curve.last().copied().unwrap_or(initial),
        peak
    );
}

/// Split the data at the highest close and test both regimes separately.
/// A directional bet will work on one leg and fail on the other; a real edge
/// should not collapse when the trend reverses.
fn regime_split(label: &str, bars: &[Bar], sig: &[Option<Side>], cfg: &EngineConfig) {
    let mut peak_idx = 0usize;
    let mut peak_px = f64::NEG_INFINITY;
    for (i, b) in bars.iter().enumerate() {
        if b.close > peak_px {
            peak_px = b.close;
            peak_idx = i;
        }
    }
    println!("\n{}", "-".repeat(78));
    println!("REGIME SPLIT: {}", label);
    println!(
        "  price peak ${:.2} at bar {} ({}), start ${:.2}, end ${:.2}",
        peak_px,
        peak_idx,
        ym(bars[peak_idx].ts),
        bars[0].close,
        bars[bars.len() - 1].close
    );
    println!(
        "  leg 1 = rising ({} bars), leg 2 = falling ({} bars)",
        peak_idx,
        bars.len() - peak_idx
    );

    let legs: [(&str, usize, usize); 2] = [
        ("rising", 0, peak_idx),
        ("falling", peak_idx, bars.len()),
    ];

    println!(
        "  {:<9}{:>8}{:>12}{:>8}{:>10}{:>10}",
        "leg", "trades", "pnl$", "PF", "long$", "short$"
    );
    for (name, s, e) in legs {
        if e <= s + 10 {
            continue;
        }
        let all = backtest(&bars[s..e], &sig[s..e], cfg);
        let long_sig: Vec<Option<Side>> = sig[s..e]
            .iter()
            .map(|x| if *x == Some(Side::Buy) { *x } else { None })
            .collect();
        let short_sig: Vec<Option<Side>> = sig[s..e]
            .iter()
            .map(|x| if *x == Some(Side::Sell) { *x } else { None })
            .collect();
        let l = backtest(&bars[s..e], &long_sig, cfg);
        let sh = backtest(&bars[s..e], &short_sig, cfg);
        println!(
            "  {:<9}{:>8}{:>12.2}{:>8.2}{:>10.2}{:>10.2}",
            name,
            all.metrics.trades,
            all.metrics.total_pnl,
            all.metrics.profit_factor,
            l.metrics.total_pnl,
            sh.metrics.total_pnl
        );
    }
}

fn main() -> anyhow::Result<()> {
    let parquet = arg("parquet").unwrap_or_else(|| "data/cache/xauusd_m1.parquet".into());
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
        "{} bars | lots 0.10 | deposit $25,000 | spread 20 pts | split {:.0}/{:.0}",
        n,
        split_pct,
        100.0 - split_pct
    );

    for s in specs() {
        let sig = entry_signals(&bars, s.kind, &s.params);

        // Full period.
        let full = backtest(&bars, &sig, &s.cfg);
        print_report(&format!("{}  [FULL PERIOD]", s.label), &bars, &full, 25_000.0);

        // Locked out-of-sample: parameters were chosen on the first slice and
        // are applied to the second, untouched.
        let oos = backtest(&bars[split..], &sig[split..], &s.cfg);
        print_report(
            &format!("{}  [OUT-OF-SAMPLE, last {:.0}%]", s.label, 100.0 - split_pct),
            &bars[split..],
            &oos,
            25_000.0,
        );

        // Cost stress: what happens with a worse broker?
        println!("\nCost stress (spread pts -> total PnL, full period):");
        for spread in [15.0f64, 20.0, 25.0, 30.0, 40.0, 50.0] {
            let mut cfg = s.cfg.clone();
            cfg.spread_pts = spread;
            let r = backtest(&bars, &sig, &cfg);
            println!(
                "  {:>4.0} pts: {:>10.2}  PF {:.2}",
                spread, r.metrics.total_pnl, r.metrics.profit_factor
            );
        }

        regime_split(s.label, &bars, &sig, &s.cfg);
    }

    Ok(())
}

//! Deep profit-and-loss breakdown for the selected strategy.
//!
//! `xau-report` answers "how much did it make". This answers "where did the
//! money come from and where did it go": per-exit-reason, per-hour,
//! per-month, per-holding-time and per-volatility-regime decompositions,
//! plus every drawdown episode with its dates and recovery time.
//!
//! Usage:
//!   cargo run -p xau-sim --bin xau-breakdown -- --parquet data/cache_long/xauusd_h1.parquet

use std::path::Path;

use anyhow::Context;
use xau_core::{Bar, Side};
use xau_data::read_bars_parquet;
use xau_sim::engine::{backtest, EngineConfig, ExitReason, Report, Trade, CONTRACT};
use xau_sim::strategy::{atr, entry_signals, StratKind, StratParams};

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

/// UTC civil time from a Unix timestamp (Howard Hinnant's algorithm).
fn civil(ts: i64) -> (i32, u32, u32, u32) {
    let days = ts.div_euclid(86_400);
    let secs = ts.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 {
        y += 1;
    }
    (y as i32, m as u32, d as u32, (secs / 3_600) as u32)
}

fn short_date(ts: i64) -> String {
    let (y, m, d, _) = civil(ts);
    format!("{y:04}-{m:02}-{d:02}")
}

fn weekday(ts: i64) -> &'static str {
    // 1970-01-01 was a Thursday.
    let idx = (ts.div_euclid(86_400) + 4).rem_euclid(7) as usize;
    ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][idx]
}

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

fn bar_ts(bars: &[Bar], i: usize) -> i64 {
    bars[i.min(bars.len() - 1)].ts
}

/// A peak-to-recovery drawdown episode, in equity-curve indices.
struct Episode {
    peak_idx: usize,
    trough_idx: usize,
    recover_idx: usize,
    depth: f64,
    pct: f64,
}

fn pnl_by<F, K>(trades: &[Trade], key: &F) -> Vec<(K, (usize, f64))>
where
    F: Fn(&Trade) -> K,
    K: Ord,
{
    use std::collections::BTreeMap;
    let mut m: BTreeMap<K, (usize, f64)> = BTreeMap::new();
    for t in trades {
        let e = m.entry(key(t)).or_insert((0, 0.0));
        e.0 += 1;
        e.1 += t.pnl;
    }
    m.into_iter().collect()
}

fn print_grouped_str<F, K>(title: &str, trades: &[Trade], key: F)
where
    F: Fn(&Trade) -> K,
    K: Ord + std::fmt::Display,
{
    let rows = pnl_by(trades, &key);
    let total = trades.iter().map(|t| t.pnl).sum::<f64>();
    let gross_profit: f64 = trades.iter().filter(|t| t.pnl > 0.0).map(|t| t.pnl).sum();
    println!("\n=== {title} ===");
    println!(
        "{:<14} {:>6} {:>10} {:>9} {:>8} {:>9} {:>9}",
        "bucket", "trades", "total $", "avg $", "win%", "% of net", "% of gp"
    );
    for (k, (n, pnl)) in rows {
        let label = k.to_string();
        let wins = trades
            .iter()
            .filter(|t| key(t).to_string() == label)
            .filter(|t| t.pnl > 0.0)
            .count();
        let win_rate = if n > 0 { wins as f64 / n as f64 * 100.0 } else { 0.0 };
        let gp = trades
            .iter()
            .filter(|t| key(t).to_string() == label && t.pnl > 0.0)
            .map(|t| t.pnl)
            .sum::<f64>();
        println!(
            "{:<14} {:>6} {:>10.1} {:>9.2} {:>7.1}% {:>8.1}% {:>8.1}%",
            label,
            n,
            pnl,
            pnl / n as f64,
            win_rate,
            if total != 0.0 { pnl / total * 100.0 } else { 0.0 },
            if gross_profit != 0.0 { gp / gross_profit * 100.0 } else { 0.0 }
        );
    }
}

fn main() -> anyhow::Result<()> {
    let parquet = arg("parquet").unwrap_or_else(|| "data/cache_long/xauusd_h1.parquet".into());
    let path = Path::new(&parquet);
    if !path.exists() {
        anyhow::bail!("parquet not found: {}", path.display());
    }
    let bars = read_bars_parquet(path).context("reading parquet")?;
    let cfg = d3_config();
    let params = StratParams { lookback: 55, trend_ma: 300, ..Default::default() };
    let sig = entry_signals(&bars, StratKind::Donchian, &params);
    let rep: Report = backtest(&bars, &sig, &cfg);
    let m = &rep.metrics;

    println!("{}", "=".repeat(90));
    println!("D3  Donchian 55 | SL 1.5 ATR | trail 2.5 ATR | 240-bar exit | EMA300");
    println!("{}", "=".repeat(90));
    let (y0, _, _, _) = civil(bars[0].ts);
    let (y1, _, _, _) = civil(bars[bars.len() - 1].ts);
    println!(
        "{} bars | {} -> {} ({} years) | 0.10 lot | $25,000 | 20 pt spread",
        bars.len(),
        short_date(bars[0].ts),
        short_date(bars[bars.len() - 1].ts),
        y1 - y0
    );
    println!(
        "Net ${:.2} ({:+.1}%) | PF {:.2} | {} trades | win {:.1}% | maxDD {:.2}% | Sharpe {:.2}",
        m.total_pnl, m.return_pct, m.profit_factor, m.trades, m.win_rate, m.max_drawdown_pct, m.sharpe
    );
    println!(
        "Gross profit ${:.0} | gross loss ${:.0} | avg win ${:.2} | avg loss ${:.2}",
        m.gross_profit, m.gross_loss.abs(), m.avg_win, m.avg_loss
    );

    // ---------------------------------------------------------------
    // 1. Where the money comes from: exit reason
    // ---------------------------------------------------------------
    print_grouped_str("PROFIT BY EXIT TYPE", &rep.trades, |t| t.reason.label());
    println!("\n  Reading: an initial-stop exit is the strategy being wrong (by design,");
    println!("  small and quick). A trailed-stop exit is the strategy being right and");
    println!("  giving some profit back. Time exits are trades that went nowhere.");

    // ---------------------------------------------------------------
    // 2. Session / hour of entry
    // ---------------------------------------------------------------
    print_grouped_str("PROFIT BY ENTRY HOUR (UTC)", &rep.trades, |t| {
        format!("{:02}:00", civil(bar_ts(&bars, t.entry_bar)).3)
    });

    // ---------------------------------------------------------------
    // 3. Day of week
    // ---------------------------------------------------------------
    print_grouped_str("PROFIT BY ENTRY WEEKDAY", &rep.trades, |t| {
        weekday(bar_ts(&bars, t.entry_bar))
    });

    // ---------------------------------------------------------------
    // 4. Holding time
    // ---------------------------------------------------------------
    print_grouped_str("PROFIT BY HOLDING TIME (bars)", &rep.trades, |t| match t.bars_held {
        0..=3 => "1-3",
        4..=8 => "4-8",
        9..=20 => "9-20",
        21..=60 => "21-60",
        61..=120 => "61-120",
        _ => "121-240",
    });

    // ---------------------------------------------------------------
    // 5. Volatility regime at entry
    // ---------------------------------------------------------------
    let atr_series = atr(&bars, cfg.atr_period);
    let mut atrs: Vec<f64> = rep
        .trades
        .iter()
        .map(|t| atr_series[t.entry_bar.min(atr_series.len() - 1)])
        .filter(|v| v.is_finite())
        .collect();
    atrs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let q1 = atrs[atrs.len() / 3];
    let q2 = atrs[atrs.len() * 2 / 3];
    print_grouped_str("PROFIT BY VOLATILITY AT ENTRY (ATR14)", &rep.trades, |t| {
        let a = atr_series[t.entry_bar.min(atr_series.len() - 1)];
        if !a.is_finite() {
            "n/a"
        } else if a <= q1 {
            "low"
        } else if a <= q2 {
            "mid"
        } else {
            "high"
        }
    });

    // ---------------------------------------------------------------
    // 6. PnL distribution
    // ---------------------------------------------------------------
    print_grouped_str("TRADE PnL DISTRIBUTION", &rep.trades, |t| match t.pnl {
        p if p <= -200.0 => "< -200",
        p if p <= -100.0 => "-200..-100",
        p if p <= -50.0 => "-100..-50",
        p if p < 0.0 => "-50..0",
        p if p < 50.0 => "0..50",
        p if p < 100.0 => "50..100",
        p if p < 200.0 => "100..200",
        p if p < 400.0 => "200..400",
        _ => "400+",
    });

    // ---------------------------------------------------------------
    // 7. Monthly grid
    // ---------------------------------------------------------------
    println!("\n=== MONTHLY PnL ($) ===");
    let mut monthly: std::collections::BTreeMap<(i32, u32), (f64, usize)> =
        std::collections::BTreeMap::new();
    for t in &rep.trades {
        let (y, mo, _, _) = civil(bar_ts(&bars, t.exit_bar));
        let e = monthly.entry((y, mo)).or_insert((0.0, 0));
        e.0 += t.pnl;
        e.1 += 1;
    }
    print!("  year ");
    for mo in 1..=12 {
        print!("{:>7}", mo);
    }
    println!("{:>9}", "total");
    let years: Vec<i32> = {
        let mut v: Vec<i32> = monthly.keys().map(|(y, _)| *y).collect();
        v.dedup();
        v
    };
    let mut pos_months = 0;
    let mut tot_months = 0;
    for y in years {
        print!("  {y} ");
        for mo in 1..=12 {
            match monthly.get(&(y, mo)) {
                Some((p, _)) => {
                    print!("{:>7.0}", p);
                    tot_months += 1;
                    if *p > 0.0 {
                        pos_months += 1;
                    }
                }
                None => print!("{:>7}", "-"),
            }
        }
        println!();
    }
    println!("  {pos_months} of {tot_months} months profitable");

    // ---------------------------------------------------------------
    // 8. Drawdown episodes
    // ---------------------------------------------------------------
    let eq = &rep.equity_curve;
    let mut episodes: Vec<Episode> = Vec::new();
    let mut peak_val = eq[0];
    let mut peak_idx = 0usize;
    let mut trough_val = eq[0];
    let mut trough_idx = 0usize;
    let mut in_dd = false;
    for (i, &v) in eq.iter().enumerate().skip(1) {
        if v >= peak_val {
            if in_dd {
                episodes.push(Episode {
                    peak_idx,
                    trough_idx,
                    recover_idx: i,
                    depth: peak_val - trough_val,
                    pct: (peak_val - trough_val) / peak_val * 100.0,
                });
                in_dd = false;
            }
            peak_val = v;
            peak_idx = i;
            trough_val = v;
            trough_idx = i;
        } else {
            in_dd = true;
            if v < trough_val {
                trough_val = v;
                trough_idx = i;
            }
        }
    }
    if in_dd {
        episodes.push(Episode {
            peak_idx,
            trough_idx,
            recover_idx: eq.len() - 1,
            depth: peak_val - trough_val,
            pct: (peak_val - trough_val) / peak_val * 100.0,
        });
    }
    episodes.sort_by(|a, b| b.depth.partial_cmp(&a.depth).unwrap());

    println!("\n=== DRAWDOWNS (top 6 by depth) ===");
    println!(
        "{:<12} {:>10} {:>7} {:>12} {:>12} {:>10}",
        "start", "depth $", "depth%", "trough", "recovered", "underwater"
    );
    for e in episodes.iter().take(6) {
        let start_ts = bar_ts(&bars, e.peak_idx.saturating_sub(1));
        let trough_ts = bar_ts(&bars, e.trough_idx.saturating_sub(1));
        let rec_ts = bar_ts(&bars, e.recover_idx.saturating_sub(1));
        let days = (rec_ts - start_ts) as f64 / 86_400.0;
        println!(
            "{:<12} {:>10.0} {:>6.1}% {:>12} {:>12} {:>8.0} d",
            short_date(start_ts),
            e.depth,
            e.pct,
            short_date(trough_ts),
            short_date(rec_ts),
            days
        );
    }
    let longest = episodes
        .iter()
        .map(|e| {
            let s = bar_ts(&bars, e.peak_idx.saturating_sub(1));
            let r = bar_ts(&bars, e.recover_idx.saturating_sub(1));
            (r - s) as f64 / 86_400.0
        })
        .fold(0.0f64, f64::max);
    println!("  Longest time underwater: {longest:.0} days");

    // ---------------------------------------------------------------
    // 9. Losing streaks in calendar time
    // ---------------------------------------------------------------
    let mut streak = 0usize;
    let mut best_streak = 0usize;
    let mut best_span: (i64, i64) = (0, 0);
    let mut cur_start = 0i64;
    for t in &rep.trades {
        if t.pnl < 0.0 {
            if streak == 0 {
                cur_start = bar_ts(&bars, t.entry_bar);
            }
            streak += 1;
            if streak > best_streak {
                best_streak = streak;
                best_span = (cur_start, bar_ts(&bars, t.exit_bar));
            }
        } else {
            streak = 0;
        }
    }
    println!(
        "\nLongest losing streak: {} trades ({} -> {}, {:.0} days)",
        best_streak,
        short_date(best_span.0),
        short_date(best_span.1),
        (best_span.1 - best_span.0) as f64 / 86_400.0
    );

    // ---------------------------------------------------------------
    // 10. Loss anatomy
    // ---------------------------------------------------------------
    let losses: Vec<&Trade> = rep.trades.iter().filter(|t| t.pnl < 0.0).collect();
    let gross_loss: f64 = losses.iter().map(|t| t.pnl).sum();
    let mut by_reason: std::collections::BTreeMap<&'static str, (usize, f64)> =
        std::collections::BTreeMap::new();
    for t in &losses {
        let e = by_reason.entry(t.reason.label()).or_insert((0, 0.0));
        e.0 += 1;
        e.1 += t.pnl;
    }
    println!("\n=== LOSS ANATOMY ({} losing trades, ${:.0} total) ===", losses.len(), gross_loss.abs());
    println!("{:<12} {:>6} {:>12} {:>8} {:>10}", "exit", "trades", "total $", "avg $", "% of losses");
    for (label, (n, pnl)) in by_reason {
        println!(
            "{:<12} {:>6} {:>12.0} {:>8.2} {:>9.1}%",
            label,
            n,
            pnl,
            pnl / n as f64,
            pnl / gross_loss * 100.0
        );
    }

    // ---------------------------------------------------------------
    // 11. Risk-adjusted (R-multiple) view
    // ---------------------------------------------------------------
    // Dollars are not comparable across this sample: gold went from $1,094 to
    // $3,774 and ATR from ~$5 to ~$60, so a fixed 0.10 lot risks far more in
    // 2026 than in 2010. Normalising each trade by its planned risk
    // (1.5 x ATR at entry) shows the actual edge and whether stops are honoured.
    let risk_of = |t: &Trade| -> f64 {
        let a = atr_series[t.entry_bar.min(atr_series.len() - 1)];
        cfg.sl_atr * a * CONTRACT * cfg.lots
    };
    let r_of = |t: &Trade| -> f64 {
        let r = risk_of(t);
        if r > 0.0 {
            t.pnl / r
        } else {
            f64::NAN
        }
    };
    let mut rs: Vec<f64> = rep.trades.iter().map(&r_of).filter(|v| v.is_finite()).collect();
    rs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let risks: Vec<f64> = rep.trades.iter().map(&risk_of).filter(|v| *v > 0.0).collect();
    let mean_risk = risks.iter().sum::<f64>() / risks.len() as f64;
    let mean_r = rs.iter().sum::<f64>() / rs.len() as f64;
    let worst_r = rs[0];
    let best_r = rs[rs.len() - 1];
    let gap15 = rs.iter().filter(|v| **v < -1.5).count();
    let gap20 = rs.iter().filter(|v| **v < -2.0).count();
    println!("\n=== RISK-ADJUSTED (R = PnL / planned risk) ===");
    println!(
        "  Planned risk 1.5 x ATR at entry: mean ${:.2}, range ${:.2}-${:.2}",
        mean_risk,
        risks.iter().cloned().fold(f64::INFINITY, f64::min),
        risks.iter().cloned().fold(0.0, f64::max)
    );
    println!(
        "  Mean R {:.2} | median R {:.2} | worst R {:.2} | best R {:.2}",
        mean_r,
        rs[rs.len() / 2],
        worst_r,
        best_r
    );
    println!(
        "  Worse than -1.5R (stop jumped): {} trades ({:.1}%) | worse than -2R: {} ({:.1}%)",
        gap15,
        gap15 as f64 / rs.len() as f64 * 100.0,
        gap20,
        gap20 as f64 / rs.len() as f64 * 100.0
    );
    println!("  NOTE: the engine fills every stop at the stop price, so it cannot");
    println!("  produce a gap-through-stop. Real gaps will exceed these figures.");

    // Planned risk per entry year: a fixed 0.10 lot risks wildly different
    // dollars as gold's volatility changes, so recent years dominate both PnL
    // and risk unless sizing is volatility-normalised.
    let mut risk_by_year: std::collections::BTreeMap<i32, (usize, f64, f64)> =
        std::collections::BTreeMap::new();
    for t in &rep.trades {
        let (y, _, _, _) = civil(bar_ts(&bars, t.entry_bar));
        let e = risk_by_year.entry(y).or_insert((0, 0.0, 0.0));
        e.0 += 1;
        e.1 += risk_of(t);
        e.2 += t.pnl;
    }
    println!("\n  Mean planned risk and PnL by entry year:");
    println!(
        "    {:<6} {:>7} {:>12} {:>10} {:>9}",
        "year", "trades", "mean risk $", "PnL $", "R/trade"
    );
    for (y, (n, risk, pnl)) in &risk_by_year {
        println!(
            "    {:<6} {:>7} {:>12.2} {:>10.0} {:>+9.3}",
            y,
            n,
            risk / *n as f64,
            pnl,
            if *risk > 0.0 { pnl / risk } else { 0.0 }
        );
    }

    // Worst and best individual trades, normalised by risk.
    let mut sorted: Vec<&Trade> = rep.trades.iter().collect();
    sorted.sort_by(|a, b| r_of(a).partial_cmp(&r_of(b)).unwrap());
    println!("\n  Worst trades (by R):");
    for t in sorted.iter().take(5) {
        println!(
            "    {} {}  {} -> {}  ${:>9.2}  {:>6.2}R  {} bars",
            short_date(bar_ts(&bars, t.entry_bar)),
            if t.side == Side::Buy { "buy " } else { "sell" },
            t.entry,
            t.exit,
            t.pnl,
            r_of(t),
            t.bars_held
        );
    }
    println!("  Best trades (by R):");
    for t in sorted.iter().rev().take(5) {
        println!(
            "    {} {}  {} -> {}  ${:>9.2}  {:>6.2}R  {} bars",
            short_date(bar_ts(&bars, t.entry_bar)),
            if t.side == Side::Buy { "buy " } else { "sell" },
            t.entry,
            t.exit,
            t.pnl,
            r_of(t),
            t.bars_held
        );
    }

    // ---------------------------------------------------------------
    // 12. Swap / rollover drag — the one cost the engine does not model
    // ---------------------------------------------------------------
    // The broker charges swap at server midnight (00:00 EET/EEST, i.e. 22:00
    // or 23:00 UTC) on any position held through it, triple on Wednesday
    // (swap_rollover3day_dow = 3). All PnL above is GROSS of swap; this
    // section estimates the drag. Using UTC midnight mis-dates at most one
    // rollover for trades opened or closed within ~2h of it.
    let dollars_per_point = cfg.lots * CONTRACT * 0.01;
    let mut swap_total = 0.0f64;
    let mut rollovers = 0usize;
    let mut trades_with_swap = 0usize;
    let mut swap_long = 0.0f64;
    let mut swap_short = 0.0f64;
    for t in &rep.trades {
        let t0 = bar_ts(&bars, t.entry_bar);
        let t1 = bar_ts(&bars, t.exit_bar);
        let pts_per_day = if t.side == Side::Buy { -12.6 } else { -4.6 };
        let mut m = (t0.div_euclid(86_400) + 1) * 86_400;
        let mut cost = 0.0;
        while m < t1 {
            let dow = (m.div_euclid(86_400) + 4).rem_euclid(7);
            let mult = if dow == 3 { 3.0 } else { 1.0 };
            cost += pts_per_day * mult * dollars_per_point;
            rollovers += 1;
            m += 86_400;
        }
        if cost != 0.0 {
            trades_with_swap += 1;
            swap_total += cost;
            if t.side == Side::Buy {
                swap_long += cost;
            } else {
                swap_short += cost;
            }
        }
    }
    println!("\n=== SWAP / ROLLOVER (estimated, not in the PnL above) ===");
    println!("  Broker swap: long -12.6 pts/day, short -4.6 pts/day, triple on Wednesday");
    println!(
        "  {} of {} trades held through a rollover ({} rollovers charged)",
        trades_with_swap, m.trades, rollovers
    );
    println!(
        "  Estimated swap: long ${:.0} | short ${:.0} | total ${:.0}",
        swap_long, swap_short, swap_total
    );
    println!(
        "  Net of swap: ${:.2} (was ${:.2}) | drag {:.1}% of gross profit",
        m.total_pnl + swap_total,
        m.total_pnl,
        if m.gross_profit != 0.0 { swap_total / m.gross_profit * 100.0 } else { 0.0 }
    );

    // ---------------------------------------------------------------
    // 13. Exit-mix sanity
    // ---------------------------------------------------------------
    println!("\n=== EXIT MIX ===");
    println!("  initial stop {} | trailed stop {} | time exit {} | other {}",
        m.sl_exits, m.trail_exits,
        rep.trades.iter().filter(|t| matches!(t.reason, ExitReason::TimeExit)).count(),
        rep.trades.iter().filter(|t| matches!(t.reason, ExitReason::EndOfData)).count());
    println!("  average hold {:.1} bars", m.avg_bars_held);

    Ok(())
}

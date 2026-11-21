//! London / New York opening-range breakout study.
//!
//! Usage:
//!   cargo run --release -p xau-sim --bin xau-orb -- --parquet data/cache/xauusd_m5.parquet
//!   cargo run --release -p xau-sim --bin xau-orb -- --mode sweep
//!   cargo run --release -p xau-sim --bin xau-orb -- --mode costs
//!
//! Every mode prints the same verdict block at the end, so a run can be
//! quoted without re-reading the tables.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context;
use xau_core::Side;
use xau_data::read_bars_parquet;
use xau_sim::engine::POINT;
use xau_sim::orb::{
    challenge, days_from_civil_pub, size, ymd_hms, ChallengeCfg, Confirm, ExitKind, OrbConfig, OrbRun, SessionTz, StopMode,
};

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

fn f_arg(name: &str) -> Option<f64> {
    arg(name).map(|v| v.parse().unwrap_or_else(|_| panic!("bad --{name}: {v}")))
}

fn i_arg(name: &str) -> Option<usize> {
    arg(name).map(|v| v.parse().unwrap_or_else(|_| panic!("bad --{name}: {v}")))
}

/// Parse YYYY-MM-DD into epoch seconds (UTC midnight), or None.
fn parse_day(s: &str) -> Option<i64> {
    let mut it = s.split('-');
    let y: i32 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    // Reuse the manuscript's own civil conversion via a known epoch anchor.
    let days = days_from_civil_pub(y, m, d);
    Some(days * 86_400)
}

fn hhmm(min: u32) -> String {
    format!("{:02}:{:02}", min / 60, min % 60)
}

fn stamp(ts: i64) -> String {
    let (_, m, d, h, mi) = ymd_hms(ts);
    format!("{m:02}-{d:02} {h:02}:{mi:02}")
}

fn tz_label(tz: SessionTz) -> &'static str {
    match tz {
        SessionTz::None => "UTC",
        SessionTz::Eu => "London",
        SessionTz::Us => "New York",
    }
}

/// Infer the bar timeframe in minutes from the median spacing.
fn timeframe_minutes(bars: &[xau_core::Bar]) -> i64 {
    let mut diffs: Vec<i64> = bars.windows(2).map(|w| w[1].ts - w[0].ts).collect();
    diffs.sort_unstable();
    let mid = diffs.get(diffs.len() / 2).copied().unwrap_or(300);
    (mid.max(60) / 60).max(1)
}

fn base_config() -> OrbConfig {
    let mut cfg = OrbConfig::default();
    if let Some(v) = arg("tz") {
        cfg.tz = match v.as_str() {
            "none" | "utc" => SessionTz::None,
            "us" | "ny" => SessionTz::Us,
            _ => SessionTz::Eu,
        };
    }
    if let Some(v) = f_arg("box-start") {
        cfg.box_start_min = (v * 60.0).round() as u32;
    }
    if let Some(v) = f_arg("box-end") {
        cfg.box_end_min = (v * 60.0).round() as u32;
    }
    if let Some(v) = f_arg("cutoff") {
        cfg.cutoff_min = (v * 60.0).round() as u32;
    }
    if let Some(v) = i_arg("swing") {
        cfg.swing_bars = v;
    }
    if let Some(v) = arg("stop") {
        cfg.stop_mode = if v == "box" { StopMode::Box } else { StopMode::Swing };
    }
    if let Some(v) = arg("confirm") {
        cfg.confirm = if v == "close" { Confirm::Close } else { Confirm::Break };
    }
    if let Some(v) = f_arg("rr") {
        cfg.rr = v;
    }
    if let Some(v) = f_arg("spread") {
        cfg.spread_pts = v;
    }
    if let Some(v) = f_arg("entry-slip") {
        cfg.entry_slip_pts = v;
    }
    if let Some(v) = f_arg("stop-slip") {
        cfg.stop_slip_pts = v;
    }
    if let Some(v) = i_arg("min-box-bars") {
        cfg.min_box_bars = v;
    }
    if let Some(v) = f_arg("max-risk") {
        cfg.max_risk_pts = v;
    }
    if let Some(v) = i_arg("ema") {
        cfg.ema_period = v;
    }
    if let Some(v) = i_arg("max-wait") {
        cfg.max_wait_min = v as u32;
    }
    if let Some(v) = f_arg("risk") {
        cfg.risk_pct = v;
    }
    if let Some(v) = f_arg("deposit") {
        cfg.initial_deposit = v;
    }
    cfg
}

fn main() -> anyhow::Result<()> {
    let parquet = arg("parquet").unwrap_or_else(|| "data/cache/xauusd_m5.parquet".into());
    let path = Path::new(&parquet);
    if !path.exists() {
        anyhow::bail!("parquet not found: {}", path.display());
    }
    let mut bars = read_bars_parquet(path).context("reading parquet")?;
    let from = arg("from").and_then(|v| parse_day(&v));
    let to = arg("to").and_then(|v| parse_day(&v));
    if from.is_some() || to.is_some() {
        bars.retain(|b| {
            from.map_or(true, |f| b.ts >= f) && to.map_or(true, |t| b.ts < t)
        });
        if bars.is_empty() {
            anyhow::bail!("no bars in the requested date range");
        }
    }
    let tf = timeframe_minutes(&bars);
    let first = bars.first().map(|b| b.ts).unwrap_or(0);
    let last = bars.last().map(|b| b.ts).unwrap_or(0);
    let mut cfg = base_config();
    if arg("min-box-bars").is_none() {
        // Require half the session window to actually have bars, which is
        // generous enough for thin minutes but rejects holidays.
        let expected = ((cfg.box_end_min + 1_440 - cfg.box_start_min) % 1_440) as usize
            / tf.max(1) as usize;
        cfg.min_box_bars = (expected / 2).max(4);
    }
    if i_arg("swing").is_none() {
        // The transcript's "most recent swing" is minutes, not bars. Scale it
        // to whatever timeframe this file is on so M1 and M5 are comparable.
        let minutes = f_arg("swing-min").unwrap_or(30.0);
        cfg.swing_bars = (minutes / tf.max(1) as f64).round().max(1.0) as usize;
    }

    println!("XAUUSD opening-range breakout");
    println!(
        "data      {} · {} bars · {}m · {} → {}",
        path.display(),
        bars.len(),
        tf,
        stamp(first),
        stamp(last)
    );
    println!(
        "box       {} local window {}–{} → break, flat {}",
        tz_label(cfg.tz),
        hhmm(cfg.box_start_min),
        hhmm(cfg.box_end_min),
        hhmm(cfg.cutoff_min)
    );
    println!(
        "rules     first break · stop {} · {:.1}R target · one trade/day · confirm {}",
        match cfg.stop_mode {
            StopMode::Swing => format!("swing {} bars (~{} min)", cfg.swing_bars, cfg.swing_bars as i64 * tf),
            StopMode::Box => "box edge".into(),
        },
        cfg.rr,
        match cfg.confirm {
            Confirm::Break => "on the break",
            Confirm::Close => "on the close",
        }
    );
    println!(
        "costs     spread {:.0} pts · entry slip {:.0} · stop slip {:.0} · stop-first when both hit",
        cfg.spread_pts, cfg.entry_slip_pts, cfg.stop_slip_pts
    );

    let mode = arg("mode").unwrap_or_else(|| "detail".into());
    match mode.as_str() {
        "sweep" => sweep(&bars, &cfg, tf),
        "costs" => costs(&bars, &cfg),
        "detail" => detail(&bars, &cfg),
        "summary" => summary(&bars, &cfg),
        other => anyhow::bail!("unknown --mode {other} (detail|sweep|costs)"),
    }

    Ok(())
}

fn stats(run: &OrbRun, cfg: &OrbConfig) -> (Vec<xau_sim::orb::SizedTrade>, xau_sim::orb::EquityStats) {
    size(&run.trades, cfg.initial_deposit, cfg.risk_pct, cfg.max_lots, 0.01)
}

/// One machine-readable line, for comparing runs in a shell loop.
fn summary(bars: &[xau_core::Bar], cfg: &OrbConfig) {
    let run = xau_sim::orb::run(bars, cfg);
    let (_, st) = stats(&run, cfg);
    let ch = challenge(
        &run.trades,
        &ChallengeCfg {
            initial_deposit: cfg.initial_deposit,
            risk_pct: cfg.risk_pct,
            ..Default::default()
        },
        cfg.max_lots,
    );
    println!(
        "SUMMARY n={} win={:.1} pf={:.2} expR={:+.3} t={:.2} dd={:.2} ret={:+.2} worst_day={:.2} pass={} days={}",
        st.trades,
        st.win_rate,
        st.profit_factor,
        st.expectancy_r,
        st.t_stat,
        st.max_drawdown_pct,
        st.return_pct,
        st.worst_day_pct,
        ch.passed,
        ch.days
    );
}

fn detail(bars: &[xau_core::Bar], cfg: &OrbConfig) {
    let run = xau_sim::orb::run(bars, cfg);
    let (sized, st) = stats(&run, cfg);

    println!();
    println!("── signal quality ───────────────────────────────────────────────");
    println!("days scanned        {:>6}", run.days_scanned);
    println!(
        "trades taken        {:>6}   ({:.2} per trading day, {:.0}% of days)",
        st.trades,
        st.trades as f64 / run.days_scanned.max(1) as f64,
        st.trades as f64 / run.days_scanned.max(1) as f64 * 100.0
    );
    println!("no break            {:>6}", run.no_break);
    println!("box too thin        {:>6}", run.thin_box);
    println!("box filtered        {:>6}", run.box_filtered);
    println!("stop too wide       {:>6}", run.risk_filtered);
    println!("trend gate          {:>6}", run.trend_filtered);
    println!("both edges in a bar {:>6}", run.ambiguous_breaks);
    println!("ran out of data     {:>6}", run.unfinished);

    if st.trades == 0 {
        println!("\nno trades — nothing to report");
        return;
    }

    println!();
    println!("── outcome at ${:.0} · {:.2}% risk/trade ───────────────────────────", cfg.initial_deposit, cfg.risk_pct);
    println!("net                 ${:>9.2}   ({:+.2}%)", st.net, st.return_pct);
    println!("final equity        ${:>9.2}", st.final_equity);
    println!("wins / losses       {:>6} / {:<6} win rate {:.1}%", st.wins, st.losses, st.win_rate);
    println!(
        "profit factor       {:>6.2}      expectancy {:+.3}R · sd {:.2}R · t {:.1}",
        st.profit_factor, st.expectancy_r, st.sd_r, st.t_stat
    );
    println!("avg win / avg loss  {:+.2}R / {:+.2}R", st.avg_win_r, st.avg_loss_r);
    println!("max drawdown        ${:>9.2}   ({:.2}%)", st.max_drawdown, st.max_drawdown_pct);
    println!("longest loss streak {:>6}          worst day {:.2}%", st.max_losing_streak, st.worst_day_pct);
    println!("avg lots            {:>6.2}          max lots {:.2}", st.avg_lots, st.max_lots_used);

    // Exit mix.
    let mut stops = 0;
    let mut targets = 0;
    let mut times = 0;
    let mut eod = 0;
    for t in &run.trades {
        match t.reason {
            ExitKind::Stop => stops += 1,
            ExitKind::Target => targets += 1,
            ExitKind::Time => times += 1,
            ExitKind::EndOfData => eod += 1,
        }
    }
    println!();
    println!("exits               stop {stops} · target {targets} · time {times} · eod {eod}");

    // Side mix.
    let buys: Vec<_> = run.trades.iter().filter(|t| t.side == Side::Buy).collect();
    let sells: Vec<_> = run.trades.iter().filter(|t| t.side == Side::Sell).collect();
    for (label, set) in [("buys ", buys), ("sells", sells)] {
        if set.is_empty() {
            continue;
        }
        let wins = set.iter().filter(|t| t.r > 0.0).count();
        let sum: f64 = set.iter().map(|t| t.r).sum();
        println!(
            "{label}               {:>4} trades · {:>5.1}% win · {:+.3}R each",
            set.len(),
            wins as f64 / set.len() as f64 * 100.0,
            sum / set.len() as f64
        );
    }

    // Hold time and risk in points.
    let mut holds: Vec<usize> = run.trades.iter().map(|t| t.bars_held).collect();
    holds.sort_unstable();
    let mut risks: Vec<f64> = run.trades.iter().map(|t| t.risk_pts).collect();
    risks.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "hold (bars)         median {} · p90 {} · max {}",
        holds[holds.len() / 2],
        holds[(holds.len() * 9) / 10],
        holds[holds.len() - 1]
    );
    let mut boxes: Vec<f64> = run.trades.iter().map(|t| (t.box_high - t.box_low) / POINT).collect();
    boxes.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "box height          median ${:.2} · p90 ${:.2} · max ${:.2}",
        boxes[boxes.len() / 2] * POINT,
        boxes[(boxes.len() * 9) / 10] * POINT,
        boxes[boxes.len() - 1] * POINT
    );
    println!(
        "stop distance       median ${:.2} · p90 ${:.2} · max ${:.2}",
        risks[risks.len() / 2] * POINT,
        risks[(risks.len() * 9) / 10] * POINT,
        risks[risks.len() - 1] * POINT
    );

    // R bucket distribution.
    println!();
    println!("── R distribution ───────────────────────────────────────────────");
    let buckets: [(&str, fn(f64) -> bool); 6] = [
        ("<= -1.2", |r: f64| r <= -1.2),
        ("-1.2..-0.5", |r: f64| r > -1.2 && r <= -0.5),
        ("-0.5..0", |r: f64| r > -0.5 && r < 0.0),
        ("0..1", |r: f64| r >= 0.0 && r < 1.0),
        ("1..1.8", |r: f64| r >= 1.0 && r < 1.8),
        (">= 1.8R", |r: f64| r >= 1.8),
    ];
    for (label, f) in buckets {
        let n = run.trades.iter().filter(|t| f(t.r)).count();
        if n == 0 {
            continue;
        }
        let bar = "█".repeat((n * 60 / run.trades.len()).max(1));
        println!("{label:>10}  {n:>4}  {bar}");
    }

    // Monthly table.
    println!();
    println!("── by month ─────────────────────────────────────────────────────");
    println!("{:<8} {:>5} {:>6} {:>7} {:>8}", "month", "n", "win%", "R sum", "avg R");
    let mut months: BTreeMap<(i32, u32), Vec<f64>> = BTreeMap::new();
    for t in &run.trades {
        let (y, m, _, _, _) = ymd_hms(t.entry_ts);
        months.entry((y, m)).or_default().push(t.r);
    }
    for ((y, m), rs) in &months {
        let wins = rs.iter().filter(|r| **r > 0.0).count();
        let sum: f64 = rs.iter().sum();
        println!(
            "{:<8} {:>5} {:>6.1} {:>+7.2} {:>+8.3}",
            format!("{y}-{m:02}"),
            rs.len(),
            wins as f64 / rs.len() as f64 * 100.0,
            sum,
            sum / rs.len() as f64
        );
    }

    // Prop challenge runs.
    println!();
    println!("── challenge simulation (same sizing as live) ───────────────────");
    let accounts = [
        (5_000.0, 0.7, 8.0, 6.0, 4.0),
        (10_000.0, 0.7, 8.0, 6.0, 4.0),
        (25_000.0, 0.8, 8.0, 6.0, 4.0),
        (100_000.0, 0.7, 8.0, 6.0, 4.0),
    ];
    for (deposit, risk, target, dd, daily) in accounts {
        let ch = ChallengeCfg {
            initial_deposit: deposit,
            risk_pct: risk,
            target_pct: target,
            max_dd_pct: dd,
            daily_dd_pct: daily,
        };
        let r = challenge(&run.trades, &ch, cfg.max_lots);
        println!(
            "${:<7.0} risk {:.2}% → {:<9} after {:>3} trades · ${:>9.2} · worst day {:.2}% · maxDD {:.2}% · {}",
            deposit,
            risk,
            if r.passed { "PASS" } else { "fail" },
            r.trades,
            r.final_equity,
            r.worst_day_pct,
            r.max_dd_pct,
            r.reason
        );
    }

    // Spread / slippage stress.
    println!();
    println!("── cost stress ──────────────────────────────────────────────────");
    println!("{:<10} {:>5} {:>7} {:>8} {:>9} {:>9}", "spread", "n", "win%", "PF", "expR", "net$");
    for mult in [1.0, 2.0, 3.0, 4.0] {
        let mut c = cfg.clone();
        c.spread_pts = cfg.spread_pts * mult;
        c.entry_slip_pts = cfg.entry_slip_pts * mult;
        c.stop_slip_pts = cfg.stop_slip_pts * mult;
        let r = xau_sim::orb::run(bars, &c);
        let (_, s) = stats(&r, &c);
        println!(
            "{:<10.0} {:>5} {:>7.1} {:>8.2} {:>+9.3} {:>+9.0}",
            c.spread_pts, s.trades, s.win_rate, s.profit_factor, s.expectancy_r, s.net
        );
    }

    // Verdict.
    println!();
    println!("── verdict ──────────────────────────────────────────────────────");
    let breakeven_wr = 100.0 / (1.0 + cfg.rr);
    println!(
        "needs {:.1}% wins to break even at {:.1}R before costs; it got {:.1}%",
        breakeven_wr, cfg.rr, st.win_rate
    );
    if st.expectancy_r > 0.05 && st.profit_factor > 1.15 {
        println!("positive after costs — worth a paper-trade, not yet money.");
    } else {
        println!("not an edge after costs on this data.");
    }
    let _ = sized;
}

fn sweep(bars: &[xau_core::Bar], base: &OrbConfig, tf: i64) {
    println!();
    println!("── sensitivity sweep ────────────────────────────────────────────");
    println!(
        "{:<34} {:>5} {:>6} {:>6} {:>7} {:>8} {:>8}",
        "config", "n", "win%", "PF", "expR", "ret%", "maxDD%"
    );

    let mut variants: Vec<(String, OrbConfig)> = Vec::new();
    let mut add = |label: String, f: &dyn Fn(&mut OrbConfig)| {
        let mut c = base.clone();
        f(&mut c);
        variants.push((label, c));
    };

    add("base".into(), &|_| {});
    for &m in &[5.0, 10.0, 15.0, 20.0, 30.0, 45.0, 60.0, 90.0] {
        add(format!("swing {m:.0} min"), &move |c: &mut OrbConfig| {
            c.swing_bars = (m / tf as f64).round().max(1.0) as usize
        });
    }
    add("stop at box edge".into(), &|c: &mut OrbConfig| {
        c.stop_mode = StopMode::Box
    });
    for &rr in &[1.0, 1.5, 2.0, 2.5, 3.0] {
        add(format!("rr {rr:.1}"), &move |c: &mut OrbConfig| c.rr = rr);
    }
    add("confirm on close".into(), &|c: &mut OrbConfig| {
        c.confirm = Confirm::Close
    });
    for &(s, e) in &[
        (7.0, 12.0),
        (8.0, 13.0),
        (8.0, 14.0),
        (9.0, 13.0),
        (9.0, 15.0),
        (10.0, 14.0),
        (7.0, 14.0),
    ] {
        add(format!("box {s:.0}-{e:.0}"), &move |c: &mut OrbConfig| {
            c.box_start_min = (s * 60.0) as u32;
            c.box_end_min = (e * 60.0) as u32;
        });
    }
    for &h in &[16.0, 17.0, 18.0, 20.0, 21.0] {
        add(format!("flat {h:.0}:00"), &move |c: &mut OrbConfig| {
            c.cutoff_min = (h * 60.0) as u32
        });
    }
    for &p in &[200usize, 600, 1200, 2400] {
        add(format!("ema {p}"), &move |c: &mut OrbConfig| c.ema_period = p);
    }
    for &m in &[30u32, 60, 120, 240] {
        add(format!("break within {m} min"), &move |c: &mut OrbConfig| {
            c.max_wait_min = m
        });
    }
    add("spread 30".into(), &|c: &mut OrbConfig| {
        c.spread_pts = 30.0;
        c.entry_slip_pts = 5.0;
        c.stop_slip_pts = 10.0;
    });

    let mut rows: Vec<(String, xau_sim::orb::EquityStats)> = Vec::new();
    for (label, c) in &variants {
        let r = xau_sim::orb::run(bars, c);
        let (_, st) = stats(&r, c);
        rows.push((label.clone(), st));
    }

    rows.sort_by(|a, b| b.1.expectancy_r.partial_cmp(&a.1.expectancy_r).unwrap());
    for (label, st) in &rows {
        println!(
            "{:<34} {:>5} {:>6.1} {:>6.2} {:>+7.3} {:>+8.1} {:>8.1}",
            label,
            st.trades,
            st.win_rate,
            st.profit_factor,
            st.expectancy_r,
            st.return_pct,
            st.max_drawdown_pct
        );
    }

    let solid: Vec<_> = rows.iter().filter(|(_, s)| s.trades >= 100).collect();
    let positives = solid.iter().filter(|(_, s)| s.expectancy_r > 0.0).count();
    println!(
        "\n{} of {} configs positive with >=100 trades; best {:+.3}R ({})",
        positives,
        solid.len(),
        rows[0].1.expectancy_r,
        rows[0].0
    );
}

fn costs(bars: &[xau_core::Bar], base: &OrbConfig) {
    println!();
    println!("── spread × slippage stress ─────────────────────────────────────");
    println!(
        "{:<28} {:>5} {:>6} {:>6} {:>7} {:>8} {:>8}",
        "cost", "n", "win%", "PF", "expR", "net$", "maxDD%"
    );
    for (spread, slip) in [
        (0.0, 0.0),
        (5.0, 0.0),
        (10.0, 1.0),
        (15.0, 2.0),
        (20.0, 3.0),
        (30.0, 5.0),
        (45.0, 8.0),
        (60.0, 10.0),
    ] {
        let mut c = base.clone();
        c.spread_pts = spread;
        c.entry_slip_pts = slip;
        c.stop_slip_pts = slip * 2.0;
        let r = xau_sim::orb::run(bars, &c);
        let (_, st) = stats(&r, &c);
        println!(
            "{:<28} {:>5} {:>6.1} {:>6.2} {:>+7.3} {:>+8.0} {:>8.2}",
            format!("{spread:.0} pts + {slip:.0} slip"),
            st.trades,
            st.win_rate,
            st.profit_factor,
            st.expectancy_r,
            st.net,
            st.max_drawdown_pct
        );
    }
}

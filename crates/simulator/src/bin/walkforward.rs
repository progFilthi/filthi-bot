//! Walk-forward strategy search with out-of-sample validation.
//!
//! The only honest way to ask "does this strategy make money":
//!   1. Optimise over the FIRST 70% of bars (in-sample).
//!   2. Take the winners and re-test them, untouched, on the LAST 30%
//!      (out-of-sample).
//!
//! If in-sample profit does not survive out-of-sample, it was curve-fitting.
//!
//! Usage:
//!   cargo run -p xau-sim --bin xau-walkforward -- --parquet data/cache/xauusd_m1.parquet

use std::path::Path;

use anyhow::Context;
use xau_core::{Bar, Side};
use xau_data::read_bars_parquet;
use xau_sim::engine::{backtest, EngineConfig, ExitReason, Metrics};
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

#[derive(Debug, Clone)]
struct Candidate {
    label: String,
    kind: StratKind,
    params: StratParams,
    cfg: EngineConfig,
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

fn candidates() -> Vec<Candidate> {
    let mut out = Vec::new();
    let liquid = vec![13, 14, 15, 16];

    // ---- Moving-average crossover (baseline family) ----
    for (f, s) in [(5usize, 20usize), (10, 30), (20, 50), (50, 200)] {
        for hours in [Vec::new(), liquid.clone()] {
            let mut cfg = base_cfg();
            cfg.allowed_hours = hours.clone();
            out.push(Candidate {
                label: format!(
                    "ma-cross {f}/{s} {}",
                    if hours.is_empty() { "all-hours" } else { "liquid" }
                ),
                kind: StratKind::MaCross,
                params: StratParams { fast: f, slow: s, ..Default::default() },
                cfg,
            });
        }
    }

    // ---- Donchian breakout (trend following) ----
    for lb in [10usize, 20, 55] {
        for (sl, tp, trail, hold) in [
            (2.0f64, 4.0f64, 0.0f64, 0usize), // fixed 2:1
            (2.0, 0.0, 2.5, 0),               // trail only
            (1.5, 0.0, 2.0, 240),             // tight trail + time exit
        ] {
            for trend in [200usize, 0] {
                let mut cfg = base_cfg();
                cfg.sl_atr = sl;
                cfg.tp_atr = tp;
                cfg.trail_atr = trail;
                cfg.max_hold_bars = hold;
                let p = StratParams {
                    lookback: lb,
                    trend_ma: if trend == 0 { 1 } else { trend },
                    ..Default::default()
                };
                out.push(Candidate {
                    label: format!(
                        "donchian lb{lb} sl{sl} tp{tp} tr{trail} h{hold} trend{}",
                        if trend == 0 { "off" } else { "200" }
                    ),
                    kind: StratKind::Donchian,
                    params: p,
                    cfg,
                });
            }
        }
    }

    // ---- Bollinger fade (mean reversion) ----
    for k in [2.0f64, 2.5, 3.0] {
        for (sl, tp, hold) in [(3.0f64, 3.0f64, 0usize), (3.0, 0.0, 120)] {
            let mut cfg = base_cfg();
            cfg.sl_atr = sl;
            cfg.tp_atr = tp;
            cfg.max_hold_bars = hold;
            out.push(Candidate {
                label: format!("bollinger k{k} sl{sl} tp{tp} h{hold}"),
                kind: StratKind::Bollinger,
                params: StratParams { lookback: 20, band_k: k, ..Default::default() },
                cfg,
            });
        }
    }

    // ---- RSI reversion ----
    for (period, buy, sell) in [(14usize, 30.0f64, 70.0f64), (14, 20.0, 80.0), (7, 25.0, 75.0)] {
        for sl in [3.0f64, 4.0] {
            let mut cfg = base_cfg();
            cfg.sl_atr = sl;
            cfg.tp_atr = 3.0;
            cfg.max_hold_bars = 120;
            out.push(Candidate {
                label: format!("rsi{period} {buy:.0}/{sell:.0} sl{sl}"),
                kind: StratKind::RsiReversion,
                params: StratParams {
                    rsi_period: period,
                    rsi_buy: buy,
                    rsi_sell: sell,
                    ..Default::default()
                },
                cfg,
            });
        }
    }

    // ---- Trend pullback ----
    for (f, s) in [(20usize, 50usize), (50, 100)] {
        for trend in [200usize, 0] {
            let mut cfg = base_cfg();
            cfg.sl_atr = 2.0;
            cfg.tp_atr = 4.0;
            out.push(Candidate {
                label: format!(
                    "pullback {f}/{s} trend{}",
                    if trend == 0 { "off" } else { "200" }
                ),
                kind: StratKind::TrendPullback,
                params: StratParams {
                    fast: f,
                    slow: s,
                    trend_ma: if trend == 0 { 1 } else { trend },
                    ..Default::default()
                },
                cfg,
            });
        }
    }

    out
}

fn fmt_row(name: &str, m: &Metrics) -> String {
    format!(
        "{:<38} {:>5} {:>6.1}% {:>10.2} {:>6.2} {:>6.2} {:>7.2}% {:>8.2}",
        name,
        m.trades,
        m.win_rate,
        m.total_pnl,
        m.profit_factor,
        m.expectancy,
        m.return_pct,
        m.sharpe
    )
}

fn header() -> String {
    format!(
        "{:<38} {:>5} {:>7} {:>10} {:>6} {:>6} {:>8} {:>8}",
        "strategy", "trds", "win%", "pnl$", "PF", "EV$", "ret%", "sharpe"
    )
}

fn evaluate(bars: &[Bar], signals: &[Option<Side>], cfg: &EngineConfig) -> (Metrics, Vec<(ExitReason, usize)>) {
    let rep = backtest(bars, signals, cfg);
    let mut reasons: Vec<(ExitReason, usize)> = Vec::new();
    for t in &rep.trades {
        if let Some(e) = reasons.iter_mut().find(|(r, _)| *r == t.reason) {
            e.1 += 1;
        } else {
            reasons.push((t.reason, 1));
        }
    }
    (rep.metrics, reasons)
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
    let (is_bars, oos_bars) = bars.split_at(split);

    println!("=== WALK-FORWARD VALIDATION ===");
    println!("Total bars: {}", n);
    println!(
        "In-sample:  {} bars ({} -> {})",
        is_bars.len(),
        is_bars[0].ts,
        is_bars[is_bars.len() - 1].ts
    );
    println!(
        "Out-sample: {} bars ({} -> {})",
        oos_bars.len(),
        oos_bars[0].ts,
        oos_bars[oos_bars.len() - 1].ts
    );
    println!("Spread: 20 pts | Lots: 0.10 | Deposit: $25,000");

    let cands = candidates();
    println!("\nTesting {} strategy configurations...\n", cands.len());

    struct Row {
        cand: Candidate,
        is_m: Metrics,
        oos_m: Metrics,
        oos_reasons: Vec<(ExitReason, usize)>,
    }

    let mut rows: Vec<Row> = Vec::new();
    for c in cands {
        // Signals are computed on the FULL series so the out-of-sample slice
        // has complete indicator history. No future data leaks: the signal at
        // bar i only ever uses bars <= i.
        let sig = entry_signals(&bars, c.kind, &c.params);
        let (is_m, _is_reasons) = evaluate(is_bars, &sig[..split], &c.cfg);
        let (oos_m, oos_reasons) = evaluate(oos_bars, &sig[split..], &c.cfg);
        rows.push(Row { cand: c, is_m, oos_m, oos_reasons });
    }

    // Rank by in-sample total PnL, with a minimum trade count so a single lucky
    // trade cannot win.
    let mut ranked: Vec<usize> = (0..rows.len()).collect();
    ranked.sort_by(|a, b| {
        let ra = &rows[*a];
        let rb = &rows[*b];
        let sa = if ra.is_m.trades >= 30 { ra.is_m.total_pnl } else { f64::NEG_INFINITY };
        let sb = if rb.is_m.trades >= 30 { rb.is_m.total_pnl } else { f64::NEG_INFINITY };
        sb.partial_cmp(&sa).unwrap()
    });

    println!("=== IN-SAMPLE RESULTS (first {:.0}%, ranked by profit) ===", split_pct);
    println!("{}", header());
    println!("{}", "-".repeat(100));
    for idx in ranked.iter().take(15) {
        let r = &rows[*idx];
        println!("{}", fmt_row(&r.cand.label, &r.is_m));
    }

    println!("\n=== OUT-OF-SAMPLE RESULTS for the top 10 in-sample winners ===");
    println!("(these numbers are the honest test: parameters were NOT tuned on this data)");
    println!("{}", header());
    println!("{}", "-".repeat(100));
    for idx in ranked.iter().take(10) {
        let r = &rows[*idx];
        println!("{}", fmt_row(&r.cand.label, &r.oos_m));
    }

    // How many configurations survive out of sample at all?
    let tested = rows.iter().filter(|r| r.is_m.trades >= 30).count();
    let is_profitable = rows.iter().filter(|r| r.is_m.trades >= 30 && r.is_m.total_pnl > 0.0).count();
    let oos_profitable = rows.iter().filter(|r| r.oos_m.trades >= 10 && r.oos_m.total_pnl > 0.0).count();

    println!("\n=== SURVIVAL SUMMARY ===");
    println!("Configurations tested:            {tested}");
    println!("Profitable in-sample:             {is_profitable} ({:.0}%)",
        if tested > 0 { is_profitable as f64 / tested as f64 * 100.0 } else { 0.0 });
    println!("Profitable out-of-sample:         {oos_profitable} ({:.0}%)",
        if tested > 0 { oos_profitable as f64 / tested as f64 * 100.0 } else { 0.0 });

    // Correlation between IS and OOS profit is the curve-fitting tell.
    let pairs: Vec<(f64, f64)> = rows
        .iter()
        .filter(|r| r.is_m.trades >= 30 && r.oos_m.trades >= 10)
        .map(|r| (r.is_m.total_pnl, r.oos_m.total_pnl))
        .collect();
    if pairs.len() > 3 {
        let n = pairs.len() as f64;
        let mx = pairs.iter().map(|p| p.0).sum::<f64>() / n;
        let my = pairs.iter().map(|p| p.1).sum::<f64>() / n;
        let cov = pairs.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum::<f64>();
        let sx = pairs.iter().map(|p| (p.0 - mx).powi(2)).sum::<f64>().sqrt();
        let sy = pairs.iter().map(|p| (p.1 - my).powi(2)).sum::<f64>().sqrt();
        if sx > 0.0 && sy > 0.0 {
            println!(
                "IS/OOS profit correlation:        {:+.2}  (near 0 = in-sample skill is noise)",
                cov / (sx * sy)
            );
        }
    }

    // Detailed report on the top in-sample winner, out of sample.
    if let Some(&best) = ranked.first() {
        let r = &rows[best];
        println!("\n=== DETAIL: best in-sample configuration, out-of-sample ===");
        println!("Strategy: {}", r.cand.label);
        println!("Trades:            {}", r.oos_m.trades);
        println!("Wins / Losses:     {} / {}", r.oos_m.wins, r.oos_m.losses);
        println!("Win rate:          {:.2}%", r.oos_m.win_rate);
        println!("Total PnL:         ${:.2}", r.oos_m.total_pnl);
        println!("Return:            {:.2}%", r.oos_m.return_pct);
        println!("Profit factor:     {:.2}", r.oos_m.profit_factor);
        println!("Expectancy:        ${:.2}/trade", r.oos_m.expectancy);
        println!("Avg win / loss:    ${:.2} / ${:.2}", r.oos_m.avg_win, r.oos_m.avg_loss);
        println!("Max drawdown:      ${:.2} ({:.2}%)", r.oos_m.max_drawdown, r.oos_m.max_drawdown_pct);
        println!("Sharpe (annual):   {:.2}", r.oos_m.sharpe);
        println!("Avg bars held:     {:.0}", r.oos_m.avg_bars_held);
        println!("Exposure:          {:.1}% of bars", r.oos_m.exposure_pct);
        println!("Max losing streak: {}", r.oos_m.max_losing_streak);
        let mut rsn = r.oos_reasons.clone();
        rsn.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
        let rs: Vec<String> = rsn
            .iter()
            .map(|(k, c)| format!("{}={}", k.label(), c))
            .collect();
        println!("Exit breakdown:    {}", rs.join(" "));
    }

    // Anything at all that survives out of sample?
    let mut oos_best: Vec<&Row> = rows.iter().filter(|r| r.oos_m.trades >= 10).collect();
    oos_best.sort_by(|a, b| b.oos_m.total_pnl.partial_cmp(&a.oos_m.total_pnl).unwrap());
    println!("\n=== BEST OUT-OF-SAMPLE PERFORMERS (reference only) ===");
    println!("{}", header());
    println!("{}", "-".repeat(100));
    for r in oos_best.iter().take(5) {
        println!("{}", fmt_row(&r.cand.label, &r.oos_m));
    }

    Ok(())
}

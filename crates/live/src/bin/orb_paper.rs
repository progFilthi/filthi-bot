//! Paper-trading logger for the filtered London opening-range breakout.
//!
//! This binary places **no orders**. It polls the terminal for closed M5 bars,
//! asks the same `xau_sim::orb` code the backtest uses what today's session
//! looks like, and writes two files:
//!
//!   * `runs/orb/journal.jsonl` — one line per signal and one per result
//!   * `runs/orb/board.json`    — today's session plus the forward record
//!
//! Why judge the rule on its own log rather than on the backtest: the
//! filtered variant is only +0.13R out of sample with t≈1.1, so the only
//! clean test left is forward data. Keeping the journal separate from the
//! D3 runner means a bad day here cannot touch the demo account.
//!
//! Time handling: the broker clock is EET/EEST (UTC+2, +1h on EU summer
//! time), so each bar is converted to true UTC before it reaches the
//! strategy. The strategy then applies London daylight-saving rules itself.
//! Getting this wrong shifts the box by an hour half the year, which is
//! exactly the kind of change that flipped this rule's test result from
//! +0.13R to −0.21R.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{Datelike, NaiveDateTime, Timelike};
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use xau_core::{Bar, Side};
use xau_live::market::{closed_bars_at, period_secs, server_now};
use xau_live::mcp::Mcp;
use xau_sim::engine::{CONTRACT, POINT};
use xau_sim::orb::{
    day_view, eu_dst_active, session_times, size, ymd_hms, DayView, ExitKind, OrbConfig, OrbTrade,
    SessionTz,
};

#[derive(Parser, Debug)]
#[command(about = "Log the filtered London breakout on demo. Never places orders.")]
struct Args {
    /// Seconds between polls.
    #[arg(long, default_value_t = 60)]
    poll: u64,
    /// Poll once and exit (for tests and cron).
    #[arg(long)]
    once: bool,
    /// Timeframe to evaluate. M5 is the tested series.
    #[arg(long, default_value = "M5")]
    period: String,
    /// Closed bars to fetch per poll. Enough to backfill a long weekend.
    #[arg(long, default_value_t = 2_016)]
    bars: usize,
    /// Percent of the paper balance risked per trade.
    #[arg(long, default_value_t = 0.7)]
    risk: f64,
    /// Paper starting balance.
    #[arg(long, default_value_t = 5_000.0)]
    deposit: f64,
    /// Hard lot ceiling (mirrors the demo runner).
    #[arg(long, default_value_t = 5.0)]
    max_lots: f64,
    /// Broker clock is UTC + this in winter; +1h more while EU summer time runs.
    #[arg(long, default_value_t = 7_200)]
    server_winter_offset_s: i64,
    #[arg(long, default_value = "http://127.0.0.1:22346/mcp")]
    url: String,
    /// Bearer token. Defaults to $MT5_MCP_TOKEN, then the token file.
    #[arg(long, env = "MT5_MCP_TOKEN", default_value = "")]
    token: String,
    /// File holding the token. The terminal rotates it on every restart.
    #[arg(long)]
    token_file: Option<PathBuf>,
    #[arg(long)]
    journal: Option<PathBuf>,
    #[arg(long)]
    board: Option<PathBuf>,
}

struct Paths {
    journal: PathBuf,
    board: PathBuf,
}

// ---------------------------------------------------------------------------
// Journal
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Entry {
    Signal {
        day: i64,
        date: String,
        /// Wall-clock time the logger wrote this line.
        recorded_at: String,
        /// False when the session was reconstructed from bars after a restart.
        live: bool,
        side: Side,
        entry: f64,
        stop: f64,
        target: f64,
        risk_pts: f64,
        box_high: f64,
        box_low: f64,
        signal_utc: String,
        entry_ts: i64,
    },
    Result {
        day: i64,
        date: String,
        recorded_at: String,
        live: bool,
        side: Side,
        entry: f64,
        stop: f64,
        target: f64,
        exit: f64,
        exit_kind: ExitKind,
        risk_pts: f64,
        pnl_pts: f64,
        r: f64,
        entry_ts: i64,
        exit_ts: i64,
        exit_utc: String,
        bars_held: usize,
        box_high: f64,
        box_low: f64,
    },
}

impl Entry {
    fn day(&self) -> i64 {
        match self {
            Entry::Signal { day, .. } | Entry::Result { day, .. } => *day,
        }
    }

    fn as_trade(&self) -> Option<OrbTrade> {
        match self {
            Entry::Result {
                day,
                side,
                entry,
                stop,
                target,
                exit,
                exit_kind,
                risk_pts,
                pnl_pts,
                r,
                entry_ts,
                exit_ts,
                bars_held,
                box_high,
                box_low,
                ..
            } => Some(OrbTrade {
                day: *day,
                side: *side,
                entry_ts: *entry_ts,
                exit_ts: *exit_ts,
                entry: *entry,
                stop: *stop,
                target: *target,
                exit: *exit,
                reason: *exit_kind,
                box_high: *box_high,
                box_low: *box_low,
                risk_pts: *risk_pts,
                pnl_pts: *pnl_pts,
                r: *r,
                bars_held: *bars_held,
            }),
            Entry::Signal { .. } => None,
        }
    }
}

#[derive(Default)]
struct Journal {
    signal_days: HashSet<i64>,
    result_days: HashSet<i64>,
    trades: Vec<OrbTrade>,
    /// Results that were written while the session was still running.
    live_trades: usize,
}

fn load_journal(path: &Path) -> Journal {
    let mut out = Journal::default();
    let Ok(file) = fs::File::open(path) else {
        return out;
    };
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Entry>(&line) {
            Ok(e) => absorb(&mut out, e),
            Err(err) => eprintln!("journal: skipping unreadable line ({err})"),
        }
    }
    out.trades.sort_by_key(|t| t.entry_ts);
    out
}

fn absorb(j: &mut Journal, e: Entry) {
    let live = match &e {
        Entry::Signal { live, .. } | Entry::Result { live, .. } => *live,
    };
    if let Some(t) = e.as_trade() {
        j.result_days.insert(e.day());
        j.trades.push(t);
        if live {
            j.live_trades += 1;
        }
    } else {
        j.signal_days.insert(e.day());
    }
}

fn append_entry(path: &Path, entry: &Entry) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    writeln!(f, "{}", serde_json::to_string(entry)?)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Time helpers
// ---------------------------------------------------------------------------

fn fmt_utc(ts: i64) -> String {
    let (y, m, d, h, mi) = ymd_hms(ts);
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}")
}

fn fmt_hhmm(ts: i64) -> String {
    let (_, _, _, h, mi) = ymd_hms(ts);
    format!("{h:02}:{mi:02}")
}

fn date_string(day: i64) -> String {
    let (y, m, d, _, _) = ymd_hms(day * 86_400);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Server clock → UTC. The broker runs UTC+2, and +1h more while EU summer
/// time is in force (the two switch on the same dates).
fn server_utc_shift(ts_server: i64, args: &Args) -> i64 {
    let summer = if eu_dst_active(ts_server.div_euclid(86_400)) {
        3_600
    } else {
        0
    };
    args.server_winter_offset_s + summer
}

/// The demo account trades Mon–Fri from 01:00 server time.
fn market_open(now_server: NaiveDateTime) -> bool {
    now_server.weekday().number_from_monday() <= 5 && now_server.hour() >= 1
}

/// The terminal's clock stops at the last quote, so a frozen Friday clock
/// still looks like Friday 23:59 on Saturday. Only call the feed live when a
/// fresh bar arrived according to the host's own clock.
fn feed_is_live(now_server: NaiveDateTime, behind_min: f64) -> bool {
    market_open(now_server) && behind_min <= 15.0
}

// ---------------------------------------------------------------------------
// Session → journal entries
// ---------------------------------------------------------------------------

fn signal_entry(view: &DayView) -> Option<Entry> {
    let side = view.side?;
    let risk_pts = (view.entry - view.stop).abs() / POINT;
    if !(risk_pts.is_finite() && risk_pts > 0.0) {
        return None;
    }
    // "live" means the logger wrote this soon after the bar printed, not
    // merely that the terminal's (possibly frozen) clock agrees with the day.
    let captured = chrono::Utc::now().timestamp();
    Some(Entry::Signal {
        day: view.day,
        date: date_string(view.day),
        recorded_at: fmt_utc(captured),
        live: (captured - view.entry_ts).abs() <= 1_800,
        side,
        entry: view.entry,
        stop: view.stop,
        target: view.target,
        risk_pts,
        box_high: view.box_high,
        box_low: view.box_low,
        signal_utc: fmt_utc(view.entry_ts),
        entry_ts: view.entry_ts,
    })
}

fn result_entry(view: &DayView, period_secs: i64) -> Option<Entry> {
    let side = view.side?;
    let r = view.result_r?;
    let risk_pts = (view.entry - view.stop).abs() / POINT;
    if !(risk_pts.is_finite() && risk_pts > 0.0) {
        return None;
    }
    let captured = chrono::Utc::now().timestamp();
    Some(Entry::Result {
        day: view.day,
        date: date_string(view.day),
        recorded_at: fmt_utc(captured),
        live: (captured - view.last_ts).abs() <= 1_800,
        side,
        entry: view.entry,
        stop: view.stop,
        target: view.target,
        exit: view.last_close,
        exit_kind: view.exit.unwrap_or(ExitKind::EndOfData),
        risk_pts,
        pnl_pts: r * risk_pts,
        r,
        entry_ts: view.entry_ts,
        exit_ts: view.last_ts,
        exit_utc: fmt_utc(view.last_ts),
        bars_held: ((view.last_ts - view.entry_ts).max(0) / period_secs.max(1)) as usize,
        box_high: view.box_high,
        box_low: view.box_low,
    })
}

// ---------------------------------------------------------------------------
// Board
// ---------------------------------------------------------------------------

fn lots_for(equity: f64, risk_pct: f64, risk_pts: f64, max_lots: f64) -> f64 {
    if !(risk_pts.is_finite() && risk_pts > 0.0) {
        return 0.0;
    }
    let budget = equity * risk_pct / 100.0;
    let raw = budget / (risk_pts * POINT * CONTRACT);
    let mut lots = (raw / 0.01).floor() * 0.01;
    if lots < 0.01 {
        lots = 0.01;
    }
    lots.min(max_lots)
}

fn round(v: f64, places: u32) -> f64 {
    let f = 10f64.powi(places as i32);
    (v * f).round() / f
}

fn write_board(
    path: &Path,
    args: &Args,
    cfg: &OrbConfig,
    now_server: NaiveDateTime,
    now_utc: i64,
    last_bar_utc: i64,
    bars: &[Bar],
    journal: &Journal,
) -> Result<()> {
    let (sized, stats) = size(
        &journal.trades,
        cfg.initial_deposit,
        cfg.risk_pct,
        args.max_lots,
        0.01,
    );
    let equity = sized
        .last()
        .map(|s| s.equity_after)
        .unwrap_or(cfg.initial_deposit);

    let wall_utc = chrono::Utc::now().timestamp();
    let behind_min = ((wall_utc - last_bar_utc) as f64 / 60.0).max(0.0);
    let feed_live = feed_is_live(now_server, behind_min);

    let today = now_utc.div_euclid(86_400);
    let (box_start, box_end, entry_end, cutoff) = session_times(today, cfg);
    let view = day_view(bars, cfg, today, now_utc);
    let has_box = view.as_ref().map(|v| v.box_bars).unwrap_or(0) >= cfg.min_box_bars
        || matches!(
            view.as_ref().map(|v| v.phase),
            Some("in_trade") | Some("closed")
        );

    let (day_date, phase, reason) = match (has_box, view.as_ref()) {
        (true, Some(v)) => (date_string(today), v.phase, v.reason),
        (_, _) if now_utc < box_start => (
            date_string(today),
            "pre_session",
            "market open, waiting for the box to start",
        ),
        _ => (date_string(today), "idle", "no session today"),
    };

    // The next session is only interesting when today has none left.
    let next_session = if phase == "idle" || phase == "pre_session" {
        let day = if now_utc < box_start { today } else { today + 1 };
        let (bs, be, ee, cut) = session_times(day, cfg);
        Some(json!({
            "date": date_string(day),
            "box_start_utc": fmt_hhmm(bs),
            "box_end_utc": fmt_hhmm(be),
            "entry_until_utc": fmt_hhmm(ee),
            "flat_utc": fmt_hhmm(cut),
        }))
    } else {
        None
    };

    let ticket = view.as_ref().and_then(|v| {
        let side = v.side?;
        let risk_pts = (v.entry - v.stop).abs() / POINT;
        Some(json!({
            "side": side,
            "entry": round(v.entry, 2),
            "stop": round(v.stop, 2),
            "target": round(v.target, 2),
            "risk_pts": round(risk_pts, 1),
            "risk_usd_full_lot": round(risk_pts * POINT * CONTRACT, 2),
            "lots": round(lots_for(equity, cfg.risk_pct, risk_pts, args.max_lots), 2),
            "risk_usd_at_lots": round(
                risk_pts * POINT * CONTRACT * lots_for(equity, cfg.risk_pct, risk_pts, args.max_lots),
                2
            ),
            "entry_utc": fmt_utc(v.entry_ts),
        }))
    });

    let recent: Vec<Value> = sized
        .iter()
        .rev()
        .take(20)
        .map(|s| {
            let t = &s.trade;
            json!({
                "date": date_string(t.day),
                "side": t.side,
                "entry": round(t.entry, 2),
                "stop": round(t.stop, 2),
                "target": round(t.target, 2),
                "exit": round(t.exit, 2),
                "exit_kind": t.reason.label(),
                "risk_pts": round(t.risk_pts, 1),
                "r": round(t.r, 3),
                "lots": s.lots,
                "pnl_usd": round(s.pnl, 2),
                "equity_after": round(s.equity_after, 2),
            })
        })
        .collect();

    let since = journal
        .trades
        .first()
        .map(|t| date_string(t.day))
        .unwrap_or_else(|| "—".into());

    let board = json!({
        "strategy": "London opening-range breakout — filtered (demo paper log)",
        "note": "Signals are logged, not traded. No order is ever sent by this process.",
        "generated_at_utc": fmt_utc(wall_utc),
        "terminal_time_utc": fmt_utc(now_utc),
        "terminal_time_server": now_server.format("%Y-%m-%d %H:%M:%S").to_string(),
        "last_bar_utc": fmt_utc(last_bar_utc),
        "feed_minutes_behind": round(behind_min, 1),
        "market_open": feed_live,
        "config": {
            "period": args.period,
            "box": format!(
                "{}–{} London",
                fmt_minute(cfg.box_start_min),
                fmt_minute(cfg.box_end_min)
            ),
            "entry_window_min": cfg.max_wait_min,
            "swing_min": cfg.swing_bars as i64 * period_secs(&args.period).unwrap_or(300) / 60,
            "target_r": cfg.rr,
            "flat": format!("{} London", fmt_minute(cfg.cutoff_min)),
            "spread_pts": cfg.spread_pts,
            "risk_pct": cfg.risk_pct,
            "deposit": cfg.initial_deposit,
            "max_lots": args.max_lots,
            "server_utc_offset_h": args.server_winter_offset_s as f64 / 3600.0,
        },
        "session": {
            "date": day_date,
            "is_current_day": day_date == date_string(wall_utc.div_euclid(86_400)),
            "phase": phase,
            "reason": reason,
            "box": view.as_ref().map(|v| json!({
                "high": round(v.box_high, 2),
                "low": round(v.box_low, 2),
                "height": round(v.box_high - v.box_low, 2),
                "bars": v.box_bars,
            })),
            "session_utc": {
                "box_start": fmt_hhmm(box_start),
                "box_end": fmt_hhmm(box_end),
                "entry_until": fmt_hhmm(entry_end),
                "flat": fmt_hhmm(cutoff),
            },
            "ticket": ticket,
            "open_r": view
                .as_ref()
                .filter(|v| v.phase == "in_trade")
                .map(|v| round(v.open_r, 3)),
            "result_r": view.as_ref().and_then(|v| v.result_r).map(|r| round(r, 3)),
        },
        "next_session": next_session,
        "forward": {
            "since": since,
            "signals": journal.signal_days.len(),
            "trades": stats.trades,
            "live_trades": journal.live_trades,
            "backfilled_trades": stats.trades.saturating_sub(journal.live_trades),
            "wins": stats.wins,
            "losses": stats.losses,
            "win_rate": round(stats.win_rate, 1),
            "expectancy_r": round(stats.expectancy_r, 3),
            "profit_factor": if stats.profit_factor.is_finite() {
                json!(round(stats.profit_factor, 2))
            } else {
                Value::Null
            },
            "t_stat": round(stats.t_stat, 2),
            "net_usd": round(stats.net, 2),
            "return_pct": round(stats.return_pct, 2),
            "equity": round(equity, 2),
            "max_drawdown_usd": round(stats.max_drawdown, 2),
            "max_drawdown_pct": round(stats.max_drawdown_pct, 2),
            "max_losing_streak": stats.max_losing_streak,
            "worst_day_pct": round(stats.worst_day_pct, 2),
            "avg_lots": round(stats.avg_lots, 2),
        },
        "recent": recent,
    });

    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, format!("{:#}\n", board))
        .with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

fn fmt_minute(min: u32) -> String {
    format!("{:02}:{:02}", min / 60, min % 60)
}

// ---------------------------------------------------------------------------
// Poll
// ---------------------------------------------------------------------------

fn tick(
    m: &mut Mcp,
    args: &Args,
    paths: &Paths,
    cfg: &OrbConfig,
    journal: &mut Journal,
) -> Result<()> {
    let per = period_secs(&args.period)?;
    let now_server = server_now(m)?;
    let now_server_ts = now_server.and_utc().timestamp();
    let now_utc = now_server_ts - server_utc_shift(now_server_ts, args);

    let mut bars = closed_bars_at(m, &args.period, args.bars, now_server)?;
    for b in &mut bars {
        b.ts -= server_utc_shift(b.ts, args);
    }

    // Walk the days the bars cover, oldest first, so a restart after downtime
    // rebuilds the missing journal entries from the same rule.
    let today = now_utc.div_euclid(86_400);
    let mut days: Vec<i64> = bars.iter().map(|b| b.ts.div_euclid(86_400)).collect();
    days.sort_unstable();
    days.dedup();

    let mut new_entries = 0usize;
    for day in days {
        let Some(view) = day_view(&bars, cfg, day, now_utc) else {
            continue;
        };
        if view.side.is_some() && !journal.signal_days.contains(&day) {
            if let Some(e) = signal_entry(&view) {
                append_entry(&paths.journal, &e)?;
                absorb(journal, e);
                new_entries += 1;
            }
        }
        if view.phase == "closed" && !journal.result_days.contains(&day) {
            if let Some(e) = result_entry(&view, per) {
                append_entry(&paths.journal, &e)?;
                absorb(journal, e);
                new_entries += 1;
            }
        }
    }
    journal.trades.sort_by_key(|t| t.entry_ts);

    let last_bar_utc = bars.last().map(|b| b.ts).unwrap_or(now_utc);
    write_board(
        &paths.board,
        args,
        cfg,
        now_server,
        now_utc,
        last_bar_utc,
        &bars,
        journal,
    )?;

    let phase = day_view(&bars, cfg, today, now_utc)
        .map(|v| v.phase.to_string())
        .unwrap_or_else(|| "idle".into());
    let behind = ((chrono::Utc::now().timestamp() - last_bar_utc) as f64 / 60.0).max(0.0);
    println!(
        "{} utc | server {} | feed {} | phase {} | signals {} | closed {} | {} new",
        fmt_utc(now_utc),
        now_server.format("%H:%M"),
        if feed_is_live(now_server, behind) {
            "live".to_string()
        } else {
            format!("stale {:.0}m", behind)
        },
        phase,
        journal.signal_days.len(),
        journal.trades.len(),
        new_entries
    );
    Ok(())
}

fn resolve_token(args: &Args, root: &Path) -> Result<String> {
    if !args.token.trim().is_empty() {
        return Ok(args.token.trim().to_string());
    }
    let path = args
        .token_file
        .clone()
        .unwrap_or_else(|| root.join("runs/live/mcp_token"));
    let raw = fs::read_to_string(&path).with_context(|| {
        format!(
            "no MCP token: set MT5_MCP_TOKEN or write the token to {}",
            path.display()
        )
    })?;
    let token = raw.trim().to_string();
    if token.is_empty() {
        bail!("token file {} is empty", path.display());
    }
    Ok(token)
}

fn main() -> Result<()> {
    let args = Args::parse();
    let per = period_secs(&args.period)?;

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let token = resolve_token(&args, &root)?;
    let paths = Paths {
        journal: args
            .journal
            .clone()
            .unwrap_or_else(|| root.join("runs/orb/journal.jsonl")),
        board: args
            .board
            .clone()
            .unwrap_or_else(|| root.join("runs/orb/board.json")),
    };

    // The configuration that survived both halves of the M5 split. The
    // backtest's flag names are in docs/strategy-orb.md.
    let mut cfg = OrbConfig {
        box_start_min: 8 * 60,
        box_end_min: 13 * 60,
        cutoff_min: 19 * 60,
        tz: SessionTz::Eu,
        // 45 minutes of swing lookback, expressed in bars of this period.
        swing_bars: (45 * 60 / per) as usize,
        // Half of the five-hour box must have printed.
        min_box_bars: (5 * 60 * 60 / per / 2) as usize,
        max_wait_min: 60,
        rr: 2.0,
        risk_pct: args.risk,
        initial_deposit: args.deposit,
        max_lots: args.max_lots,
        ..Default::default()
    };
    if cfg.swing_bars == 0 {
        cfg.swing_bars = 1;
    }

    let mut journal = load_journal(&paths.journal);
    if !journal.trades.is_empty() {
        println!(
            "journal: {} signals, {} closed trades since {}",
            journal.signal_days.len(),
            journal.trades.len(),
            date_string(journal.trades[0].day)
        );
    }

    let mut mcp = Mcp::connect(&args.url, &token)?;
    println!(
        "logging {} box {}–{} London, entries for {} min, {:.1}R, flat {} London",
        args.period,
        fmt_minute(cfg.box_start_min),
        fmt_minute(cfg.box_end_min),
        cfg.max_wait_min,
        cfg.rr,
        fmt_minute(cfg.cutoff_min)
    );

    loop {
        match tick(&mut mcp, &args, &paths, &cfg, &mut journal) {
            Ok(()) => {
                if args.once {
                    return Ok(());
                }
            }
            Err(e) => {
                eprintln!("poll error: {e:#}");
                if args.once {
                    return Err(e);
                }
                // The terminal drops idle sessions when it restarts.
                match Mcp::connect(&args.url, &token) {
                    Ok(m) => mcp = m,
                    Err(e2) => eprintln!("reconnect failed: {e2:#}"),
                }
            }
        }
        thread::sleep(Duration::from_secs(args.poll));
    }
}

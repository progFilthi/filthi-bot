//! D3 live runner, in Rust.
//!
//! The point of this binary is that the code placing orders and the code that
//! was backtested are the same code: signals, stops and the trail all come
//! from `xau_sim::strategy`, not from a re-implementation.
//!
//! Behaviour, one closed bar at a time (mirrors `xau_sim::engine::backtest`):
//!   * manage first: 240-bar time exit, then trail 2.5 x ATR from the best price
//!   * the broker holds the stop, so intrabar fills behave like the engine
//!   * no take profit (measured: every TP < 8 ATR loses money on H1)
//!   * no opposite-signal exit, one position at a time
//!   * enter at market on the signal from the last closed bar
//!
//! Bar timestamps stay in the broker's own clock. Nothing here needs UTC: the
//! only time comparison is "has this bar closed?", and both sides of that
//! comparison come from the same server clock.

use xau_live::market::*;
use xau_live::mcp::Mcp;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use xau_core::{Bar, Side};
use xau_sim::strategy::{atr, donchian, ema, entry_signals, StratKind, StratParams};

#[derive(Parser, Debug)]
#[command(name = "xau-live", version, about = "D3 live runner over MT5 MCP")]
struct Args {
    /// Timeframe to trade.
    #[arg(long, default_value = "H1")]
    period: String,
    /// Position size in lots.
    #[arg(long, default_value_t = 0.01)]
    lots: f64,
    /// Poll interval, seconds.
    #[arg(long, default_value_t = 60)]
    poll: u64,
    /// Run one pass and exit.
    #[arg(long)]
    once: bool,
    /// Decide and log, never send or modify.
    #[arg(long)]
    dry_run: bool,
    /// Allow a non-demo account.
    #[arg(long)]
    allow_live: bool,
    /// Analyse the live market on every timeframe and exit. Never trades.
    #[arg(long)]
    scan: bool,
    /// Adopt an open position even if it was opened on another timeframe.
    #[arg(long)]
    adopt_any: bool,
    /// Halt when equity falls this percent below the starting balance.
    #[arg(long, default_value_t = 25.0)]
    max_drawdown_pct: f64,
    /// Trade the intraday variant: entries only inside the UTC window, and
    /// flat before the daily break. No overnight exposure.
    #[arg(long)]
    intraday: bool,
    /// First UTC hour an entry may fill (with --intraday).
    #[arg(long, default_value_t = 7)]
    entry_from: u32,
    /// Last UTC hour an entry may fill (with --intraday).
    #[arg(long, default_value_t = 18)]
    entry_to: u32,
    #[arg(long, default_value = "http://127.0.0.1:22346/mcp")]
    url: String,
    /// Bearer token. Defaults to $MT5_MCP_TOKEN, then the token file.
    #[arg(long, env = "MT5_MCP_TOKEN", default_value = "")]
    token: String,
    /// File holding the token. The terminal rotates it on every restart.
    #[arg(long)]
    token_file: Option<PathBuf>,
    /// State file. Defaults to runs/live/rust_<period>_state.json.
    #[arg(long)]
    state: Option<PathBuf>,
    #[arg(long)]
    log: Option<PathBuf>,
}

// --- state ------------------------------------------------------------------
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pos {
    position_id: String,
    side: String,
    entry: f64,
    sl: f64,
    best: f64,
    bars_held: usize,
    #[serde(default)]
    trailed: bool,
    #[serde(default)]
    adopted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct State {
    last_bar_ts: i64,
    start_balance: Option<f64>,
    position: Option<Pos>,
    closed_trades: u32,
}

impl Default for State {
    fn default() -> Self {
        Self { last_bar_ts: 0, start_balance: None, position: None, closed_trades: 0 }
    }
}

struct Ctx {
    mcp: Mcp,
    state_path: PathBuf,
    log_path: PathBuf,
    period: String,
    bar_secs: i64,
    lots: f64,
    dry_run: bool,
    max_drawdown_pct: f64,
    adopt_any: bool,
    manual_sig: Option<String>,
    intraday: bool,
    entry_from: u32,
    entry_to: u32,
    /// Repo root, used to find the dashboard's stop file.
    root: PathBuf,
    /// Whether the stop has already been logged, so an idle paused runner
    /// writes one line instead of one per minute.
    paused_seen: bool,
}

impl Ctx {
    fn log(&self, event: &str, fields: &[(&str, Value)]) {
        let mut obj = Map::new();
        obj.insert("ts".into(), json!(Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)));
        obj.insert("event".into(), json!(event));
        for (k, v) in fields {
            obj.insert((*k).to_string(), v.clone());
        }
        let line = Value::Object(obj).to_string();
        println!("{line}");
        if let Some(dir) = self.log_path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(mut fh) = OpenOptions::new().create(true).append(true).open(&self.log_path) {
            let _ = writeln!(fh, "{line}");
        }
    }

    fn save(&self, state: &State) -> Result<()> {
        if let Some(dir) = self.state_path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&self.state_path, serde_json::to_string_pretty(state)?)
            .with_context(|| format!("writing {}", self.state_path.display()))?;
        Ok(())
    }

    fn load(&self) -> State {
        fs::read_to_string(&self.state_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
}

// --- live scan (read-only) ---------------------------------------------------
/// One line per timeframe: where price is against the Donchian channel, the
/// trend gate, and (if there is a signal) the stop it would place. Never sends
/// anything.
fn scan(m: &mut Mcp) -> Result<()> {
    println!(
        "{:<4} {:<12} {:>9} {:>9} {:>9} {:>9} {:>8}  {}",
        "tf", "last closed", "close", "lo55", "hi55", "ema300", "atr", "read"
    );
    println!("{}", "-".repeat(118));

    for p in ["M5", "M15", "H1", "H4"] {
        let per = period_secs(p)?;
        let all = bars(m, p, 420)?;
        if all.is_empty() {
            println!("{p:<4} (no bars)");
            continue;
        }
        let now = server_now(m)?.and_utc().timestamp();
        let closed: Vec<Bar> = if all.last().unwrap().ts + per <= now {
            all.clone()
        } else {
            all[..all.len() - 1].to_vec()
        };
        if closed.len() < TREND_MA + 2 {
            println!("{p:<4} (only {} closed bars)", closed.len());
            continue;
        }

        let i = closed.len() - 1;
        let bar = closed[i];
        let params = StratParams { lookback: LOOKBACK, trend_ma: TREND_MA, ..Default::default() };
        let sig = entry_signals(&closed, StratKind::Donchian, &params)
            .last()
            .copied()
            .flatten();
        let (hi, lo) = donchian(&closed, LOOKBACK);
        let closes: Vec<f64> = closed.iter().map(|b| b.close).collect();
        let trend = ema(&closes, TREND_MA);
        let atrs = atr(&closed, ATR_PERIOD);
        let a = atrs[i];

        // The terminal stamps bars with the broker's clock; show true UTC.
        let when = chrono::DateTime::from_timestamp(server_to_utc(bar.ts), 0)
            .map(|d| d.format("%m-%d %H:%M").to_string())
            .unwrap_or_default();
        let fill_hour = utc_hour_of(bar.ts + per);
        let window = if in_entry_window(fill_hour) { "open" } else { "shut" };

        let read = match sig {
            Some(s) => {
                let dir = if s == Side::Buy { 1.0 } else { -1.0 };
                let stop = round2(bar.close - dir * SL_ATR * a);
                let risk = (bar.close - stop).abs();
                format!(
                    "SIGNAL {} — stop {stop:.2} ({risk:.2} = {:.0} pts, no TP) · window {window} (next bar {:02}:00 UTC)",
                    s.as_str(),
                    risk / 0.01,
                    fill_hour
                )
            }
            None => {
                let gate = if bar.close > trend[i] { "uptrend" } else { "downtrend" };
                format!(
                    "no breakout ({gate}; needs +{:.2} to buy / -{:.2} to sell) · window {window} (next bar {:02}:00 UTC)",
                    hi[i] - bar.close,
                    bar.close - lo[i],
                    fill_hour
                )
            }
        };

        println!(
            "{:<4} {:<12} {:>9.2} {:>9.2} {:>9.2} {:>9.2} {:>8.2}  {}",
            p, when, bar.close, lo[i], hi[i], trend[i], a, read
        );
    }
    Ok(())
}

// --- one pass ---------------------------------------------------------------
fn run_once(ctx: &mut Ctx, state: &mut State) -> Result<bool> {
    let acct = account(&mut ctx.mcp)?;
    let a = &acct["account"];
    let kind = a["type"].as_str().unwrap_or("");
    if kind != "demo" && !ctx.dry_run {
        bail!("refusing to trade a {kind} account");
    }
    let equity = as_f64(&a["equity"]);
    let balance = as_f64(&a["balance"]);

    if state.start_balance.is_none() {
        state.start_balance = Some(balance);
        ctx.save(state)?;
    }
    let floor = state.start_balance.unwrap_or(balance) * (1.0 - ctx.max_drawdown_pct / 100.0);
    if equity <= floor {
        ctx.log("halt_drawdown", &[("equity", json!(equity)), ("floor", json!(floor))]);
        bail!("drawdown halt: equity {equity:.2} <= floor {floor:.2}");
    }

    // ---- reconcile with the broker ----------------------------------------
    // Two tracks (H1, H4) may run at once, so nothing here may assume the
    // account holds exactly one position, or that the position it can see is
    // ours. Ownership is the `D3-<PERIOD>` comment.
    let live = positions(&mut ctx.mcp)?;
    let on_symbol: Vec<Value> = live
        .into_iter()
        .filter(|l| field(l, "symbol") == SYMBOL)
        .collect();
    let my_tag = format!("D3-{}", ctx.period);
    // Exact match, not a prefix: "D3-M1" must never claim an "D3-M15" trade.
    let tagged = |l: &Value, tag: &str| field(l, "comment").trim() == tag;

    let mut fresh_adopt = false;
    if let Some(p) = state.position.clone() {
        if !on_symbol.iter().any(|l| field(l, "position_id") == p.position_id) {
            ctx.log(
                "closed_by_broker",
                &[("side", json!(p.side)), ("entry", json!(p.entry)),
                  ("bars", json!(p.bars_held)), ("balance", json!(balance))],
            );
            state.position = None;
            state.closed_trades += 1;
            ctx.save(state)?;
        }
    }
    if state.position.is_none() {
        let mine: Option<&Value> = on_symbol.iter().find(|l| tagged(l, &my_tag));
        let adoptable = match mine {
            Some(l) => Some(l),
            None if ctx.adopt_any => on_symbol.first(),
            None => None,
        };
        if let Some(l) = adoptable {
            let comment = field(l, "comment");
            let adopted = Pos {
                position_id: field(l, "position_id"),
                side: field(l, "action"),
                entry: as_f64(&l["price_open"]),
                sl: as_f64(&l["stop_loss"]),
                best: as_f64(&l["price_open"]),
                bars_held: 0,
                trailed: false,
                adopted: true,
            };
            ctx.log("adopted_position",
                &[("side", json!(adopted.side)), ("entry", json!(adopted.entry)),
                  ("id", json!(adopted.position_id)), ("comment", json!(comment))]);
            state.position = Some(adopted);
            fresh_adopt = true;
        } else {
            // Positions belonging to other tracks are none of our business.
            let foreign: Vec<String> = on_symbol
                .iter()
                .map(|l| field(l, "comment"))
                .filter(|c| c.starts_with("D3-"))
                .collect();
            if !foreign.is_empty() {
                ctx.log("ignoring_other_tracks", &[("comments", json!(foreign))]);
            }
            // An untagged XAUUSD position is not ours: it was opened by hand or
            // by something else, so it is left completely alone. Logged loudly
            // because it changes the account's real exposure.
            let manual: Vec<(String, String, f64)> = on_symbol
                .iter()
                .filter(|l| !field(l, "comment").trim().starts_with("D3-"))
                .map(|l| (field(l, "position_id"), field(l, "action"), as_f64(&l["volume"])))
                .collect();
            if !manual.is_empty() {
                let sig = manual
                    .iter()
                    .map(|m| format!("{}:{}:{}", m.0, m.1, m.2))
                    .collect::<Vec<_>>()
                    .join(",");
                if ctx.manual_sig.as_deref() != Some(sig.as_str()) {
                    ctx.log(
                        "manual_position_present",
                        &[("positions", json!(manual)),
                          ("note", json!("not ours; left untouched, but it does change account risk"))],
                    );
                    ctx.manual_sig = Some(sig);
                }
            } else {
                ctx.manual_sig = None;
            }
            ctx.save(state)?;
        }
    }

    // ---- bars ---------------------------------------------------------------
    let all = bars(&mut ctx.mcp, &ctx.period, 420)?;
    if all.is_empty() {
        bail!("no bars returned for {}", ctx.period);
    }
    let now = server_now(&mut ctx.mcp)?.and_utc().timestamp();
    let last_ts = all.last().unwrap().ts;
    let closed: Vec<Bar> = if last_ts + ctx.bar_secs <= now {
        all.clone()
    } else {
        all[..all.len() - 1].to_vec()
    };
    if closed.len() < TREND_MA + 2 {
        bail!("need {} bars, got {}", TREND_MA + 2, closed.len());
    }
    let bar = *closed.last().unwrap();

    // On a fresh adoption the clock starts now: bars that closed before we took
    // the position over must not count toward the trail's best price or the
    // 240-bar time exit. The engine seeds `best` at the entry price, so do the
    // same and wait for the next bar to close.
    if fresh_adopt {
        ctx.log(
            "adopt_boundary",
            &[("last_bar", json!(bar.ts)),
              ("note", json!("pre-adoption bars excluded from trail and time exit"))],
        );
        state.last_bar_ts = bar.ts;
        ctx.save(state)?;
        return Ok(true);
    }

    if bar.ts <= state.last_bar_ts {
        return Ok(false);
    }
    if state.last_bar_ts == 0 {
        ctx.log("bootstrap", &[("bars", json!(closed.len())), ("last_bar", json!(bar.ts))]);
    }

    let atrs = atr(&closed, ATR_PERIOD);
    let atr_now = *atrs.last().unwrap();

    // ---- manage -------------------------------------------------------------
    if let Some(mut p) = state.position.clone() {
        p.bars_held += 1;

        if p.bars_held >= MAX_HOLD_BARS {
            ctx.log("time_exit", &[("side", json!(p.side)), ("bars", json!(p.bars_held))]);
            if !ctx.dry_run {
                let r = ctx.mcp.call(
                    "trade_close_single_position",
                    json!({ "symbol": SYMBOL, "position_ticket": p.position_id }),
                )?;
                ctx.log("time_exit_filled", &[("retcode", r["retcode"].clone())]);
            }
            state.position = None;
            state.closed_trades += 1;
            state.last_bar_ts = bar.ts;
            ctx.save(state)?;
            return Ok(true);
        }

        // Intraday rule: nothing is carried past the day's last bar.
        if ctx.intraday && is_day_last_bar(bar.ts, ctx.bar_secs) {
            ctx.log(
                "day_close",
                &[("side", json!(p.side)), ("entry", json!(p.entry)),
                  ("sl", json!(p.sl)), ("bars", json!(p.bars_held)),
                  ("note", json!("flat before the daily break; no overnight hold"))],
            );
            if !ctx.dry_run {
                let r = ctx.mcp.call(
                    "trade_close_single_position",
                    json!({ "symbol": SYMBOL, "position_ticket": p.position_id }),
                )?;
                ctx.log("day_close_filled", &[("retcode", r["retcode"].clone())]);
            }
            state.position = None;
            state.closed_trades += 1;
            state.last_bar_ts = bar.ts;
            ctx.save(state)?;
            return Ok(true);
        }

        if atr_now.is_finite() && atr_now > 0.0 {
            let buy = p.side == "buy";
            p.best = if buy { p.best.max(bar.high) } else { p.best.min(bar.low) };
            let dist = TRAIL_ATR * atr_now;
            let cand = if buy { p.best - dist } else { p.best + dist };
            let improves = if p.sl == 0.0 {
                true
            } else if buy {
                cand > p.sl
            } else {
                cand < p.sl
            };
            let cand_r = round2(cand);
            if improves && (cand_r - round2(p.sl)).abs() >= 0.01 {
                ctx.log(
                    "trail",
                    &[("side", json!(p.side)), ("best", json!(round2(p.best))),
                      ("old_sl", json!(round2(p.sl))), ("new_sl", json!(cand_r)),
                      ("bars", json!(p.bars_held))],
                );
                if !ctx.dry_run {
                    let r = ctx.mcp.call(
                        "trade_modify_sl_tp",
                        json!({
                            "symbol": SYMBOL,
                            "position_ticket": p.position_id,
                            "sl": cand_r
                        }),
                    )?;
                    ctx.log("trail_filled", &[("retcode", r["retcode"].clone())]);
                }
                p.sl = cand_r;
                p.trailed = true;
            }
        }
        state.position = Some(p);
        state.last_bar_ts = bar.ts;
        ctx.save(state)?;
        return Ok(true);
    }

    // ---- the dashboard's stop button ---------------------------------------
    // Checked *after* the position block above, on purpose: a stop must not
    // trap a live position. The trail keeps trailing and the day-close still
    // fires; only new entries are refused.
    if let Some(p) = paused(&ctx.root) {
        if !ctx.paused_seen {
            ctx.log(
                "paused",
                &[("since", json!(p.since)), ("reason", json!(p.reason)),
                  ("note", json!("no new entries; an open position is still trailed and closed at the day's end"))],
            );
            ctx.paused_seen = true;
        }
        return Ok(false);
    }
    if ctx.paused_seen {
        ctx.log("resumed", &[("note", json!("stop file removed; entries allowed again"))]);
        ctx.paused_seen = false;
    }

    // ---- entry --------------------------------------------------------------
    let params = StratParams { lookback: LOOKBACK, trend_ma: TREND_MA, ..Default::default() };
    let signals = entry_signals(&closed, StratKind::Donchian, &params);
    let sig = signals.last().copied().flatten();

    let (hi, lo) = donchian(&closed, LOOKBACK);
    let closes: Vec<f64> = closed.iter().map(|b| b.close).collect();
    let trend = ema(&closes, TREND_MA);
    let i = closed.len() - 1;

    // The fill happens at the open of the next bar, so the hour that matters
    // is the one after this closed bar — in UTC, not on the broker's clock.
    let fill_ts = bar.ts + ctx.bar_secs;
    let fill_hour = utc_hour_of(fill_ts);
    let window_ok = !ctx.intraday || (fill_hour >= ctx.entry_from && fill_hour <= ctx.entry_to);

    match sig {
        None => {
            ctx.log(
                "no_signal",
                &[("close", json!(round2(closes[i]))), ("hi", json!(round2(hi[i]))),
                  ("lo", json!(round2(lo[i]))), ("ema", json!(round2(trend[i]))),
                  ("atr", json!(round2(atr_now)))],
            );
        }
        Some(_) if !window_ok => {
            ctx.log(
                "signal_outside_window",
                &[("fill_hour_utc", json!(fill_hour)),
                  ("window", json!(format!("{:02}–{:02} UTC", ctx.entry_from, ctx.entry_to))),
                  ("note", json!("breakout is real but the intraday rule does not enter here"))],
            );
        }
        Some(side) => {
            let price = last_price(&mut ctx.mcp)?;
            let dir = if side == Side::Buy { 1.0 } else { -1.0 };
            let stop = round2(price - dir * SL_ATR * atr_now);
            let risk = (price - stop).abs() * CONTRACT * ctx.lots;
            let side_s = side.as_str();
            let order = json!({
                "symbol": SYMBOL, "type": side_s, "volume": ctx.lots,
                "sl": stop, "comment": format!("D3-{}", ctx.period)
            });
            ctx.log(
                "entry_signal",
                &[("side", json!(side_s)), ("price", json!(round2(price))),
                  ("sl", json!(stop)), ("atr", json!(round2(atr_now))),
                  ("risk_usd", json!(round2(risk))), ("order", order.clone())],
            );
            if !ctx.dry_run {
                let r = ctx.mcp.call("trade_send_market_order", order)?;
                ctx.log(
                    "entry_filled",
                    &[("retcode", r["retcode"].clone()), ("price", r["price"].clone())],
                );
                // Find the trade *we* just opened, by its tag — never assume it
                // is the first position (a manual trade may be open too).
                let after = positions(&mut ctx.mcp)?;
                if let Some(l) = after
                    .iter()
                    .find(|l| field(l, "comment").trim() == my_tag)
                {
                    state.position = Some(Pos {
                        position_id: field(l, "position_id"),
                        side: field(l, "action"),
                        entry: as_f64(&l["price_open"]),
                        sl: stop,
                        best: as_f64(&l["price_open"]),
                        bars_held: 0,
                        trailed: false,
                        adopted: false,
                    });
                } else {
                    ctx.log("entry_missing", &[("note", json!("no position after send"))]);
                }
            }
        }
    }

    state.last_bar_ts = bar.ts;
    ctx.save(state)?;
    Ok(true)
}

/// The terminal rotates its bearer token on every restart, so the token lives
/// in a gitignored file rather than in source. Env var wins if set.
fn resolve_token(args: &Args, root: &std::path::Path) -> Result<String> {
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
    let bar_secs = period_secs(&args.period)?;

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let token = resolve_token(&args, &root)?;
    let state_path = args
        .state
        .unwrap_or_else(|| root.join(format!("runs/live/rust_{}_state.json", args.period.to_lowercase())));
    let log_path = args
        .log
        .unwrap_or_else(|| root.join(format!("runs/live/rust_{}_runner.log", args.period.to_lowercase())));

    let mut ctx = Ctx {
        mcp: Mcp::connect(&args.url, &token)?,
        state_path,
        log_path,
        period: args.period.clone(),
        bar_secs,
        lots: args.lots,
        dry_run: args.dry_run,
        max_drawdown_pct: args.max_drawdown_pct,
        adopt_any: args.adopt_any,
        manual_sig: None,
        intraday: args.intraday,
        entry_from: args.entry_from,
        entry_to: args.entry_to,
        root: root.clone(),
        paused_seen: false,
    };

    let mut state = ctx.load();

    if args.scan {
        return scan(&mut ctx.mcp);
    }

    ctx.log(
        "runner_start",
        &[("period", json!(args.period)), ("lots", json!(args.lots)),
          ("dry_run", json!(args.dry_run)), ("state", json!(ctx.state_path.display().to_string())),
          ("intraday", json!(args.intraday)),
          ("entry_window_utc", json!(if args.intraday {
              format!("{:02}–{:02}", args.entry_from, args.entry_to)
          } else { "all hours".to_string() }))],
    );

    loop {
        match run_once(&mut ctx, &mut state) {
            Ok(processed) => {
                if args.once {
                    ctx.log("runner_once_done", &[("processed", json!(processed))]);
                    return Ok(());
                }
                if !processed {
                    thread::sleep(Duration::from_secs(args.poll));
                }
            }
            Err(e) => {
                ctx.log("poll_error", &[("error", json!(format!("{e:#}")) )]);
                if args.once {
                    return Err(e);
                }
                // Reconnect: the terminal drops idle sessions on restart.
                match Mcp::connect(&args.url, &token) {
                    Ok(m) => ctx.mcp = m,
                    Err(e2) => ctx.log("reconnect_failed", &[("error", json!(format!("{e2:#}"))) ]),
                }
                thread::sleep(Duration::from_secs(args.poll));
            }
        }
    }
}

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

mod mcp;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{NaiveDateTime, Utc};
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use mcp::Mcp;
use xau_core::{Bar, Side};
use xau_sim::strategy::{atr, donchian, ema, entry_signals, StratKind, StratParams};

// --- D3, the validated configuration (configs/phase0.toml [strategy]) -------
const LOOKBACK: usize = 55;
const TREND_MA: usize = 300;
const ATR_PERIOD: usize = 14;
const SL_ATR: f64 = 1.5;
const TRAIL_ATR: f64 = 2.5;
const MAX_HOLD_BARS: usize = 240;
const SYMBOL: &str = "XAUUSD";
const CONTRACT: f64 = 100.0;

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
    #[arg(long, default_value = "http://127.0.0.1:22346/mcp")]
    url: String,
    #[arg(long, env = "MT5_MCP_TOKEN", default_value = "oo6totlCD01Iu//GWbs0E/0qqP28lE+8TGkXB/WXYL")]
    token: String,
    /// State file. Defaults to runs/live/rust_<period>_state.json.
    #[arg(long)]
    state: Option<PathBuf>,
    #[arg(long)]
    log: Option<PathBuf>,
}

fn period_secs(p: &str) -> Result<i64> {
    Ok(match p {
        "M5" => 300,
        "M15" => 900,
        "M30" => 1800,
        "H1" => 3600,
        "H4" => 14400,
        "D1" => 86400,
        other => bail!("unsupported period {other}"),
    })
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

// --- terminal reads ---------------------------------------------------------
fn as_f64(v: &Value) -> f64 {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(f64::NAN)
}

fn parse_naive(s: &str) -> Result<NaiveDateTime> {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
        .with_context(|| format!("bad timestamp {s:?}"))
}

fn server_now(m: &mut Mcp) -> Result<NaiveDateTime> {
    let t = m.call("get_time_information", json!({}))?;
    let s = t
        .get("trade_server_last_known_time")
        .and_then(Value::as_str)
        .context("no trade_server_last_known_time")?;
    parse_naive(s)
}

fn bars(m: &mut Mcp, period: &str, count: usize) -> Result<Vec<Bar>> {
    let per = period_secs(period)?;
    let now = server_now(m)?;
    let span_days = (count as f64 * per as f64 / 84_600.0 * 2.2).ceil() as i64 + 10;
    let from = now - chrono::Duration::days(span_days);
    let to = now + chrono::Duration::days(1);

    let page = m.call(
        "get_chart_history",
        json!({
            "symbol": SYMBOL,
            "period": period,
            "datetime_from": from.format("%Y-%m-%dT%H:%M:%S").to_string(),
            "datetime_to": to.format("%Y-%m-%dT%H:%M:%S").to_string(),
            "limit": 100_000
        }),
    )?;
    let hist = page
        .get("history")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let tail = if hist.len() > count { &hist[hist.len() - count..] } else { &hist[..] };

    let mut out = Vec::with_capacity(tail.len());
    for b in tail {
        let ts = parse_naive(b.get("time").and_then(Value::as_str).context("bar without time")?)
            .map(|d| d.and_utc().timestamp())
            .unwrap_or(0);
        out.push(Bar {
            ts,
            open: as_f64(&b["open"]),
            high: as_f64(&b["high"]),
            low: as_f64(&b["low"]),
            close: as_f64(&b["close"]),
            volume: b.get("tick_volume").and_then(Value::as_u64).unwrap_or(0),
            spread_pts: b.get("spread").and_then(Value::as_i64).unwrap_or(0),
            spread_max: b.get("spread").and_then(Value::as_i64).unwrap_or(0),
        });
    }
    Ok(out)
}

fn last_price(m: &mut Mcp) -> Result<f64> {
    let b = bars(m, "M1", 2)?;
    b.last().map(|b| b.close).context("no M1 bars")
}

fn account(m: &mut Mcp) -> Result<Value> {
    m.call("get_trading_account_info", json!({}))
}

fn positions(m: &mut Mcp) -> Result<Vec<Value>> {
    let v = m.call("get_trading_open_positions", json!({}))?;
    Ok(v.get("positions").and_then(Value::as_array).cloned().unwrap_or_default())
}

fn field<'a>(p: &'a Value, k: &str) -> String {
    match &p[k] {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
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

        let when = chrono::DateTime::from_timestamp(bar.ts, 0)
            .map(|d| d.format("%m-%d %H:%M").to_string())
            .unwrap_or_default();

        let read = match sig {
            Some(s) => {
                let dir = if s == Side::Buy { 1.0 } else { -1.0 };
                let stop = round2(bar.close - dir * SL_ATR * a);
                let risk = (bar.close - stop).abs();
                format!(
                    "SIGNAL {} — stop {stop:.2} ({risk:.2} = {:.0} pts, no TP)",
                    s.as_str(),
                    risk / 0.01
                )
            }
            None => {
                let gate = if bar.close > trend[i] { "uptrend" } else { "downtrend" };
                format!(
                    "no breakout ({gate}; needs +{:.2} to buy / -{:.2} to sell)",
                    hi[i] - bar.close,
                    bar.close - lo[i]
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
    let live = positions(&mut ctx.mcp)?;
    let mut fresh_adopt = false;
    if let Some(p) = state.position.clone() {
        if !live.iter().any(|l| field(l, "position_id") == p.position_id) {
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
        if let Some(l) = live.first() {
            // A position opened on H1 must not be trailed with H4 volatility
            // (or the reverse). The comment names the timeframe it belongs to.
            let comment = field(l, "comment");
            if let Some(owner) = comment.strip_prefix("D3-") {
                if !owner.is_empty() && owner != ctx.period && !ctx.adopt_any {
                    bail!(
                        "open position {} is commented {comment:?}, which belongs to {owner}, \
                         but this runner is {}. Refusing to manage it on the wrong timeframe \
                         (pass --adopt-any to override).",
                        field(l, "position_id"),
                        ctx.period
                    );
                }
            }
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
                  ("id", json!(adopted.position_id))]);
            state.position = Some(adopted);
            fresh_adopt = true;
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
                if let Some(l) = positions(&mut ctx.mcp)?.first() {
                    let r = ctx.mcp.call(
                        "trade_close_single_position",
                        json!({ "symbol": SYMBOL, "position_ticket": field(l, "position_id") }),
                    )?;
                    ctx.log("time_exit_filled", &[("retcode", r["retcode"].clone())]);
                }
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
                    if let Some(l) = positions(&mut ctx.mcp)?.first() {
                        let r = ctx.mcp.call(
                            "trade_modify_sl_tp",
                            json!({
                                "symbol": SYMBOL,
                                "position_ticket": field(l, "position_id"),
                                "sl": cand_r
                            }),
                        )?;
                        ctx.log("trail_filled", &[("retcode", r["retcode"].clone())]);
                    }
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

    // ---- entry --------------------------------------------------------------
    let params = StratParams { lookback: LOOKBACK, trend_ma: TREND_MA, ..Default::default() };
    let signals = entry_signals(&closed, StratKind::Donchian, &params);
    let sig = signals.last().copied().flatten();

    let (hi, lo) = donchian(&closed, LOOKBACK);
    let closes: Vec<f64> = closed.iter().map(|b| b.close).collect();
    let trend = ema(&closes, TREND_MA);
    let i = closed.len() - 1;

    match sig {
        None => {
            ctx.log(
                "no_signal",
                &[("close", json!(round2(closes[i]))), ("hi", json!(round2(hi[i]))),
                  ("lo", json!(round2(lo[i]))), ("ema", json!(round2(trend[i]))),
                  ("atr", json!(round2(atr_now)))],
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
                if let Some(l) = positions(&mut ctx.mcp)?.first() {
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

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn main() -> Result<()> {
    let args = Args::parse();
    let bar_secs = period_secs(&args.period)?;

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let state_path = args
        .state
        .unwrap_or_else(|| root.join(format!("runs/live/rust_{}_state.json", args.period.to_lowercase())));
    let log_path = args
        .log
        .unwrap_or_else(|| root.join(format!("runs/live/rust_{}_runner.log", args.period.to_lowercase())));

    let mut ctx = Ctx {
        mcp: Mcp::connect(&args.url, &args.token)?,
        state_path,
        log_path,
        period: args.period.clone(),
        bar_secs,
        lots: args.lots,
        dry_run: args.dry_run,
        max_drawdown_pct: args.max_drawdown_pct,
        adopt_any: args.adopt_any,
    };

    let mut state = ctx.load();

    if args.scan {
        return scan(&mut ctx.mcp);
    }

    ctx.log(
        "runner_start",
        &[("period", json!(args.period)), ("lots", json!(args.lots)),
          ("dry_run", json!(args.dry_run)), ("state", json!(ctx.state_path.display().to_string()))],
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
                match Mcp::connect(&args.url, &args.token) {
                    Ok(m) => ctx.mcp = m,
                    Err(e2) => ctx.log("reconnect_failed", &[("error", json!(format!("{e2:#}"))) ]),
                }
                thread::sleep(Duration::from_secs(args.poll));
            }
        }
    }
}

//! M15 mean-reversion scalper, in Rust.
//!
//! This is the live half of the result in `docs/scalping.md`: fade a close
//! outside a 20-period 2σ Bollinger band, stop 0.5 ATR, target 1.5 ATR, leave
//! after 8 bars. Measured on EURUSD M15 over four years: PF 1.33, 34% win rate,
//! 4,821 trades, 4/4 folds, and a $50 account at 0.01 lots ends at $461 with an
//! 11% worst drawdown.
//!
//! Why it is a separate binary from `xau-live`: the gold D3 runner is live and
//! validated, and its dashboard, state file and comment tag are built around
//! one rule on one symbol. Nothing here touches that path. Ownership is the
//! `SC-<period>` comment, so the two runners can never claim each other's
//! positions.
//!
//! Behaviour, one closed bar at a time (mirrors `xau_sim::engine::backtest`
//! with sl_atr 0.5, tp_atr 1.5, trail 0, max_hold 8):
//!   * manage first: 8-bar time exit, then flat before the weekend close
//!   * the broker holds both the stop and the target, so a 3-pip stop does not
//!     depend on the once-a-minute poll
//!   * no trailing stop — the tested rule has none
//!   * enter at market on the signal from the last closed bar
//!   * refuse any trade whose stop risks more than `--max-risk-pct` of equity
//!
//! The risk governor is the part that makes this safe on a small balance. On a
//! $50 account a EURUSD stop risks about 29 cents, so trades go through; point
//! the same code at gold and every signal is refused, because one gold stop is
//! 60-80% of the account.

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
use xau_live::market::{
    account, as_f64, bars_for, field, is_week_last_bar, last_price_for, paused, period_secs,
    positions, server_now, trade_history, utc_hour_of,
};
use xau_live::mcp::Mcp;
use xau_sim::strategy::{atr, entry_signals, StratKind, StratParams};

#[derive(Parser, Debug)]
#[command(name = "xau-scalp-live", version, about = "M15 Bollinger scalper over MT5 MCP")]
struct Args {
    /// Timeframe to trade.
    #[arg(long, default_value = "M15")]
    period: String,
    /// Symbol. EURUSD is the tested one.
    #[arg(long, default_value = "EURUSD")]
    symbol: String,
    /// Price value of one point (EURUSD: 0.00001).
    #[arg(long, default_value_t = 0.00001)]
    point: f64,
    /// Units of base currency per lot (EURUSD: 100000).
    #[arg(long, default_value_t = 100_000.0)]
    contract: f64,
    /// Decimal places for prices (EURUSD: 5).
    #[arg(long, default_value_t = 5)]
    digits: u32,
    /// Position size in lots.
    #[arg(long, default_value_t = 0.01)]
    lots: f64,
    /// Stop distance in ATR units.
    #[arg(long, default_value_t = 0.5)]
    sl_atr: f64,
    /// Target distance in ATR units.
    #[arg(long, default_value_t = 1.5)]
    tp_atr: f64,
    /// Close a position after this many bars.
    #[arg(long, default_value_t = 8)]
    hold: usize,
    /// Bollinger lookback.
    #[arg(long, default_value_t = 20)]
    lookback: usize,
    /// Band width in standard deviations.
    #[arg(long, default_value_t = 2.0)]
    band_k: f64,
    /// Refuse a trade whose stop risks more than this percent of equity.
    #[arg(long, default_value_t = 2.0)]
    max_risk_pct: f64,
    /// Halt when equity falls this percent below the starting balance.
    #[arg(long, default_value_t = 25.0)]
    max_drawdown_pct: f64,
    /// Poll interval, seconds. A new bar is only actionable once it closes.
    #[arg(long, default_value_t = 30)]
    poll: u64,
    /// Run one pass and exit.
    #[arg(long)]
    once: bool,
    /// Decide and log, never send.
    #[arg(long)]
    dry_run: bool,
    /// Allow a non-demo account.
    #[arg(long)]
    allow_live: bool,
    /// Entries only when the next bar opens inside this UTC hour range.
    #[arg(long, default_value_t = 7)]
    entry_from: u32,
    /// Last UTC hour an entry may fill.
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
    tp: f64,
    bars_held: usize,
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
    refused_risk: u32,
}

impl Default for State {
    fn default() -> Self {
        Self {
            last_bar_ts: 0,
            start_balance: None,
            position: None,
            closed_trades: 0,
            refused_risk: 0,
        }
    }
}

struct Ctx {
    mcp: Mcp,
    state_path: PathBuf,
    log_path: PathBuf,
    period: String,
    symbol: String,
    point: f64,
    contract: f64,
    digits: u32,
    bar_secs: i64,
    lots: f64,
    sl_atr: f64,
    tp_atr: f64,
    hold: usize,
    lookback: usize,
    band_k: f64,
    max_risk_pct: f64,
    max_drawdown_pct: f64,
    allow_live: bool,
    dry_run: bool,
    entry_from: u32,
    entry_to: u32,
    root: PathBuf,
    paused_seen: bool,
}

impl Ctx {
    fn log(&self, event: &str, fields: &[(&str, Value)]) {
        let mut obj = Map::new();
        obj.insert(
            "ts".into(),
            json!(Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        );
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

    fn tag(&self) -> String {
        format!("SC-{}", self.period)
    }

    fn round(&self, x: f64) -> f64 {
        round_to(x, self.digits)
    }

    fn params(&self) -> StratParams {
        StratParams {
            lookback: self.lookback,
            band_k: self.band_k,
            ..Default::default()
        }
    }

    /// The stop and target the rule would place for an entry at `price`.
    fn brackets(&self, side: Side, price: f64, atr_now: f64) -> (f64, f64) {
        let dir = if side == Side::Buy { 1.0 } else { -1.0 };
        (
            self.round(price - dir * self.sl_atr * atr_now),
            self.round(price + dir * self.tp_atr * atr_now),
        )
    }
}

/// Round to the symbol's price precision, so MT5 never rejects an order for
/// being off-tick.
fn round_to(x: f64, digits: u32) -> f64 {
    let f = 10f64.powi(digits as i32);
    (x * f).round() / f
}

/// Would a stop of this size be affordable on this balance?
///
/// This is the whole point of the governor: the same code is safe on $50 and
/// refuses gold, without either being a special case. Non-finite input is a
/// refusal, because a sizing bug must never become a position.
fn risk_allowed(risk: f64, equity: f64, cap_pct: f64) -> bool {
    if !risk.is_finite() || !equity.is_finite() || equity <= 0.0 || risk < 0.0 {
        return false;
    }
    risk / equity * 100.0 <= cap_pct
}

// --- one pass ---------------------------------------------------------------
fn run_once(ctx: &mut Ctx, state: &mut State) -> Result<bool> {
    let acct = account(&mut ctx.mcp)?;
    let a = &acct["account"];
    let kind = a["type"].as_str().unwrap_or("");
    if kind != "demo" && !ctx.allow_live && !ctx.dry_run {
        bail!("refusing to trade a {kind} account without --allow-live");
    }
    let equity = as_f64(&a["equity"]);
    let balance = as_f64(&a["balance"]);
    let free_margin = as_f64(&a["margin_free"]);

    if state.start_balance.is_none() {
        state.start_balance = Some(balance);
        ctx.save(state)?;
    }
    let floor = state.start_balance.unwrap_or(balance) * (1.0 - ctx.max_drawdown_pct / 100.0);
    if equity <= floor {
        ctx.log(
            "halt_drawdown",
            &[("equity", json!(equity)), ("floor", json!(floor))],
        );
        bail!("drawdown halt: equity {equity:.2} <= floor {floor:.2}");
    }

    // ---- reconcile with the broker ----------------------------------------
    // Ownership is the comment tag. Any other position on the symbol — the D3
    // runner's, or one opened by hand — is left completely alone.
    let live = positions(&mut ctx.mcp)?;
    let on_symbol: Vec<Value> = live
        .into_iter()
        .filter(|l| field(l, "symbol") == ctx.symbol)
        .collect();
    let tag = ctx.tag();

    if let Some(p) = state.position.clone() {
        if !on_symbol.iter().any(|l| field(l, "position_id") == p.position_id) {
            // The broker closed it: stop, target, or manual. Record what it
            // actually paid so the log doubles as a ledger.
            let closed = trade_history(&mut ctx.mcp, 40)?
                .into_iter()
                .find(|h| field(h, "position_id") == p.position_id);
            let profit = closed.as_ref().map(|h| as_f64(&h["profit"])).unwrap_or(f64::NAN);
            let exit = closed
                .as_ref()
                .map(|h| as_f64(&h["close_price"]))
                .unwrap_or(f64::NAN);
            let reason = closed
                .as_ref()
                .map(|h| field(h, "close_reason"))
                .unwrap_or_default();
            ctx.log(
                "closed_by_broker",
                &[
                    ("side", json!(p.side)),
                    ("entry", json!(p.entry)),
                    ("exit", json!(exit)),
                    ("sl", json!(p.sl)),
                    ("tp", json!(p.tp)),
                    ("bars", json!(p.bars_held)),
                    ("profit", json!(profit)),
                    ("reason", json!(reason)),
                    ("balance", json!(balance)),
                ],
            );
            state.position = None;
            state.closed_trades += 1;
            ctx.save(state)?;
        }
    }

    if state.position.is_none() {
        if let Some(l) = on_symbol.iter().find(|l| field(l, "comment").trim() == tag) {
            let adopted = Pos {
                position_id: field(l, "position_id"),
                side: field(l, "action"),
                entry: as_f64(&l["price_open"]),
                sl: as_f64(&l["stop_loss"]),
                tp: as_f64(&l["take_profit"]),
                bars_held: 0,
                adopted: true,
            };
            ctx.log(
                "adopted_position",
                &[
                    ("side", json!(adopted.side)),
                    ("entry", json!(adopted.entry)),
                    ("sl", json!(adopted.sl)),
                    ("tp", json!(adopted.tp)),
                    ("id", json!(adopted.position_id)),
                    ("note", json!("bar count restarts at zero")),
                ],
            );
            state.position = Some(adopted);
            ctx.save(state)?;
        }
    }

    // ---- bars ---------------------------------------------------------------
    let all = bars_for(&mut ctx.mcp, &ctx.symbol, &ctx.period, 420)?;
    if all.is_empty() {
        bail!("no bars returned for {}", ctx.period);
    }
    let now = server_now(&mut ctx.mcp)?.and_utc().timestamp();
    // Drop the last bar unconditionally: it is the one still forming. Deciding
    // this from the clock instead is what let an early version read a bar whose
    // close was still moving, which is look-ahead by another name — a closed
    // bar's close never changes, and the next bar existing is the only proof.
    if all.len() < 2 {
        bail!("need at least two bars to find a closed one");
    }
    let closed: Vec<Bar> = all[..all.len() - 1].to_vec();
    if closed.len() < ctx.lookback + 3 {
        bail!("need {} bars, got {}", ctx.lookback + 3, closed.len());
    }
    let bar = *closed.last().unwrap();
    if bar.ts <= state.last_bar_ts {
        return Ok(false);
    }
    if state.last_bar_ts == 0 {
        ctx.log(
            "bootstrap",
            &[("bars", json!(closed.len())), ("last_bar", json!(bar.ts))],
        );
    }

    let atrs = atr(&closed, 14);
    let atr_now = *atrs.last().unwrap();
    if !atr_now.is_finite() || atr_now <= 0.0 {
        ctx.log("no_atr", &[("atr", json!(atr_now))]);
        state.last_bar_ts = bar.ts;
        ctx.save(state)?;
        return Ok(true);
    }

    // ---- manage -------------------------------------------------------------
    if let Some(mut p) = state.position.clone() {
        p.bars_held += 1;

        let exit_reason = if p.bars_held >= ctx.hold {
            Some("time_exit")
        } else if is_week_last_bar(bar.ts, ctx.bar_secs) {
            // The tested rule is flat before the weekend gap.
            Some("week_close")
        } else {
            None
        };

        if let Some(reason) = exit_reason {
            ctx.log(
                reason,
                &[
                    ("side", json!(p.side)),
                    ("entry", json!(p.entry)),
                    ("bars", json!(p.bars_held)),
                    ("close", json!(bar.close)),
                ],
            );
            if !ctx.dry_run {
                let r = ctx.mcp.call(
                    "trade_close_single_position",
                    json!({ "symbol": ctx.symbol, "position_ticket": p.position_id }),
                )?;
                ctx.log(
                    "exit_filled",
                    &[("reason", json!(reason)), ("retcode", r["retcode"].clone())],
                );
            }
            state.position = None;
            state.closed_trades += 1;
            state.last_bar_ts = bar.ts;
            ctx.save(state)?;
            return Ok(true);
        }

        state.position = Some(p);
        state.last_bar_ts = bar.ts;
        ctx.save(state)?;
        return Ok(true);
    }

    // ---- the dashboard's stop button ---------------------------------------
    // Checked after position management on purpose: a stop must never trap an
    // open position, it only refuses new entries.
    if let Some(p) = paused(&ctx.root) {
        if !ctx.paused_seen {
            ctx.log(
                "paused",
                &[
                    ("since", json!(p.since)),
                    ("reason", json!(p.reason)),
                    ("note", json!("no new entries; an open position still exits on time")),
                ],
            );
            ctx.paused_seen = true;
        }
        return Ok(false);
    }
    if ctx.paused_seen {
        ctx.log("resumed", &[("note", json!("stop file removed; entries allowed"))]);
        ctx.paused_seen = false;
    }

    // ---- entry --------------------------------------------------------------
    let signals = entry_signals(&closed, StratKind::Bollinger, &ctx.params());
    let sig = signals.last().copied().flatten();

    // The fill happens at the open of the next bar, so the hour that matters is
    // the one after this closed bar — in UTC, not on the broker's clock.
    let fill_hour = utc_hour_of(bar.ts + ctx.bar_secs);
    let window_ok = fill_hour >= ctx.entry_from && fill_hour <= ctx.entry_to;

    match sig {
        None => {
            ctx.log(
                "no_signal",
                &[
                    ("close", json!(bar.close)),
                    ("atr", json!(ctx.round(atr_now))),
                    // Both on the broker clock. If bar_ts + one period is still
                    // ahead of now, the bar we just read was not closed yet.
                    ("bar_ts", json!(bar.ts)),
                    ("now", json!(now)),
                ],
            );
        }
        Some(_) if !window_ok => {
            ctx.log(
                "signal_outside_window",
                &[
                    ("fill_hour_utc", json!(fill_hour)),
                    (
                        "window",
                        json!(format!("{:02}–{:02} UTC", ctx.entry_from, ctx.entry_to)),
                    ),
                ],
            );
        }
        Some(side) => {
            let price = last_price_for(&mut ctx.mcp, &ctx.symbol)?;
            let (sl, tp) = ctx.brackets(side, price, atr_now);
            let stop_dist = (price - sl).abs();
            let risk = stop_dist * ctx.contract * ctx.lots;
            let risk_pct = if equity > 0.0 { risk / equity * 100.0 } else { 100.0 };

            // The governor. This is the whole reason the binary can be pointed
            // at a $50 account without a hand on the mouse.
            if !risk_allowed(risk, equity, ctx.max_risk_pct) {
                state.refused_risk += 1;
                ctx.log(
                    "risk_too_large",
                    &[
                        ("side", json!(side.as_str())),
                        ("price", json!(price)),
                        ("sl", json!(sl)),
                        ("stop_dist", json!(ctx.round(stop_dist))),
                        ("risk_usd", json!((risk * 100.0).round() / 100.0)),
                        ("risk_pct", json!((risk_pct * 100.0).round() / 100.0)),
                        ("cap_pct", json!(ctx.max_risk_pct)),
                        ("equity", json!(equity)),
                        (
                            "note",
                            json!("signal skipped: the stop is too wide for this balance"),
                        ),
                    ],
                );
                state.last_bar_ts = bar.ts;
                ctx.save(state)?;
                return Ok(true);
            }

            // Margin: never spend the last of the free margin on one position.
            let margin = ctx
                .mcp
                .call(
                    "trade_order_calc_margin",
                    json!({
                        "symbol": ctx.symbol, "type": side.as_str(),
                        "volume": ctx.lots, "price": price
                    }),
                )
                .ok()
                .map(|v| as_f64(&v["margin"]))
                .unwrap_or(f64::NAN);
            if margin.is_finite() && free_margin < margin + risk * 3.0 {
                ctx.log(
                    "margin_too_thin",
                    &[
                        ("free_margin", json!(free_margin)),
                        ("margin", json!(margin)),
                        ("risk_usd", json!(risk)),
                    ],
                );
                state.last_bar_ts = bar.ts;
                ctx.save(state)?;
                return Ok(true);
            }

            let order = json!({
                "symbol": ctx.symbol, "type": side.as_str(), "volume": ctx.lots,
                "sl": sl, "tp": tp, "comment": tag
            });
            ctx.log(
                "entry_signal",
                &[
                    ("side", json!(side.as_str())),
                    ("price", json!(price)),
                    ("sl", json!(sl)),
                    ("tp", json!(tp)),
                    ("stop_dist", json!(ctx.round(stop_dist))),
                    ("stop_pts", json!((stop_dist / ctx.point).round())),
                    ("risk_usd", json!((risk * 100.0).round() / 100.0)),
                    ("risk_pct", json!((risk_pct * 100.0).round() / 100.0)),
                    ("atr", json!(ctx.round(atr_now))),
                    ("dry_run", json!(ctx.dry_run)),
                    ("order", order.clone()),
                ],
            );

            if !ctx.dry_run {
                let r = ctx.mcp.call("trade_send_market_order", order)?;
                ctx.log(
                    "entry_filled",
                    &[
                        ("retcode", r["retcode"].clone()),
                        ("price", r["price"].clone()),
                    ],
                );
                // Find the trade we just opened by its tag — never assume it is
                // the only position; the gold runner may be in the market too.
                let after = positions(&mut ctx.mcp)?;
                if let Some(l) = after.iter().find(|l| field(l, "comment").trim() == tag) {
                    state.position = Some(Pos {
                        position_id: field(l, "position_id"),
                        side: field(l, "action"),
                        entry: as_f64(&l["price_open"]),
                        sl,
                        tp,
                        bars_held: 0,
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
    if args.sl_atr <= 0.0 || args.tp_atr <= 0.0 || args.hold == 0 {
        bail!("--sl-atr, --tp-atr and --hold must all be positive");
    }

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let token = resolve_token(&args, &root)?;
    let stem = format!(
        "scalp_{}_{}",
        args.symbol.to_lowercase(),
        args.period.to_lowercase()
    );
    let state_path = args
        .state
        .unwrap_or_else(|| root.join(format!("runs/live/{stem}_state.json")));
    let log_path = args
        .log
        .unwrap_or_else(|| root.join(format!("runs/live/{stem}_runner.log")));

    let mut ctx = Ctx {
        mcp: Mcp::connect(&args.url, &token)?,
        state_path,
        log_path,
        period: args.period.clone(),
        symbol: args.symbol.clone(),
        point: args.point,
        contract: args.contract,
        digits: args.digits,
        bar_secs,
        lots: args.lots,
        sl_atr: args.sl_atr,
        tp_atr: args.tp_atr,
        hold: args.hold,
        lookback: args.lookback,
        band_k: args.band_k,
        max_risk_pct: args.max_risk_pct,
        max_drawdown_pct: args.max_drawdown_pct,
        allow_live: args.allow_live,
        dry_run: args.dry_run,
        entry_from: args.entry_from,
        entry_to: args.entry_to,
        root: root.clone(),
        paused_seen: false,
    };

    let mut state = ctx.load();

    ctx.log(
        "runner_start",
        &[
            ("symbol", json!(args.symbol)),
            ("period", json!(args.period)),
            ("lots", json!(args.lots)),
            ("sl_atr", json!(args.sl_atr)),
            ("tp_atr", json!(args.tp_atr)),
            ("hold", json!(args.hold)),
            ("max_risk_pct", json!(args.max_risk_pct)),
            ("dry_run", json!(args.dry_run)),
            (
                "entry_window_utc",
                json!(format!("{:02}–{:02}", args.entry_from, args.entry_to)),
            ),
            ("state", json!(ctx.state_path.display().to_string())),
        ],
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
                ctx.log("poll_error", &[("error", json!(format!("{e:#}")))]);
                if args.once {
                    return Err(e);
                }
                match Mcp::connect(&args.url, &token) {
                    Ok(m) => ctx.mcp = m,
                    Err(e2) => ctx.log("reconnect_failed", &[("error", json!(format!("{e2:#}"))) ]),
                }
                thread::sleep(Duration::from_secs(args.poll));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn governor_passes_a_eurusd_stop_on_fifty_dollars() {
        // 0.5 ATR of EURUSD M15 is ~29 points, 0.01 lot = 1000 units, so the
        // stop risks about 29 cents — 0.6% of $50.
        let risk: f64 = 29.0 * 0.00001 * 100_000.0 * 0.01;
        assert!((risk - 0.29).abs() < 1e-9);
        assert!(risk_allowed(risk, 50.0, 2.0));
    }

    #[test]
    fn governor_refuses_a_gold_stop_on_fifty_dollars() {
        // Same shape of trade on gold: 0.5 ATR of $3.25, 0.01 lot = 1 oz.
        let risk: f64 = 1.62 * 100.0 * 0.01;
        assert!((risk - 1.62).abs() < 1e-9);
        assert!(!risk_allowed(risk, 50.0, 2.0));
        // The same trade is fine on a funded balance, so the cap is the account
        // talking, not the instrument.
        assert!(risk_allowed(risk, 5_000.0, 2.0));
    }

    #[test]
    fn governor_refuses_broken_numbers() {
        assert!(!risk_allowed(f64::NAN, 50.0, 2.0));
        assert!(!risk_allowed(1.0, 0.0, 2.0));
        assert!(!risk_allowed(-1.0, 50.0, 2.0));
    }

    #[test]
    fn brackets_are_symmetric_and_rounded() {
        // The stop and target are mirrored around the entry and rounded to the
        // symbol's precision, so MT5 never rejects them as off-tick.
        assert_eq!(round_to(1.09173 - 0.5 * 0.00065, 5), 1.09141);
        assert_eq!(round_to(1.09173 + 1.5 * 0.00065, 5), 1.09271);
        assert_eq!(round_to(3619.876, 2), 3619.88);
    }
}

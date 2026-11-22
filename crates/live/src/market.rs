//! Shared market access and strategy configuration.
//!
//! One implementation used by the live runner, the `--scan` view and the
//! dashboard, so the numbers on screen are computed by the same code that
//! places orders.

use anyhow::{bail, Context, Result};
use chrono::NaiveDateTime;
use serde::Serialize;
use serde_json::{json, Value};

use xau_core::{Bar, Side};
use xau_sim::orb::eu_dst_active;
use xau_sim::strategy::{atr, donchian, ema, entry_signals, StratKind, StratParams};

use crate::mcp::Mcp;

// --- D3, the validated configuration (configs/phase0.toml [strategy]) -------
pub const LOOKBACK: usize = 55;
pub const TREND_MA: usize = 300;
pub const ATR_PERIOD: usize = 14;
pub const SL_ATR: f64 = 1.5;
pub const TRAIL_ATR: f64 = 2.5;
pub const MAX_HOLD_BARS: usize = 240;
pub const SYMBOL: &str = "XAUUSD";
pub const CONTRACT: f64 = 100.0;

/// Timeframes the dashboard shows. Only the traded one: the page is a
/// control panel for one rule, not a market scanner.
pub const SCAN_PERIODS: [&str; 1] = ["H1"];

pub fn period_secs(p: &str) -> Result<i64> {
    Ok(match p {
        "M1" => 60,
        "M5" => 300,
        "M15" => 900,
        "M30" => 1800,
        "H1" => 3600,
        "H4" => 14400,
        "D1" => 86400,
        other => bail!("unsupported period {other}"),
    })
}

// --- intraday overlay -------------------------------------------------------
/// Entries are only taken when the *next* bar opens inside this UTC hour
/// range. The validated intraday variant uses 07–18 UTC: London open through
/// the New York afternoon.
pub const ENTRY_FROM_UTC: u32 = 7;
pub const ENTRY_TO_UTC: u32 = 18;

/// The broker's clock is UTC+2, and one hour more while EU summer time is in
/// force (the broker and the EU switch on the same dates).
pub fn server_offset_s(ts_server: i64) -> i64 {
    7_200 + if eu_dst_active(ts_server.div_euclid(86_400)) { 3_600 } else { 0 }
}

/// Broker wall clock → true UTC.
///
/// The terminal has no UTC: every bar it returns is stamped with the broker's
/// own clock. The backtest data was converted before import, so anything the
/// live path compares against UTC — the entry window, above all — has to
/// convert too. A fixed −2h shift would be right in winter and an hour wrong
/// for half the year.
pub fn server_to_utc(ts_server: i64) -> i64 {
    ts_server - server_offset_s(ts_server)
}

/// UTC hour (0–23) of a broker-stamped timestamp.
pub fn utc_hour_of(ts_server: i64) -> u32 {
    (server_to_utc(ts_server).rem_euclid(86_400) / 3_600) as u32
}

/// True when this bar is the last one before the daily maintenance break.
///
/// The break starts at 23:00 server, so the bar whose *close* lands on 23:00
/// is the last tradeable one. Comparing in server time keeps DST out of it;
/// the same bar is 20:00 UTC in winter and 19:00 in summer.
pub fn is_day_last_bar(ts_server: i64, period_secs: i64) -> bool {
    (ts_server + period_secs).rem_euclid(86_400) / 60 == 23 * 60
}

pub fn in_entry_window(hour_utc: u32) -> bool {
    hour_utc >= ENTRY_FROM_UTC && hour_utc <= ENTRY_TO_UTC
}

pub fn strategy_params() -> StratParams {
    StratParams { lookback: LOOKBACK, trend_ma: TREND_MA, ..Default::default() }
}

// --- terminal reads ---------------------------------------------------------
pub fn as_f64(v: &Value) -> f64 {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(f64::NAN)
}

pub fn parse_naive(s: &str) -> Result<NaiveDateTime> {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
        .with_context(|| format!("bad timestamp {s:?}"))
}

pub fn server_now(m: &mut Mcp) -> Result<NaiveDateTime> {
    let t = m.call("get_time_information", json!({}))?;
    let s = t
        .get("trade_server_last_known_time")
        .and_then(Value::as_str)
        .context("no trade_server_last_known_time")?;
    parse_naive(s)
}

pub fn bars(m: &mut Mcp, period: &str, count: usize) -> Result<Vec<Bar>> {
    let now = server_now(m)?;
    bars_at(m, period, count, now)
}

/// Same as `bars`, but reuses a clock the caller already read. Saves one
/// round-trip per timeframe when several are being scanned.
pub fn bars_at(m: &mut Mcp, period: &str, count: usize, now: NaiveDateTime) -> Result<Vec<Bar>> {
    let per = period_secs(period)?;
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

/// Bars excluding the still-forming one, plus the broker's current clock.
/// Signals are only ever taken on these.
pub fn closed_bars(m: &mut Mcp, period: &str, count: usize) -> Result<(Vec<Bar>, i64)> {
    let now = server_now(m)?;
    Ok((closed_bars_at(m, period, count, now)?, now.and_utc().timestamp()))
}

pub fn closed_bars_at(
    m: &mut Mcp,
    period: &str,
    count: usize,
    now: NaiveDateTime,
) -> Result<Vec<Bar>> {
    let per = period_secs(period)?;
    let all = bars_at(m, period, count, now)?;
    let now = now.and_utc().timestamp();
    if all.is_empty() {
        bail!("no bars returned for {period}");
    }
    Ok(if all.last().unwrap().ts + per <= now {
        all
    } else {
        all[..all.len() - 1].to_vec()
    })
}

pub fn last_price(m: &mut Mcp) -> Result<f64> {
    let b = bars(m, "M1", 2)?;
    b.last().map(|b| b.close).context("no M1 bars")
}

pub fn account(m: &mut Mcp) -> Result<Value> {
    m.call("get_trading_account_info", json!({}))
}

pub fn positions(m: &mut Mcp) -> Result<Vec<Value>> {
    let v = m.call("get_trading_open_positions", json!({}))?;
    Ok(v.get("positions").and_then(Value::as_array).cloned().unwrap_or_default())
}

/// Closed positions, newest last. The terminal only returns what its own
/// history window still holds, so this is a recent-trades view, not a ledger.
pub fn trade_history(m: &mut Mcp, limit: u32) -> Result<Vec<Value>> {
    let v = m.call("get_trading_history_positions", json!({ "limit": limit }))?;
    Ok(v.get("positions").and_then(Value::as_array).cloned().unwrap_or_default())
}

pub fn field(p: &Value, k: &str) -> String {
    match &p[k] {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

// --- per-timeframe analysis --------------------------------------------------
/// Everything the UI and the scan table need about one timeframe, computed from
/// closed bars only.
#[derive(Debug, Clone, Serialize)]
pub struct Analysis {
    pub period: String,
    pub bar_ts: i64,
    /// The same bar on the true UTC clock, for display. `bar_ts` stays on the
    /// broker clock so countdowns keep working.
    pub bar_ts_utc: i64,
    pub bars: usize,
    pub close: f64,
    pub hi: f64,
    pub lo: f64,
    pub ema: f64,
    pub atr: f64,
    /// "buy" / "sell" / null
    pub side: Option<String>,
    /// Stop the strategy would place for a signal taken at the close.
    pub stop: Option<f64>,
    /// Distance price must still travel to trigger each side (0 when triggered).
    pub to_buy: f64,
    pub to_sell: f64,
    pub uptrend: bool,
    /// Risk in dollars per 1.00 lot for that stop distance.
    pub risk_per_lot: f64,
    /// Seconds until this timeframe's next bar closes.
    pub next_close_in: i64,
    /// UTC hour the next bar opens at — the hour an entry would actually fill.
    pub fill_hour_utc: u32,
    /// Whether that hour is inside the entry window. A signal outside it is
    /// real but not tradeable under the intraday rule.
    pub window_open: bool,
}

pub fn analyse(m: &mut Mcp, period: &str) -> Result<Analysis> {
    let per = period_secs(period)?;
    let now = server_now(m)?;
    let closed = closed_bars_at(m, period, 420, now)?;
    if closed.len() < TREND_MA + 2 {
        bail!("need {} bars, got {}", TREND_MA + 2, closed.len());
    }
    let now = now.and_utc().timestamp();
    let i = closed.len() - 1;
    let bar = closed[i];

    let params = strategy_params();
    let side = entry_signals(&closed, StratKind::Donchian, &params)
        .last()
        .copied()
        .flatten();
    let (hi, lo) = donchian(&closed, LOOKBACK);
    let closes: Vec<f64> = closed.iter().map(|b| b.close).collect();
    let trend = ema(&closes, TREND_MA);
    let atrs = atr(&closed, ATR_PERIOD);
    let a = atrs[i];

    let stop = side.map(|s| {
        let dir = if s == Side::Buy { 1.0 } else { -1.0 };
        round2(bar.close - dir * SL_ATR * a)
    });

    Ok(Analysis {
        period: period.to_string(),
        bar_ts: bar.ts,
        bar_ts_utc: server_to_utc(bar.ts),
        bars: closed.len(),
        close: bar.close,
        hi: hi[i],
        lo: lo[i],
        ema: trend[i],
        atr: a,
        side: side.map(|s| s.as_str().to_string()),
        stop,
        to_buy: (hi[i] - bar.close).max(0.0),
        to_sell: (bar.close - lo[i]).max(0.0),
        uptrend: bar.close > trend[i],
        risk_per_lot: SL_ATR * a * CONTRACT,
        next_close_in: (bar.ts + 2 * per - now).max(0),
        fill_hour_utc: utc_hour_of(bar.ts + per),
        window_open: in_entry_window(utc_hour_of(bar.ts + per)),
    })
}

pub fn analyse_all(m: &mut Mcp) -> Vec<Result<Analysis>> {
    SCAN_PERIODS.iter().map(|p| analyse(m, p)).collect()
}

pub fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

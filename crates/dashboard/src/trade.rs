//! Order handling for the dashboard — the only code path that can move money.
//!
//! Everything enforced here is enforced **server-side**, because the browser is
//! not a trust boundary and the broker's own pre-check is not either: measured
//! on the demo account, `trade_check_market_order` returns `retcode 0 / Done`
//! for a *buy with the stop loss above the market*, which is a guaranteed
//! immediate loss. So the rules below are ours:
//!
//!   * demo accounts only
//!   * a stop loss is mandatory, finite, and on the losing side of price
//!   * lot size is capped and must sit on a 0.01 step
//!   * stop distance is bounded, so a decimal-point slip cannot open a
//!     position with either a 1-cent or a $500 stop
//!   * the order is tagged `manual-ui`, keeping it out of strategy P&L
//!
//! Every attempt, successful or not, is written to SQLite by the caller.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use xau_live::market::{self, SYMBOL};
use xau_live::mcp::Mcp;

/// Hard cap for a hand-placed order. The strategy's own H4 size is 0.10.
pub const MAX_LOTS: f64 = 0.10;
pub const MIN_LOTS: f64 = 0.01;
/// Reject stops tighter than this (a fat finger, or a stop inside the spread).
pub const MIN_STOP_USD: f64 = 0.50;
/// ...and wider than this (almost always a typo, e.g. 3500 instead of 3600).
pub const MAX_STOP_USD: f64 = 200.0;
/// Tag written on every order from this UI.
pub const COMMENT: &str = "manual-ui";

/// The comment for an order, recording which signal it came from when the
/// trader copied one. Still a *manual* order — a human pressed the button —
/// but the provenance is visible in the terminal and in the audit log.
pub fn comment_for(source: &Option<String>) -> String {
    match source.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) if market::SCAN_PERIODS.contains(&p) => format!("{COMMENT}:{p}"),
        _ => COMMENT.to_string(),
    }
}

/// True for anything this UI placed, with or without a signal source.
pub fn is_ui_comment(c: &str) -> bool {
    c.trim().starts_with(COMMENT)
}

/// MT5 trade return codes that mean the order is live. Success is **not** 0:
/// a filled market order comes back as 10009 (Done), 10008 (Placed) or 10010
/// (Done, partial). Treating anything non-zero as failure reports a live
/// position as rejected, which is the most dangerous wrong answer here.
const DONE: [i64; 3] = [10008, 10009, 10010];

fn is_done(code: Option<i64>) -> bool {
    match code {
        Some(c) => DONE.contains(&c),
        // No code at all: we cannot call it a rejection, so the caller must
        // fall back to checking whether the position actually exists.
        None => false,
    }
}

fn detail(r: &Value) -> String {
    r["retcode_details"]
        .as_str()
        .or_else(|| r["retcode_string"].as_str())
        .or_else(|| r["message"].as_str())
        .unwrap_or("no detail from terminal")
        .to_string()
}

fn position_ids(m: &mut Mcp) -> Result<Vec<String>> {
    Ok(market::positions(m)?
        .iter()
        .map(|p| market::field(p, "position_id"))
        .collect())
}

#[derive(Deserialize, Clone, Debug)]
pub struct OrderReq {
    pub side: String,
    pub lots: f64,
    pub sl: f64,
    pub tp: Option<f64>,
    /// false/absent = preview only, true = actually send.
    #[serde(default)]
    pub confirm: bool,
    /// Timeframe whose signal the trader copied, if any ("H1", "H4", ...).
    /// Recorded in the comment; never changes the fact that this is manual.
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Preview {
    pub side: String,
    pub lots: f64,
    pub bid: f64,
    pub ask: f64,
    pub entry: f64,
    pub sl: f64,
    pub tp: Option<f64>,
    pub stop_points: f64,
    pub stop_usd: f64,
    pub risk_usd: f64,
    pub margin: f64,
    pub margin_free: f64,
    pub spread_pts: f64,
    pub comment: String,
}

/// Bid, ask and spread from the newest M1 bar. Bars are built on bid, so the
/// ask is the bid plus the quoted spread.
pub fn quote(m: &mut Mcp) -> Result<(f64, f64, f64)> {
    let bars = market::bars(m, "M1", 2)?;
    let last = bars.last().context("no M1 bars")?;
    let bid = last.close;
    let spread = last.spread_pts as f64;
    if !bid.is_finite() || bid <= 0.0 {
        bail!("no usable price from the terminal");
    }
    Ok((bid, bid + spread * 0.01, spread))
}

/// Refuse to trade anything that is not an order-enabled demo account.
pub fn ensure_tradeable(m: &mut Mcp) -> Result<()> {
    let acct = market::account(m)?;
    let kind = market::field(&acct["account"], "type");
    if kind != "demo" {
        bail!("refusing to place dashboard orders on a {kind} account");
    }
    let t = &acct["terminal"];
    if t["mcp_trade_allowed"].as_bool() == Some(false) {
        bail!("the terminal has MCP trading disabled");
    }
    if t["experts_trade_allowed"].as_bool() == Some(false) {
        bail!("the terminal has Algo Trading disabled");
    }
    Ok(())
}

pub fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// The stop distance in dollars that `req` implies, given a price to enter at.
pub fn stop_distance(req: &OrderReq, entry: f64) -> f64 {
    if req.side == "buy" {
        entry - req.sl
    } else {
        req.sl - entry
    }
}

pub fn validate(req: &OrderReq, bid: f64, ask: f64) -> Result<()> {
    if req.side != "buy" && req.side != "sell" {
        bail!("side must be \"buy\" or \"sell\", got {:?}", req.side);
    }
    if !req.lots.is_finite() || !(MIN_LOTS..=MAX_LOTS).contains(&req.lots) {
        bail!(
            "lots must be between {MIN_LOTS} and {MAX_LOTS}, got {}",
            req.lots
        );
    }
    // 0.01 granularity, allowing for float representation
    let steps = req.lots * 100.0;
    if (steps - steps.round()).abs() > 1e-6 {
        bail!("lots must be a multiple of 0.01, got {}", req.lots);
    }
    if !req.sl.is_finite() || req.sl <= 0.0 {
        bail!("a stop loss is required");
    }
    if let Some(src) = req
        .source
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if !market::SCAN_PERIODS.contains(&src) {
            bail!("unknown signal source {src:?}");
        }
    }

    let entry = if req.side == "buy" { ask } else { bid };
    let dist = stop_distance(req, entry);
    if dist <= 0.0 {
        bail!(
            "stop {} is on the wrong side: a {} fills near {:.2} and its stop must be {} it",
            round2(req.sl),
            req.side,
            round2(entry),
            if req.side == "buy" { "below" } else { "above" }
        );
    }
    if dist < MIN_STOP_USD {
        bail!(
            "stop is only ${:.2} away; the minimum is ${:.2}",
            dist,
            MIN_STOP_USD
        );
    }
    if dist > MAX_STOP_USD {
        bail!(
            "stop is ${:.2} away; the maximum is ${:.2} (likely a typo)",
            dist,
            MAX_STOP_USD
        );
    }

    if let Some(tp) = req.tp {
        if !tp.is_finite() || tp <= 0.0 {
            bail!("take profit must be a positive price");
        }
        let win = if req.side == "buy" {
            tp - entry
        } else {
            entry - tp
        };
        if win <= 0.0 {
            bail!(
                "take profit must be {} the entry price",
                if req.side == "buy" { "above" } else { "below" }
            );
        }
    }
    Ok(())
}

/// Build the preview the user must approve. Calls the broker's read-only check
/// so the margin figures come from the terminal, not from our arithmetic.
pub fn preview(m: &mut Mcp, req: &OrderReq) -> Result<Preview> {
    ensure_tradeable(m)?;
    let (bid, ask, spread) = quote(m)?;
    validate(req, bid, ask)?;

    let entry = if req.side == "buy" { ask } else { bid };
    let dist = stop_distance(req, entry);
    let sl = round2(req.sl);
    let risk = dist * market::CONTRACT * req.lots;

    let mut args = json!({
        "symbol": SYMBOL, "type": req.side, "volume": req.lots, "sl": sl
    });
    if let Some(tp) = req.tp {
        args["tp"] = json!(round2(tp));
    }
    let check = m.call("trade_check_market_order", args)?;
    let result = &check["result"];
    if check["ok"].as_bool() == Some(false) {
        bail!(
            "broker check failed: {}",
            check["error"].as_str().unwrap_or("unknown")
        );
    }
    if let Some(rc) = result["retcode"].as_i64() {
        if rc != 0 {
            bail!(
                "broker rejected the order: {}",
                result["retcode_string"].as_str().unwrap_or("unknown")
            );
        }
    }

    Ok(Preview {
        side: req.side.clone(),
        lots: req.lots,
        bid: round2(bid),
        ask: round2(ask),
        entry: round2(entry),
        sl,
        tp: req.tp.map(round2),
        stop_points: (dist / 0.01).round(),
        stop_usd: round2(dist),
        risk_usd: round2(risk),
        margin: market::as_f64(&result["margin"]),
        margin_free: market::as_f64(&result["margin_free"]),
        spread_pts: spread,
        comment: comment_for(&req.source),
    })
}

/// Send the order. Re-validates against a fresh quote, so a confirmation that
/// sat on screen while price moved cannot place a stop on the wrong side.
pub fn place(m: &mut Mcp, req: &OrderReq) -> Result<Value> {
    ensure_tradeable(m)?;
    let (bid, ask, _) = quote(m)?;
    validate(req, bid, ask)?;

    let sl = round2(req.sl);
    let mut order = json!({
        "symbol": SYMBOL,
        "type": req.side,
        "volume": req.lots,
        "sl": sl,
        "comment": comment_for(&req.source),
    });
    if let Some(tp) = req.tp {
        order["tp"] = json!(round2(tp));
    }

    // Snapshot first, so the new position can be identified afterwards even if
    // another manual-ui position is already open.
    let before = position_ids(m)?;
    let r = m.call("trade_send_market_order", order)?;
    let rc = r["retcode"].as_i64();
    let why = detail(&r);

    let after = market::positions(m)?;
    let opened = after.iter().find(|p| {
        is_ui_comment(&market::field(p, "comment"))
            && !before.contains(&market::field(p, "position_id"))
    });

    match opened {
        Some(p) => Ok(json!({
            "retcode": rc,
            "detail": why,
            "position_id": market::field(p, "position_id"),
            "price": market::as_f64(&p["price_open"]),
            "lots": market::as_f64(&p["volume"]),
            "sl": market::as_f64(&p["stop_loss"]),
        })),
        None if !is_done(rc) => bail!("order rejected: {why}"),
        // The terminal says done but no new position is visible: surface it
        // instead of claiming success we cannot see.
        None => {
            bail!("terminal reported \"{why}\" but no new position is open — check the terminal")
        }
    }
}

pub fn close(m: &mut Mcp, ticket: &str) -> Result<Value> {
    if ticket.trim().is_empty() {
        bail!("position ticket is required");
    }
    let r = m.call(
        "trade_close_single_position",
        json!({ "symbol": SYMBOL, "position_ticket": ticket.trim() }),
    )?;
    if let Some(rc) = r["retcode"].as_i64() {
        if !is_done(Some(rc)) {
            bail!("close rejected: {}", detail(&r));
        }
    }
    Ok(r)
}

/// Move a stop loss. Only accepts a stop on the losing side of the *current*
/// price, so "modify" can never be used to push a stop through the market.
pub fn modify_stop(m: &mut Mcp, ticket: &str, sl: f64) -> Result<Value> {
    let (bid, ask, _) = quote(m)?;
    let positions = market::positions(m)?;
    let p = positions
        .iter()
        .find(|p| market::field(p, "position_id") == ticket.trim())
        .with_context(|| format!("position {ticket} is not open"))?;
    let side = market::field(p, "action");
    if side != "buy" && side != "sell" {
        bail!("position {ticket} has an unexpected side {side:?}");
    }

    if !sl.is_finite() || sl <= 0.0 {
        bail!("stop loss must be a positive price");
    }
    if side == "buy" && sl >= bid {
        bail!("a buy stop must be below the market ({:.2})", round2(bid));
    }
    if side == "sell" && sl <= ask {
        bail!("a sell stop must be above the market ({:.2})", round2(ask));
    }

    let r = m.call(
        "trade_modify_sl_tp",
        json!({ "symbol": SYMBOL, "position_ticket": ticket.trim(), "sl": round2(sl) }),
    )?;
    if let Some(rc) = r["retcode"].as_i64() {
        if !is_done(Some(rc)) {
            bail!("modify rejected: {}", detail(&r));
        }
    }
    Ok(r)
}

/// Stop at the position's own entry, plus the spread on a sell so that
/// break-even really is break-even after paying to get out.
pub fn breakeven_price(entry: f64, side: &str, spread_pts: f64) -> f64 {
    let pad = spread_pts * 0.01;
    round2(if side == "buy" {
        entry + pad
    } else {
        entry - pad
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(side: &str, lots: f64, sl: f64) -> OrderReq {
        OrderReq {
            side: side.into(),
            lots,
            sl,
            tp: None,
            confirm: false,
            source: None,
        }
    }

    // bid 3500.00, ask 3500.15
    const BID: f64 = 3500.00;
    const ASK: f64 = 3500.15;

    #[test]
    fn accepts_a_sane_sell() {
        // sell fills at bid, stop above it
        assert!(validate(&req("sell", 0.01, 3520.0), BID, ASK).is_ok());
    }

    #[test]
    fn accepts_a_sane_buy() {
        // buy fills at ask, stop below it
        assert!(validate(&req("buy", 0.05, 3480.0), BID, ASK).is_ok());
    }

    #[test]
    fn rejects_stop_on_the_wrong_side() {
        // The broker's own check says "Done" for this; we must not.
        let e = validate(&req("buy", 0.01, 9999.0), BID, ASK).unwrap_err();
        assert!(e.to_string().contains("wrong side"), "{e}");
        assert!(validate(&req("sell", 0.01, 3000.0), BID, ASK).is_err());
    }

    #[test]
    fn rejects_missing_stop() {
        assert!(validate(&req("buy", 0.01, 0.0), BID, ASK).is_err());
        assert!(validate(&req("buy", 0.01, f64::NAN), BID, ASK).is_err());
    }

    #[test]
    fn rejects_lot_sizes_outside_the_cap() {
        assert!(validate(&req("buy", 0.0, 3480.0), BID, ASK).is_err());
        assert!(validate(&req("buy", 0.11, 3480.0), BID, ASK).is_err());
        assert!(validate(&req("buy", 1.0, 3480.0), BID, ASK).is_err());
        assert!(validate(&req("buy", 0.015, 3480.0), BID, ASK).is_err());
    }

    #[test]
    fn rejects_stops_that_are_too_tight_or_absurd() {
        assert!(
            validate(&req("buy", 0.01, 3500.10), BID, ASK).is_err(),
            "3 cents"
        );
        assert!(
            validate(&req("buy", 0.01, 3200.0), BID, ASK).is_err(),
            "$300"
        );
    }

    #[test]
    fn rejects_unknown_side() {
        assert!(validate(&req("BUY", 0.01, 3480.0), BID, ASK).is_err());
        assert!(validate(&req("short", 0.01, 3480.0), BID, ASK).is_err());
    }

    #[test]
    fn rejects_take_profit_on_the_wrong_side() {
        let mut r = req("buy", 0.01, 3480.0);
        r.tp = Some(3400.0);
        assert!(validate(&r, BID, ASK).is_err());
        r.tp = Some(3600.0);
        assert!(validate(&r, BID, ASK).is_ok());
    }

    #[test]
    fn stop_distance_uses_the_side_you_enter_on() {
        assert!((stop_distance(&req("buy", 0.01, 3490.0), ASK) - 10.15).abs() < 1e-9);
        assert!((stop_distance(&req("sell", 0.01, 3510.0), BID) - 10.0).abs() < 1e-9);
    }

    #[test]
    fn signal_source_is_recorded_but_stays_manual() {
        assert_eq!(comment_for(&None), "manual-ui");
        assert_eq!(comment_for(&Some("H1".into())), "manual-ui:H1");
        assert_eq!(
            comment_for(&Some(" H1 ".into())),
            "manual-ui:H1",
            "trimmed"
        );
        assert_eq!(comment_for(&Some("".into())), "manual-ui");
        assert_eq!(
            comment_for(&Some("M1".into())),
            "manual-ui",
            "not a scanned timeframe"
        );
        // Retired timeframes fall back to a plain manual tag rather than
        // producing a comment that claims a signal we no longer scan.
        assert_eq!(comment_for(&Some("H4".into())), "manual-ui");
        assert_eq!(comment_for(&Some("M5".into())), "manual-ui");
        assert!(is_ui_comment("manual-ui"));
        assert!(is_ui_comment("manual-ui:H1"));
        assert!(!is_ui_comment("D3-H1"));
    }

    #[test]
    fn rejects_an_unknown_signal_source() {
        let mut r = req("buy", 0.01, 3480.0);
        r.source = Some("H2".into());
        assert!(validate(&r, BID, ASK).is_err());
        // H4 and M5 used to be scanned; they are retired now, so a request
        // carrying one is a stale client, not a valid signal.
        r.source = Some("H4".into());
        assert!(validate(&r, BID, ASK).is_err());
        r.source = Some("M5".into());
        assert!(validate(&r, BID, ASK).is_err());
        r.source = Some("H1".into());
        assert!(validate(&r, BID, ASK).is_ok());
    }

    #[test]
    fn success_codes_are_mt5_done_not_zero() {
        assert!(is_done(Some(10009)), "Done");
        assert!(is_done(Some(10008)), "Placed");
        assert!(is_done(Some(10010)), "Done partial");
        assert!(!is_done(Some(0)), "0 is not the success code here");
        assert!(!is_done(Some(10004)), "requote");
        assert!(!is_done(Some(10019)), "no money");
        assert!(!is_done(None), "unknown must not be called success");
    }

    #[test]
    fn breakeven_covers_the_spread() {
        assert!((breakeven_price(3500.0, "buy", 15.0) - 3500.15).abs() < 1e-9);
        assert!((breakeven_price(3500.0, "sell", 15.0) - 3499.85).abs() < 1e-9);
    }
}

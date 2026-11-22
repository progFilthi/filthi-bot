//! London / New York opening-range breakout ("ORB").
//!
//! The mechanical version of the transcript's strategy:
//!
//!   1. Mark the high and low of a fixed session window (default: London
//!      09:00–14:00 local time — his "London open to New York open" box).
//!   2. After the window closes, take the **first** break of that box, up or
//!      down. One trade per day, whichever side breaks first.
//!   3. Stop beyond the most recent swing (lowest low / highest high of the
//!      N bars before the break), or beyond the box edge.
//!   4. Target a fixed multiple of the risk (default 2R).
//!   5. Hard flat time (default 19:00 local), so nothing is held overnight.
//!
//! Cost model is the pessimistic one used everywhere else in this crate:
//! the OHLC series is the bid. A buy pays the spread (we lift the ask); a
//! sell receives the bid. Stops additionally pay slippage. Targets do not
//! get favourable slippage. Within a bar that touches both stop and target,
//! the stop is assumed to fill first.
//!
//! No look-ahead: the box only uses bars that closed inside the window, and
//! the stop only uses bars before the entry bar.

use xau_core::{Bar, Side};
use serde::{Deserialize, Serialize};

use crate::engine::{CONTRACT, POINT};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionTz {
    /// Fixed UTC clock — no daylight saving.
    None,
    /// London/Europe: +1h from the last Sunday of March to the last Sunday of October.
    Eu,
    /// New York: +1h from the second Sunday of March to the first Sunday of November.
    Us,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    /// Enter the instant price trades through the box edge (the transcript's rule).
    Break,
    /// Require a bar to *close* outside the box, then enter at the next open.
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopMode {
    /// Recent swing: min low / max high of the N bars before the break.
    Swing,
    /// The opposite side of the box.
    Box,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExitKind {
    Stop,
    Target,
    Time,
    EndOfData,
    /// Still running: only `day_view` produces this.
    Open,
}

impl ExitKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Stop => "SL",
            Self::Target => "TP",
            Self::Time => "TIME",
            Self::EndOfData => "EOD",
            Self::Open => "OPEN",
        }
    }
}

#[derive(Debug, Clone)]
pub struct OrbConfig {
    /// Box start, in minutes after local midnight.
    pub box_start_min: u32,
    /// Box end / first moment a break is allowed, in local minutes.
    pub box_end_min: u32,
    /// Hard flat time, in local minutes.
    pub cutoff_min: u32,
    /// Which daylight-saving rule turns local minutes into UTC.
    pub tz: SessionTz,
    /// Bars used for the swing stop (0 = box edge only).
    pub swing_bars: usize,
    pub stop_mode: StopMode,
    pub confirm: Confirm,
    /// Target as a multiple of risk.
    pub rr: f64,
    /// Spread charged on a buy entry, in points.
    pub spread_pts: f64,
    /// Adverse slippage on entry, in points.
    pub entry_slip_pts: f64,
    /// Adverse slippage on a stop exit, in points.
    pub stop_slip_pts: f64,
    /// Minimum bars inside the box for the day to be tradeable.
    pub min_box_bars: usize,
    /// Box-height filters in points (0 = off).
    pub min_box_pts: f64,
    pub max_box_pts: f64,
    /// Skip days whose stop is wider than this, in points (0 = off).
    pub max_risk_pts: f64,
    /// Only trade breaks that agree with this EMA of the close (0 = off).
    /// Expressed in bars of the series being tested.
    pub ema_period: usize,
    /// Skip the day if no break appears within this many minutes of the box
    /// end (0 = wait until the cut-off).
    pub max_wait_min: u32,
    pub initial_deposit: f64,
    /// Percent of equity risked per trade.
    pub risk_pct: f64,
    /// Hard lot ceiling (prop firms often cap this).
    pub max_lots: f64,
}

impl Default for OrbConfig {
    fn default() -> Self {
        Self {
            box_start_min: 9 * 60,
            box_end_min: 14 * 60,
            cutoff_min: 19 * 60,
            tz: SessionTz::Eu,
            swing_bars: 30,
            stop_mode: StopMode::Swing,
            confirm: Confirm::Break,
            rr: 2.0,
            spread_pts: 15.0,
            entry_slip_pts: 2.0,
            stop_slip_pts: 5.0,
            min_box_bars: 60,
            min_box_pts: 0.0,
            max_box_pts: 0.0,
            max_risk_pts: 0.0,
            ema_period: 0,
            max_wait_min: 0,
            initial_deposit: 5_000.0,
            risk_pct: 0.7,
            max_lots: 5.0,
        }
    }
}

/// One completed ORB trade, cost-adjusted, expressed as a multiple of risk.
#[derive(Debug, Clone)]
pub struct OrbTrade {
    /// UTC day the trade belongs to (epoch days).
    pub day: i64,
    pub side: Side,
    pub entry_ts: i64,
    pub exit_ts: i64,
    pub entry: f64,
    pub stop: f64,
    pub target: f64,
    pub exit: f64,
    pub reason: ExitKind,
    pub box_high: f64,
    pub box_low: f64,
    /// Stop distance from the actual fill, in points.
    pub risk_pts: f64,
    /// Net PnL in points (already includes spread and slippage).
    pub pnl_pts: f64,
    /// Net PnL divided by risk. -1 is a clean stop-out.
    pub r: f64,
    pub bars_held: usize,
}

#[derive(Debug, Clone, Default)]
pub struct OrbRun {
    pub trades: Vec<OrbTrade>,
    pub days_scanned: usize,
    /// Days with a box but no break before the cut-off.
    pub no_break: usize,
    /// Days skipped because the box had too few bars.
    pub thin_box: usize,
    /// Days skipped by the box-height filter.
    pub box_filtered: usize,
    /// Days skipped because the stop was too wide.
    pub risk_filtered: usize,
    /// Days where both edges were crossed inside one bar.
    pub ambiguous_breaks: usize,
    /// Days skipped by the trend gate.
    pub trend_filtered: usize,
    /// Days where a trade was still open when the data ended.
    pub unfinished: usize,
}

// ---------------------------------------------------------------------------
// Calendar helpers
// ---------------------------------------------------------------------------

/// Calendar date and UTC clock time for an epoch-second timestamp.
pub fn ymd_hms(ts: i64) -> (i32, u32, u32, u32, u32) {
    let day = ts.div_euclid(86_400);
    let secs = ts.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(day);
    ((y), m, d, (secs / 3_600) as u32, ((secs % 3_600) / 60) as u32)
}

fn civil_from_days(z: i64) -> (i32, u32, u32) {
    // Howard Hinnant's algorithm, days since 1970-01-01.
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    ((y + if m <= 2 { 1 } else { 0 }) as i32, m, d)
}

/// Public wrapper so the study binary can build date filters.
pub fn days_from_civil_pub(y: i32, m: u32, d: u32) -> i64 {
    days_from_civil(y, m, d)
}

fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if m > 2 { m - 3 } else { m + 9 } as u64;
    let doy = (153 * mp + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

/// 0 = Sunday.
fn weekday(z: i64) -> u32 {
    ((z + 4).rem_euclid(7)) as u32
}

fn last_sunday(y: i32, month: u32, days_in_month: u32) -> u32 {
    let mut d = days_in_month;
    while d > 1 {
        if weekday(days_from_civil(y, month, d)) == 0 {
            return d;
        }
        d -= 1;
    }
    1
}

fn second_sunday(y: i32, month: u32) -> u32 {
    let mut seen = 0;
    for d in 1..=14 {
        if weekday(days_from_civil(y, month, d)) == 0 {
            seen += 1;
            if seen == 2 {
                return d;
            }
        }
    }
    8
}

fn first_sunday(y: i32, month: u32) -> u32 {
    for d in 1..=7 {
        if weekday(days_from_civil(y, month, d)) == 0 {
            return d;
        }
    }
    1
}

fn dst_active(day: i64, tz: SessionTz) -> bool {
    let (y, _, _) = civil_from_days(day);
    match tz {
        SessionTz::None => false,
        SessionTz::Eu => {
            let start = days_from_civil(y, 3, last_sunday(y, 3, 31)) * 86_400 + 3_600;
            let end = days_from_civil(y, 10, last_sunday(y, 10, 31)) * 86_400 + 3_600;
            let t = day * 86_400 + 12 * 3_600;
            t >= start && t < end
        }
        SessionTz::Us => {
            let start = days_from_civil(y, 3, second_sunday(y, 3)) * 86_400 + 7 * 3_600;
            let end = days_from_civil(y, 11, first_sunday(y, 11)) * 86_400 + 6 * 3_600;
            let t = day * 86_400 + 12 * 3_600;
            t >= start && t < end
        }
    }
}

/// Convert a local minute-of-day on `day` into a UTC minute-of-day.
fn local_to_utc_min(day: i64, local_min: u32, tz: SessionTz) -> i64 {
    let shift = if dst_active(day, tz) { 60i64 } else { 0 };
    (local_min as i64 + 1_440 - shift) % 1_440
}

/// Is European summer time in force on this UTC day?
///
/// Exposed because the broker's server clock (EET/EEST) switches on the same
/// dates, so a server timestamp becomes UTC by subtracting 2h, plus 1h while
/// this is true.
pub fn eu_dst_active(day: i64) -> bool {
    dst_active(day, SessionTz::Eu)
}

// ---------------------------------------------------------------------------
// The strategy
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct Chosen {
    /// Bar index the fill happens on.
    idx: usize,
    side: Side,
    /// Price level the trade is entered at (box edge, or the fill bar's open).
    trigger: f64,
    gap_fill: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Skip {
    None,
    ThinBox,
    BoxFilter,
    NoBreak,
    Trend,
}

struct DayScan {
    box_high: f64,
    box_low: f64,
    box_bars: usize,
    chosen: Option<Chosen>,
    ambiguous: bool,
    skip: Skip,
    cutoff: i64,
}

/// The whole per-day decision, shared by the backtest and the live logger so
/// the two cannot drift apart.
fn scan_day(
    bars: &[Bar],
    cfg: &OrbConfig,
    day: i64,
    day_start: usize,
    day_end: usize,
    ema_series: Option<&Vec<f64>>,
) -> DayScan {
    let midnight = day * 86_400;
    let box_start = midnight + local_to_utc_min(day, cfg.box_start_min, cfg.tz) * 60;
    let box_end = midnight + local_to_utc_min(day, cfg.box_end_min, cfg.tz) * 60;
    let cutoff = midnight + local_to_utc_min(day, cfg.cutoff_min, cfg.tz) * 60;

    let mut box_high = f64::MIN;
    let mut box_low = f64::MAX;
    let mut box_n = 0usize;
    for b in &bars[day_start..day_end] {
        if b.ts >= box_start && b.ts < box_end {
            box_high = box_high.max(b.high);
            box_low = box_low.min(b.low);
            box_n += 1;
        }
    }

    let mut scan = DayScan {
        box_high,
        box_low,
        box_bars: box_n,
        chosen: None,
        ambiguous: false,
        skip: Skip::None,
        cutoff,
    };

    if box_n < cfg.min_box_bars {
        scan.skip = Skip::ThinBox;
        return scan;
    }
    let box_pts = (box_high - box_low) / POINT;
    if (cfg.min_box_pts > 0.0 && box_pts < cfg.min_box_pts)
        || (cfg.max_box_pts > 0.0 && box_pts > cfg.max_box_pts)
    {
        scan.skip = Skip::BoxFilter;
        return scan;
    }

    for k in day_start..day_end {
        let bar = &bars[k];
        if bar.ts < box_end || bar.ts >= cutoff {
            continue;
        }
        if cfg.max_wait_min > 0 && bar.ts >= box_end + cfg.max_wait_min as i64 * 60 {
            break;
        }
        let hi_break = bar.high > box_high;
        let lo_break = bar.low < box_low;
        if !hi_break && !lo_break {
            continue;
        }

        let side = if hi_break && lo_break {
            scan.ambiguous = true;
            // Both edges traded inside one bar: assume the nearer edge to
            // the open was reached first rather than flattering the result.
            if (bar.open - box_low).abs() <= (box_high - bar.open).abs() {
                Side::Sell
            } else {
                Side::Buy
            }
        } else if hi_break {
            Side::Buy
        } else {
            Side::Sell
        };

        match cfg.confirm {
            Confirm::Break => {
                let level = match side {
                    Side::Buy => box_high,
                    Side::Sell => box_low,
                };
                scan.chosen = Some(Chosen {
                    idx: k,
                    side,
                    trigger: level,
                    gap_fill: match side {
                        Side::Buy => bar.open > level,
                        Side::Sell => bar.open < level,
                    },
                });
            }
            Confirm::Close => {
                let outside = match side {
                    Side::Buy => bar.close > box_high,
                    Side::Sell => bar.close < box_low,
                };
                if !outside {
                    continue;
                }
                let nk = k + 1;
                if nk < day_end && bars[nk].ts < cutoff {
                    scan.chosen = Some(Chosen {
                        idx: nk,
                        side,
                        trigger: bars[nk].open,
                        gap_fill: true,
                    });
                }
            }
        }
        break;
    }

    let Some(ch) = scan.chosen else {
        scan.skip = Skip::NoBreak;
        return scan;
    };

    if let Some(e) = ema_series {
        let ref_idx = ch.idx.saturating_sub(1);
        let trend = e.get(ref_idx).copied().unwrap_or(f64::NAN);
        if trend.is_finite() {
            let agrees = match ch.side {
                Side::Buy => ch.trigger > trend,
                Side::Sell => ch.trigger < trend,
            };
            if !agrees {
                scan.skip = Skip::Trend;
                scan.chosen = None;
            }
        }
    }
    scan
}

fn ema_series_for(bars: &[Bar], cfg: &OrbConfig) -> Option<Vec<f64>> {
    if cfg.ema_period > 1 {
        Some(ema(&bars.iter().map(|b| b.close).collect::<Vec<_>>(), cfg.ema_period))
    } else {
        None
    }
}

fn day_ranges(bars: &[Bar]) -> Vec<(i64, usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bars.len() {
        let day = bars[i].ts.div_euclid(86_400);
        let start = i;
        while i < bars.len() && bars[i].ts.div_euclid(86_400) == day {
            i += 1;
        }
        out.push((day, start, i));
    }
    out
}

/// Run the ORB strategy and return cost-adjusted trades.
pub fn run(bars: &[Bar], cfg: &OrbConfig) -> OrbRun {
    let mut out = OrbRun::default();
    if bars.is_empty() {
        return out;
    }
    let ema_series = ema_series_for(bars, cfg);

    for (day, day_start, day_end) in day_ranges(bars) {
        out.days_scanned += 1;
        let scan = scan_day(bars, cfg, day, day_start, day_end, ema_series.as_ref());
        match scan.skip {
            Skip::ThinBox => {
                out.thin_box += 1;
                continue;
            }
            Skip::BoxFilter => {
                out.box_filtered += 1;
                continue;
            }
            Skip::NoBreak => {
                if scan.ambiguous {
                    out.ambiguous_breaks += 1;
                }
                out.no_break += 1;
                continue;
            }
            Skip::Trend => {
                out.trend_filtered += 1;
                continue;
            }
            Skip::None => {}
        }
        if scan.ambiguous {
            out.ambiguous_breaks += 1;
        }

        let Some(ch) = scan.chosen else {
            out.no_break += 1;
            continue;
        };
        let Some(t) = build_trade(
            bars,
            cfg,
            day,
            ch.idx,
            ch.side,
            ch.trigger,
            ch.gap_fill,
            scan.box_high,
            scan.box_low,
        ) else {
            out.risk_filtered += 1;
            continue;
        };
        out.trades
            .push(manage(bars, cfg, ch.idx, day_end, scan.cutoff, t, true));
    }
    out
}

/// Live state for one session, used by the paper-trading logger.
#[derive(Debug, Clone)]
pub struct DayView {
    pub day: i64,
    pub box_high: f64,
    pub box_low: f64,
    pub box_bars: usize,
    pub skip: &'static str,
    pub side: Option<Side>,
    pub entry: f64,
    pub stop: f64,
    pub target: f64,
    pub entry_ts: i64,
    pub last_ts: i64,
    pub last_close: f64,
    /// Unrealised R while the trade is running.
    pub open_r: f64,
    /// Realised R once the trade is finished.
    pub result_r: Option<f64>,
    pub exit: Option<ExitKind>,
    pub reason: &'static str,
    /// Machine-readable state: `waiting`, `no_entry`, `skipped`, `in_trade`,
    /// `closed`.
    pub phase: &'static str,
}

/// Evaluate `day` without forcing it closed: an unresolved trade is reported
/// as still open at the last available close. `now_ts` decides whether the
/// break window has already passed.
pub fn day_view(bars: &[Bar], cfg: &OrbConfig, day: i64, now_ts: i64) -> Option<DayView> {
    let (_, day_start, day_end) = day_ranges(bars)
        .into_iter()
        .find(|(d, _, _)| *d == day)?;
    let ema_series = ema_series_for(bars, cfg);
    let scan = scan_day(bars, cfg, day, day_start, day_end, ema_series.as_ref());
    let last = day_end - 1;

    let mut view = DayView {
        day,
        box_high: scan.box_high,
        box_low: scan.box_low,
        box_bars: scan.box_bars,
        skip: match scan.skip {
            Skip::None => "",
            Skip::ThinBox => "thin box — not enough bars in the session",
            Skip::BoxFilter => "box height filter",
            Skip::NoBreak => "no break inside the entry window",
            Skip::Trend => "break disagreed with the trend filter",
        },
        side: None,
        entry: f64::NAN,
        stop: f64::NAN,
        target: f64::NAN,
        entry_ts: 0,
        last_ts: bars[last].ts,
        last_close: bars[last].close,
        open_r: 0.0,
        result_r: None,
        exit: None,
        reason: "",
        phase: "waiting",
    };

    let Some(ch) = scan.chosen else {
        let window_open = now_ts < scan.cutoff
            && now_ts < day * 86_400 + local_to_utc_min(day, cfg.box_end_min, cfg.tz) * 60
                + cfg.max_wait_min.max(1) as i64 * 60;
        view.reason = if scan.skip == Skip::None && window_open {
            "waiting for the first break"
        } else if scan.skip == Skip::None {
            "entry window closed without a break"
        } else {
            "skipped"
        };
        view.phase = if scan.skip != Skip::None {
            "skipped"
        } else if window_open {
            "waiting"
        } else {
            "no_entry"
        };
        return Some(view);
    };

    let Some(mut t) = build_trade(
        bars,
        cfg,
        day,
        ch.idx,
        ch.side,
        ch.trigger,
        ch.gap_fill,
        scan.box_high,
        scan.box_low,
    ) else {
        view.skip = "stop too wide";
        view.reason = "skipped";
        return Some(view);
    };

    view.side = Some(t.side);
    view.entry = t.entry;
    view.stop = t.stop;
    view.target = t.target;
    view.entry_ts = t.entry_ts;

    t = manage(bars, cfg, ch.idx, day_end, scan.cutoff, t, false);
    view.last_ts = t.exit_ts;
    view.last_close = t.exit;
    view.exit = Some(t.reason);
    if t.reason == ExitKind::Open {
        view.open_r = t.r;
        view.reason = "in trade";
        view.phase = "in_trade";
    } else {
        view.result_r = Some(t.r);
        view.reason = match t.reason {
            ExitKind::Stop => "stopped out",
            ExitKind::Target => "target hit",
            ExitKind::Time => "closed at the flat time",
            _ => "finished",
        };
        view.phase = "closed";
    }
    Some(view)
}

/// The session's clocks for one UTC day, as epoch seconds: box start, box
/// end, last moment a break may be taken, and the hard flat time.
pub fn session_times(day: i64, cfg: &OrbConfig) -> (i64, i64, i64, i64) {
    let midnight = day * 86_400;
    let box_start = midnight + local_to_utc_min(day, cfg.box_start_min, cfg.tz) * 60;
    let box_end = midnight + local_to_utc_min(day, cfg.box_end_min, cfg.tz) * 60;
    let cutoff = midnight + local_to_utc_min(day, cfg.cutoff_min, cfg.tz) * 60;
    let entry_end = if cfg.max_wait_min > 0 {
        (box_end + cfg.max_wait_min as i64 * 60).min(cutoff)
    } else {
        cutoff
    };
    (box_start, box_end, entry_end, cutoff)
}

fn build_trade(
    bars: &[Bar],
    cfg: &OrbConfig,
    day: i64,
    idx: usize,
    side: Side,
    trigger: f64,
    gap_fill: bool,
    box_high: f64,
    box_low: f64,
) -> Option<OrbTrade> {
    let bar = &bars[idx];
    let base = if gap_fill { bar.open } else { trigger };

    // Costs at entry: a buy lifts the ask and slips; a sell hits the bid.
    let entry = match side {
        Side::Buy => base + (cfg.spread_pts + cfg.entry_slip_pts) * POINT,
        Side::Sell => base - cfg.entry_slip_pts * POINT,
    };

    let stop = match cfg.stop_mode {
        StopMode::Box => match side {
            Side::Buy => box_low - POINT,
            Side::Sell => box_high + POINT,
        },
        StopMode::Swing => {
            let from = idx.saturating_sub(cfg.swing_bars.max(1));
            let mut s = match side {
                Side::Buy => f64::MAX,
                Side::Sell => f64::MIN,
            };
            for b in &bars[from..idx] {
                match side {
                    Side::Buy => s = s.min(b.low),
                    Side::Sell => s = s.max(b.high),
                }
            }
            let s = match side {
                Side::Buy => s - POINT,
                Side::Sell => s + POINT,
            };
            // A gap can put the swing on the wrong side of the fill; fall back
            // to the box edge, which is where the transcript would put it.
            match side {
                Side::Buy => {
                    if s < entry {
                        s
                    } else {
                        box_low - POINT
                    }
                }
                Side::Sell => {
                    if s > entry {
                        s
                    } else {
                        box_high + POINT
                    }
                }
            }
        }
    };

    let risk_pts = (entry - stop).abs() / POINT;
    if !(risk_pts.is_finite() && risk_pts > 0.0) {
        return None;
    }
    if cfg.max_risk_pts > 0.0 && risk_pts > cfg.max_risk_pts {
        return None;
    }

    let target = match side {
        Side::Buy => entry + cfg.rr * (entry - stop),
        Side::Sell => entry - cfg.rr * (stop - entry),
    };

    Some(OrbTrade {
        day,
        side,
        entry_ts: bar.ts,
        exit_ts: bar.ts,
        entry,
        stop,
        target,
        exit: entry,
        reason: ExitKind::EndOfData,
        box_high,
        box_low,
        risk_pts,
        pnl_pts: 0.0,
        r: 0.0,
        bars_held: 0,
    })
}

fn manage(
    bars: &[Bar],
    cfg: &OrbConfig,
    entry_idx: usize,
    day_end: usize,
    cutoff: i64,
    mut t: OrbTrade,
    force: bool,
) -> OrbTrade {
    // A buy closes at the bid; a sell closes at the ask.
    let exit_cost = |side: Side, px: f64, slip_pts: f64| match side {
        Side::Buy => px - slip_pts * POINT,
        Side::Sell => px + (cfg.spread_pts + slip_pts) * POINT,
    };

    let mut result: Option<(usize, f64, ExitKind)> = None;
    for k in entry_idx..day_end {
        let bar = &bars[k];
        if bar.ts >= cutoff && k > entry_idx {
            result = Some((k, exit_cost(t.side, bar.open, 0.0), ExitKind::Time));
            break;
        }
        if k == entry_idx && cfg.confirm == Confirm::Break {
            // On the bar that triggered the break we do not know whether the
            // stop was touched before or after the break, so only a close
            // through the stop counts. This avoids phantom stop-outs from the
            // wick that preceded the break.
            let beyond = match t.side {
                Side::Buy => bar.close <= t.stop,
                Side::Sell => bar.close >= t.stop,
            };
            if beyond {
                let px = exit_cost(t.side, bar.close, cfg.stop_slip_pts);
                result = Some((k, px, ExitKind::Stop));
                break;
            }
            continue;
        }
        let stopped = match t.side {
            Side::Buy => bar.low <= t.stop,
            Side::Sell => bar.high >= t.stop,
        };
        if stopped {
            result = Some((k, exit_cost(t.side, t.stop, cfg.stop_slip_pts), ExitKind::Stop));
            break;
        }
        let targeted = match t.side {
            Side::Buy => bar.high >= t.target,
            Side::Sell => bar.low <= t.target,
        };
        if targeted {
            result = Some((k, exit_cost(t.side, t.target, 0.0), ExitKind::Target));
            break;
        }
    }

    let (k, px, reason) = result.unwrap_or_else(|| {
        let last = day_end - 1;
        (
            last,
            exit_cost(t.side, bars[last].close, 0.0),
            if force { ExitKind::EndOfData } else { ExitKind::Open },
        )
    });

    t.exit = px;
    t.exit_ts = bars[k].ts;
    t.reason = reason;
    t.bars_held = k.saturating_sub(entry_idx);
    let dir = if t.side == Side::Buy { 1.0 } else { -1.0 };
    t.pnl_pts = (px - t.entry) * dir / POINT;
    t.r = t.pnl_pts / t.risk_pts;
    t
}

fn ema(values: &[f64], period: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; values.len()];
    if period == 0 || values.len() < period {
        return out;
    }
    let k = 2.0 / (period as f64 + 1.0);
    let mut acc: f64 = values[..period].iter().sum::<f64>() / period as f64;
    out[period - 1] = acc;
    for i in period..values.len() {
        acc = values[i] * k + acc * (1.0 - k);
        out[i] = acc;
    }
    out
}

// ---------------------------------------------------------------------------
// Sizing and statistics
// ---------------------------------------------------------------------------

/// A trade sized against a running account balance.
#[derive(Debug, Clone)]
pub struct SizedTrade {
    pub trade: OrbTrade,
    pub lots: f64,
    pub pnl: f64,
    pub equity_after: f64,
}

#[derive(Debug, Clone, Default)]
pub struct EquityStats {
    pub trades: usize,
    pub wins: usize,
    pub losses: usize,
    pub win_rate: f64,
    pub gross_profit: f64,
    pub gross_loss: f64,
    pub net: f64,
    pub profit_factor: f64,
    pub expectancy_r: f64,
    pub avg_win_r: f64,
    pub avg_loss_r: f64,
    pub max_drawdown: f64,
    pub max_drawdown_pct: f64,
    pub max_losing_streak: usize,
    pub final_equity: f64,
    pub return_pct: f64,
    /// Worst single day, as a percent of the equity at its start.
    pub worst_day_pct: f64,
    pub avg_lots: f64,
    pub max_lots_used: f64,
    pub skip_risk: usize,
    /// Standard deviation of per-trade R.
    pub sd_r: f64,
    /// t-statistic of the mean R against zero.
    pub t_stat: f64,
}

/// Size the trades with fixed-fractional risk and produce equity stats.
///
/// `floor_lots` is the broker minimum. If the requested risk is smaller than
/// minimum lot risk, the trade is taken at the minimum lot (mirroring a real
/// account that cannot go smaller) and counted in `skip_risk` only when the
/// minimum lot would blow through the risk budget by more than 2x.
pub fn size(
    trades: &[OrbTrade],
    initial_deposit: f64,
    risk_pct: f64,
    max_lots: f64,
    floor_lots: f64,
) -> (Vec<SizedTrade>, EquityStats) {
    let mut equity = initial_deposit;
    let mut peak = initial_deposit;
    let mut out: Vec<SizedTrade> = Vec::with_capacity(trades.len());
    let mut st = EquityStats::default();

    // Group by day for the worst-day statistic.
    let mut day_open_equity = equity;
    let mut current_day = i64::MIN;

    for t in trades {
        if t.day != current_day {
            current_day = t.day;
            day_open_equity = equity;
        }
        let dollars_per_lot_per_point = POINT * CONTRACT;
        let budget = equity * risk_pct / 100.0;
        let raw_lots = budget / (t.risk_pts * dollars_per_lot_per_point);
        let mut lots = (raw_lots / 0.01).floor() * 0.01;
        if lots < floor_lots {
            lots = floor_lots;
            if raw_lots > 0.0 && floor_lots / raw_lots > 2.0 {
                st.skip_risk += 1;
            }
        }
        if lots > max_lots {
            lots = (max_lots / 0.01).floor() * 0.01;
        }
        let pnl = t.pnl_pts * dollars_per_lot_per_point * lots;
        equity += pnl;
        peak = peak.max(equity);
        let dd = peak - equity;
        if dd > st.max_drawdown {
            st.max_drawdown = dd;
            st.max_drawdown_pct = if peak > 0.0 { dd / peak * 100.0 } else { 0.0 };
        }
        let day_pct = if day_open_equity > 0.0 {
            (equity - day_open_equity) / day_open_equity * 100.0
        } else {
            0.0
        };
        st.worst_day_pct = st.worst_day_pct.min(day_pct);
        st.avg_lots += lots;
        st.max_lots_used = st.max_lots_used.max(lots);

        out.push(SizedTrade {
            trade: t.clone(),
            lots,
            pnl,
            equity_after: equity,
        });
    }

    let mut streak = 0usize;
    st.trades = out.len();
    for s in &out {
        if s.pnl > 0.0 {
            st.wins += 1;
            st.gross_profit += s.pnl;
            streak = 0;
        } else {
            st.losses += 1;
            st.gross_loss += -s.pnl;
            streak += 1;
            st.max_losing_streak = st.max_losing_streak.max(streak);
        }
        st.net += s.pnl;
    }
    st.final_equity = equity;
    st.return_pct = (equity / initial_deposit - 1.0) * 100.0;
    st.win_rate = if st.trades > 0 {
        st.wins as f64 / st.trades as f64 * 100.0
    } else {
        0.0
    };
    st.profit_factor = if st.gross_loss > 0.0 {
        st.gross_profit / st.gross_loss
    } else if st.gross_profit > 0.0 {
        f64::INFINITY
    } else {
        0.0
    };
    st.expectancy_r = if st.trades > 0 {
        out.iter().map(|s| s.trade.r).sum::<f64>() / st.trades as f64
    } else {
        0.0
    };
    let (mut wr, mut lr) = (Vec::new(), Vec::new());
    for s in &out {
        if s.trade.r > 0.0 {
            wr.push(s.trade.r);
        } else {
            lr.push(s.trade.r);
        }
    }
    st.avg_win_r = if wr.is_empty() { 0.0 } else { wr.iter().sum::<f64>() / wr.len() as f64 };
    st.avg_loss_r = if lr.is_empty() { 0.0 } else { lr.iter().sum::<f64>() / lr.len() as f64 };
    st.avg_lots /= st.trades.max(1) as f64;
    if st.trades > 1 {
        let rs: Vec<f64> = out.iter().map(|s| s.trade.r).collect();
        let mean = rs.iter().sum::<f64>() / rs.len() as f64;
        let var = rs.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (rs.len() as f64 - 1.0);
        st.sd_r = var.sqrt();
        if st.sd_r > 0.0 {
            st.t_stat = mean / (st.sd_r / (rs.len() as f64).sqrt());
        }
    }
    (out, st)
}

/// Clear a prop-style challenge, or record how it died.
#[derive(Debug, Clone)]
pub struct ChallengeResult {
    pub passed: bool,
    pub days: i64,
    pub trades: usize,
    pub final_equity: f64,
    pub reason: String,
    pub worst_day_pct: f64,
    pub max_dd_pct: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct ChallengeCfg {
    pub initial_deposit: f64,
    pub risk_pct: f64,
    pub target_pct: f64,
    pub max_dd_pct: f64,
    /// 0 = no daily-loss rule.
    pub daily_dd_pct: f64,
}

impl Default for ChallengeCfg {
    fn default() -> Self {
        Self {
            initial_deposit: 5_000.0,
            risk_pct: 0.7,
            target_pct: 8.0,
            max_dd_pct: 6.0,
            daily_dd_pct: 4.0,
        }
    }
}

/// Walk the trades as a challenge account: same lots the live account would
/// use, with the platform's drawdown rules checked after every trade.
pub fn challenge(trades: &[OrbTrade], cfg: &ChallengeCfg, max_lots: f64) -> ChallengeResult {
    let mut equity = cfg.initial_deposit;
    let start = cfg.initial_deposit;
    let target = start * (1.0 + cfg.target_pct / 100.0);
    let floor = start * (1.0 - cfg.max_dd_pct / 100.0);
    let mut peak = start;
    let mut current_day = i64::MIN;
    let mut day_open = equity;
    let mut worst_day: f64 = 0.0;
    let mut max_dd: f64 = 0.0;
    let mut used = 0usize;

    for t in trades {
        if t.day != current_day {
            current_day = t.day;
            day_open = equity;
        }
        used += 1;
        let budget = equity * cfg.risk_pct / 100.0;
        let mut lots = (budget / (t.risk_pts * POINT * CONTRACT) / 0.01).floor() * 0.01;
        if lots < 0.01 {
            lots = 0.01;
        }
        lots = lots.min(max_lots);
        equity += t.pnl_pts * POINT * CONTRACT * lots;

        let day_pct = (equity - day_open) / day_open * 100.0;
        worst_day = worst_day.min(day_pct);
        peak = peak.max(equity);
        let dd_pct = (peak - equity) / peak * 100.0;
        max_dd = max_dd.max(dd_pct);

        if equity <= floor {
            return ChallengeResult {
                passed: false,
                days: (t.day - trades[0].day).max(0),
                trades: used,
                final_equity: equity,
                reason: format!("max drawdown {:.2}% breached", dd_pct),
                worst_day_pct: worst_day,
                max_dd_pct: max_dd,
            };
        }
        if cfg.daily_dd_pct > 0.0 && day_pct <= -cfg.daily_dd_pct {
            return ChallengeResult {
                passed: false,
                days: (t.day - trades[0].day).max(0),
                trades: used,
                final_equity: equity,
                reason: format!("daily loss {:.2}% breached", day_pct),
                worst_day_pct: worst_day,
                max_dd_pct: max_dd,
            };
        }
        if equity >= target {
            return ChallengeResult {
                passed: true,
                days: (t.day - trades[0].day).max(0),
                trades: used,
                final_equity: equity,
                reason: "target reached".into(),
                worst_day_pct: worst_day,
                max_dd_pct: max_dd,
            };
        }
    }

    ChallengeResult {
        passed: false,
        days: trades
            .last()
            .map(|t| (t.day - trades[0].day).max(0))
            .unwrap_or(0),
        trades: used,
        final_equity: equity,
        reason: format!(
            "data ended at {:+.2}% (target {:+.2}%)",
            (equity / start - 1.0) * 100.0,
            cfg.target_pct
        ),
        worst_day_pct: worst_day,
        max_dd_pct: max_dd,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(ts: i64, o: f64, h: f64, l: f64, c: f64) -> Bar {
        Bar {
            ts,
            open: o,
            high: h,
            low: l,
            close: c,
            volume: 1,
            spread_pts: 15,
            spread_max: 15,
        }
    }

    /// 2026-03-02 (a Monday, outside DST).
    const DAY: i64 = 20_514;

    #[test]
    fn civil_round_trip() {
        for z in [0i64, 20_514, 19_000, 8_000] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
        assert_eq!(civil_from_days(20_514), (2026, 3, 2));
    }

    #[test]
    fn dst_windows() {
        // 2026: EU DST starts 29 March, ends 25 October.
        assert!(!dst_active(days_from_civil(2026, 3, 28), SessionTz::Eu));
        assert!(dst_active(days_from_civil(2026, 3, 30), SessionTz::Eu));
        assert!(dst_active(days_from_civil(2026, 10, 24), SessionTz::Eu));
        assert!(!dst_active(days_from_civil(2026, 10, 26), SessionTz::Eu));
        // Local 09:00 -> 08:00 UTC while Europe is on summer time.
        assert_eq!(
            local_to_utc_min(days_from_civil(2026, 7, 1), 540, SessionTz::Eu),
            480
        );
        assert_eq!(
            local_to_utc_min(days_from_civil(2026, 1, 5), 540, SessionTz::Eu),
            540
        );
    }

    #[test]
    fn buys_the_first_break_and_targets_two_r() {
        let mut bars = Vec::new();
        // Box 09:00-14:00 UTC on DAY: high 2005, low 1995.
        for m in 0..300 {
            let up = m % 60 < 30;
            let (h, l) = if up { (2005.0, 1995.0) } else { (2002.0, 1998.0) };
            bars.push(bar(DAY * 86_400 + (9 * 60 + m) * 60, 2000.0, h, l, 2000.0));
        }
        // 14:00 break above 2005.
        bars.push(bar(DAY * 86_400 + 14 * 3600, 2004.0, 2010.0, 2003.0, 2008.0));
        // Later bars: drift to target.
        for (i, px) in [2012.0, 2016.0, 2025.0].iter().enumerate() {
            let ts = DAY * 86_400 + (14 * 3600) + (i as i64 + 1) * 60;
            bars.push(bar(ts, px - 2.0, *px + 2.0, px - 3.0, *px));
        }

        let cfg = OrbConfig {
            min_box_bars: 60,
            swing_bars: 30,
            ..Default::default()
        };
        let run = run(&bars, &cfg);
        assert_eq!(run.trades.len(), 1);
        let t = &run.trades[0];
        assert_eq!(t.side, Side::Buy);
        assert!(t.entry > 2005.0, "entry {} should include the spread", t.entry);
        // Stop is below the recent swing low, and the target is 2R above entry.
        assert!(t.stop < t.entry);
        assert!((t.target - (t.entry + 2.0 * (t.entry - t.stop))).abs() < 1e-9);
        assert_eq!(t.reason, ExitKind::Target);
        assert!(t.r > 1.5, "2R target should net close to 2R, got {}", t.r);
    }

    #[test]
    fn takes_only_the_first_break_of_the_day() {
        let mut bars = Vec::new();
        for m in 0..300 {
            bars.push(bar(DAY * 86_400 + (9 * 60 + m) * 60, 2000.0, 2004.0, 1996.0, 2000.0));
        }
        // Break up, then a big sell-off that would breach the low too.
        bars.push(bar(DAY * 86_400 + 14 * 3600, 2003.0, 2006.0, 2002.0, 2005.0));
        bars.push(bar(DAY * 86_400 + 14 * 3600 + 60, 2005.0, 2005.0, 1990.0, 1992.0));
        let cfg = OrbConfig { min_box_bars: 60, ..Default::default() };
        let run = run(&bars, &cfg);
        assert_eq!(run.trades.len(), 1);
        assert_eq!(run.trades[0].side, Side::Buy);
    }

    #[test]
    fn no_break_means_no_trade() {
        let mut bars = Vec::new();
        for m in 0..300 {
            bars.push(bar(DAY * 86_400 + (9 * 60 + m) * 60, 2000.0, 2004.0, 1996.0, 2000.0));
        }
        for m in 0..240 {
            bars.push(bar(DAY * 86_400 + 14 * 3600 + m * 60, 2000.0, 2003.0, 1997.0, 2001.0));
        }
        let cfg = OrbConfig { min_box_bars: 60, ..Default::default() };
        let run = run(&bars, &cfg);
        assert!(run.trades.is_empty());
        assert_eq!(run.no_break, 1);
    }

    #[test]
    fn the_break_must_arrive_inside_the_wait_window() {
        let mut bars = Vec::new();
        for m in 0..300 {
            bars.push(bar(DAY * 86_400 + (9 * 60 + m) * 60, 2000.0, 2004.0, 1996.0, 2000.0));
        }
        // First break at 14:30 UTC — 90 minutes after the 13:00 box end.
        for m in 0..90 {
            bars.push(bar(DAY * 86_400 + 13 * 3600 + m * 60, 2000.0, 2003.0, 1997.0, 2000.0));
        }
        bars.push(bar(DAY * 86_400 + 14 * 3600 + 30 * 60, 2003.0, 2008.0, 2002.0, 2007.0));

        let mut cfg = OrbConfig {
            box_start_min: 9 * 60,
            box_end_min: 13 * 60,
            min_box_bars: 60,
            ..Default::default()
        };
        cfg.max_wait_min = 60;
        assert!(run(&bars, &cfg).trades.is_empty(), "14:30 break is outside a 60-min window");
        cfg.max_wait_min = 120;
        assert_eq!(run(&bars, &cfg).trades.len(), 1, "a 120-min window should catch it");
    }

    #[test]
    fn stops_out_inside_the_entry_bar_range() {
        let mut bars = Vec::new();
        for m in 0..300 {
            bars.push(bar(DAY * 86_400 + (9 * 60 + m) * 60, 2000.0, 2004.0, 1996.0, 2000.0));
        }
        // Break up on this bar, then collapse through the stop.
        bars.push(bar(DAY * 86_400 + 14 * 3600, 2003.0, 2006.0, 1980.0, 1985.0));
        let cfg = OrbConfig { min_box_bars: 60, ..Default::default() };
        let run = run(&bars, &cfg);
        assert_eq!(run.trades.len(), 1);
        assert_eq!(run.trades[0].reason, ExitKind::Stop);
        assert!(run.trades[0].r < -0.99, "stop-out must cost at least 1R after slippage");
    }

    #[test]
    fn day_view_agrees_with_the_backtest_on_finished_trades() {
        let mut bars = Vec::new();
        for m in 0..300 {
            let (h, l) = if m % 60 < 30 { (2005.0, 1995.0) } else { (2002.0, 1998.0) };
            bars.push(bar(DAY * 86_400 + (9 * 60 + m) * 60, 2000.0, h, l, 2000.0));
        }
        bars.push(bar(DAY * 86_400 + 14 * 3600, 2004.0, 2010.0, 2003.0, 2008.0));
        for (i, px) in [2012.0, 2016.0, 2025.0].iter().enumerate() {
            let ts = DAY * 86_400 + 14 * 3600 + (i as i64 + 1) * 60;
            bars.push(bar(ts, px - 2.0, *px + 2.0, px - 3.0, *px));
        }
        let cfg = OrbConfig { min_box_bars: 60, ..Default::default() };
        let run = run(&bars, &cfg);
        let t = &run.trades[0];
        let view = day_view(&bars, &cfg, DAY, DAY * 86_400 + 20 * 3600).unwrap();
        assert_eq!(view.side, Some(t.side));
        assert_eq!(view.exit, Some(t.reason));
        assert!((view.result_r.unwrap() - t.r).abs() < 1e-9);
        assert_eq!(view.reason, "target hit");
    }

    #[test]
    fn day_view_reports_a_running_trade_as_open() {
        let mut bars = Vec::new();
        for m in 0..300 {
            bars.push(bar(DAY * 86_400 + (9 * 60 + m) * 60, 2000.0, 2004.0, 1996.0, 2000.0));
        }
        // Break up, then drift sideways with no target and no stop.
        bars.push(bar(DAY * 86_400 + 14 * 3600, 2003.0, 2006.0, 2002.0, 2005.0));
        for m in 1..10 {
            bars.push(bar(DAY * 86_400 + 14 * 3600 + m * 60, 2005.0, 2006.0, 2003.0, 2004.0));
        }
        let cfg = OrbConfig { min_box_bars: 60, ..Default::default() };
        let view = day_view(&bars, &cfg, DAY, DAY * 86_400 + 14 * 3600 + 600).unwrap();
        assert_eq!(view.reason, "in trade");
        assert_eq!(view.exit, Some(ExitKind::Open));
        assert!(view.result_r.is_none());
        assert!(view.open_r < 0.0, "drifted against the entry, so R should be negative");
    }

    #[test]
    fn sizing_risks_the_configured_percent() {
        let t = OrbTrade {
            day: DAY,
            side: Side::Buy,
            entry_ts: DAY * 86_400,
            exit_ts: DAY * 86_400,
            entry: 2000.0,
            stop: 1990.0,
            target: 2020.0,
            exit: 1990.0,
            reason: ExitKind::Stop,
            box_high: 1999.0,
            box_low: 1991.0,
            risk_pts: 1000.0,
            pnl_pts: -1010.0,
            r: -1.01,
            bars_held: 5,
        };
        let (sized, st) = size(&[t], 5_000.0, 0.7, 10.0, 0.01);
        // 0.7% of 5,000 = $35; 1000 pts = $10 per 0.01 lot -> 0.03 lots.
        assert_eq!(sized[0].lots, 0.03);
        assert!((sized[0].pnl + 30.3).abs() < 0.001);
        assert_eq!(st.trades, 1);
        assert!((st.max_drawdown - 30.3).abs() < 0.001);
    }
}

//! Cleaning pass over loaded bars (README §3: OHLC sanity, zero/negative
//! spreads, gap classification weekend vs unexpected, outlier checks).
//!
//! Policy:
//! - Inconsistent OHLC bars (high < max(o,c) or low > min(o,c)) are DROPPED
//!   and counted — they cannot be simulated.
//! - Bars with non-positive prices are DROPPED and counted.
//! - Zero/negative spread bars are KEPT but counted (the simulator will
//!   clamp to a floor spread; dropping would create artificial gaps).
//! - Gaps between consecutive bars are classified: daily break (<= 2h+tf,
//!   rollover), weekend (Friday->Sunday/Monday, 40..72h), everything else
//!   unexpected (multi-day market holidays land here).
//! - Outliers (huge single-bar ranges) are FLAGGED, never dropped: real
//!   extreme bars (e.g. March 2020) must stay in the data.

use chrono::{Datelike, TimeZone, Utc};
use xau_core::Bar;

#[derive(Debug, Default, Clone)]
pub struct GapExample {
    pub ts_before: i64,
    pub ts_after: i64,
    pub gap_s: i64,
}

#[derive(Debug, Default, Clone)]
pub struct GapStats {
    pub weekend: usize,
    pub daily_break: usize,
    pub unexpected: usize,
    /// Hour (UTC) at which each daily break starts (bar before the gap).
    pub daily_break_start_hour_utc: [u32; 24],
    /// Same hour in the broker server clock (UTC + configured offset).
    pub daily_break_start_hour_server: [u32; 24],
    pub unexpected_examples: Vec<GapExample>,
    pub longest_gap_s: i64,
}

#[derive(Debug, Default, Clone)]
pub struct CleanReport {
    pub input_bars: usize,
    pub dropped_bad_ohlc: usize,
    pub dropped_bad_price: usize,
    pub zero_spread: usize,
    pub negative_spread: usize,
    pub outliers_flagged: usize,
    pub outlier_examples: Vec<Bar>,
    pub gaps: GapStats,
    /// Bars kept after cleaning.
    pub output_bars: usize,
}

/// Clean bars of a given timeframe (`tf_secs`, 60 for M1).
/// `point` is used to express outlier thresholds in price units;
/// `utc_offset_s` is the broker server offset (for the daily-break
/// hour histogram, the timezone-verification signal).
pub fn clean_bars(
    bars: &[Bar],
    tf_secs: i64,
    point: f64,
    utc_offset_s: i64,
) -> (Vec<Bar>, CleanReport) {
    let mut rep = CleanReport {
        input_bars: bars.len(),
        ..Default::default()
    };
    let mut kept: Vec<Bar> = Vec::with_capacity(bars.len());

    for b in bars {
        if !(b.open > 0.0 && b.high > 0.0 && b.low > 0.0 && b.close > 0.0) {
            rep.dropped_bad_price += 1;
            continue;
        }
        if !(b.high >= b.open.max(b.close) && b.low <= b.open.min(b.close)) {
            rep.dropped_bad_ohlc += 1;
            continue;
        }
        if b.spread_pts < 0 {
            rep.negative_spread += 1;
        } else if b.spread_pts == 0 {
            rep.zero_spread += 1;
        }
        kept.push(*b);
    }

    classify_gaps(&kept, tf_secs, utc_offset_s, &mut rep);

    // Outlier flag: single-bar range far beyond the typical bar.
    let mut ranges: Vec<f64> = kept.iter().map(|b| b.high - b.low).collect();
    ranges.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_range = if ranges.is_empty() {
        0.0
    } else {
        ranges[ranges.len() / 2]
    };
    let min_abs = 100.0 * point; // at least $1 on gold at point=0.01
    for b in &kept {
        let range = b.high - b.low;
        if range > 25.0 * median_range && range > min_abs {
            rep.outliers_flagged += 1;
            if rep.outlier_examples.len() < 5 {
                rep.outlier_examples.push(*b);
            }
        }
    }

    rep.output_bars = kept.len();
    (kept, rep)
}

fn hour_of(ts: i64) -> u32 {
    (ts.rem_euclid(86_400) / 3_600) as u32
}

fn weekday_monday0(ts: i64) -> u32 {
    // 1970-01-01 was a Thursday (Mon=0..Sun=6 via weekday().num_days_from_monday()).
    Utc.timestamp_opt(ts, 0)
        .single()
        .map(|d| d.weekday().num_days_from_monday())
        .unwrap_or(7)
}

fn classify_gaps(bars: &[Bar], tf_secs: i64, utc_offset_s: i64, rep: &mut CleanReport) {
    for w in bars.windows(2) {
        let (prev, next) = (&w[0], &w[1]);
        let diff = next.ts - prev.ts;
        if diff <= tf_secs {
            continue; // normal cadence
        }
        if diff > rep.gaps.longest_gap_s {
            rep.gaps.longest_gap_s = diff;
        }
        let wd = weekday_monday0(prev.ts); // 0=Mon..6=Sun
        let is_fri = wd == 4;
        if is_fri && diff > 40 * 3600 && diff <= 72 * 3600 {
            rep.gaps.weekend += 1;
        } else if diff <= 2 * 3600 + tf_secs {
            rep.gaps.daily_break += 1;
            let h_utc = hour_of(prev.ts) as usize;
            rep.gaps.daily_break_start_hour_utc[h_utc] += 1;
            let h_server = hour_of(prev.ts + utc_offset_s) as usize;
            rep.gaps.daily_break_start_hour_server[h_server] += 1;
        } else if rep.gaps.unexpected_examples.len() < 10 {
            rep.gaps.unexpected += 1;
            rep.gaps.unexpected_examples.push(GapExample {
                ts_before: prev.ts,
                ts_after: next.ts,
                gap_s: diff,
            });
        } else {
            rep.gaps.unexpected += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(ts: i64, o: f64, h: f64, l: f64, c: f64, spread: i64) -> Bar {
        Bar {
            ts,
            open: o,
            high: h,
            low: l,
            close: c,
            volume: 10,
            spread_pts: spread,
            spread_max: spread,
        }
    }

    #[test]
    fn drops_bad_ohlc_and_bad_price() {
        let bars = vec![
            bar(0, 100.0, 101.0, 99.0, 100.5, 20),
            bar(60, 100.0, 98.0, 99.0, 100.5, 20), // high < max(o,c)
            bar(120, 0.0, 101.0, 99.0, 100.5, 20), // zero price
            bar(180, 100.0, 101.0, 99.0, 100.5, 20),
        ];
        let (kept, rep) = clean_bars(&bars, 60, 0.01, 7200);
        assert_eq!(rep.dropped_bad_ohlc, 1);
        assert_eq!(rep.dropped_bad_price, 1);
        assert_eq!(rep.output_bars, 2);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[1].ts, 180);
    }

    #[test]
    fn counts_zero_and_negative_spread_but_keeps() {
        let bars = vec![
            bar(0, 100.0, 101.0, 99.0, 100.5, 0),
            bar(60, 100.0, 101.0, 99.0, 100.5, -3),
            bar(120, 100.0, 101.0, 99.0, 100.5, 20),
        ];
        let (_kept, rep) = clean_bars(&bars, 60, 0.01, 7200);
        assert_eq!(rep.zero_spread, 1);
        assert_eq!(rep.negative_spread, 1);
        assert_eq!(rep.output_bars, 3);
    }

    #[test]
    fn classifies_weekend_daily_and_unexpected() {
        // 2024-01-05 18:20 UTC is a Friday.
        let fri = 1704478800;
        let t0 = fri - 7260 - 60;
        let bars = vec![
            bar(t0, 100.0, 101.0, 99.0, 100.5, 20),
            bar(t0 + 60, 100.0, 101.0, 99.0, 100.5, 20), // normal
            // daily break: 2h gap on a Friday (rollover)
            bar(t0 + 60 + 7200, 100.0, 101.0, 99.0, 100.5, 20),
            // weekend: Friday -> Sunday (~49h)
            bar(t0 + 60 + 7200 + 49 * 3600, 100.0, 101.0, 99.0, 100.5, 20),
            bar(
                t0 + 60 + 7200 + 49 * 3600 + 60,
                100.0,
                101.0,
                99.0,
                100.5,
                20,
            ), // normal
            // unexpected: 6h gap mid-week
            bar(
                t0 + 60 + 7200 + 49 * 3600 + 60 + 6 * 3600,
                100.0,
                101.0,
                99.0,
                100.5,
                20,
            ),
        ];
        let (_kept, rep) = clean_bars(&bars, 60, 0.01, 7200);
        assert_eq!(rep.gaps.daily_break, 1);
        assert_eq!(rep.gaps.weekend, 1);
        assert_eq!(rep.gaps.unexpected, 1);
        assert_eq!(rep.gaps.longest_gap_s, 49 * 3600);
    }

    #[test]
    fn server_hour_histogram_uses_offset() {
        // UTC 19:00 (epoch 68400 = Thursday 1970-01-01) + offset 7200 -> server 21:00.
        let bars = vec![
            bar(68400, 100.0, 101.0, 99.0, 100.5, 20),
            bar(68400 + 7200 + 60, 100.0, 101.0, 99.0, 100.5, 20),
        ];
        let (_kept, rep) = clean_bars(&bars, 60, 0.01, 7200);
        assert_eq!(rep.gaps.daily_break, 1);
        assert_eq!(rep.gaps.daily_break_start_hour_utc[19], 1);
        assert_eq!(rep.gaps.daily_break_start_hour_server[21], 1);
    }

    #[test]
    fn outliers_flagged_not_dropped() {
        let mut bars: Vec<Bar> = (0..100)
            .map(|i| bar(i * 60, 100.0, 100.5, 99.5, 100.2, 20))
            .collect();
        // one crash bar: range $10 (median is $1.0 -> 25x is $25, so use $30)
        bars.push(bar(100 * 60, 100.0, 115.0, 85.0, 110.0, 20));
        let (kept, rep) = clean_bars(&bars, 60, 0.01, 7200);
        assert_eq!(rep.outliers_flagged, 1);
        assert_eq!(rep.output_bars, bars.len());
        assert_eq!(kept.len(), bars.len());
    }
}

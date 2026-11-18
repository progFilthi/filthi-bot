//! Resampling M1 bars to M5/M15/H1 (README §3: "without look-ahead").
//!
//! Bucket = floor(ts / tf) * tf (UTC). A bucket aggregates only bars whose
//! open time lies inside it, so nothing from the future can leak into a
//! bucket's values. `spread_pts` = spread of the first bar in the bucket
//! (the spread at bucket "open"), `spread_max` = worst spread in the bucket.

use xau_core::Bar;

/// Resample `bars` (sorted ascending, one timeframe) to `tf_secs` buckets.
pub fn resample(bars: &[Bar], tf_secs: i64) -> Vec<Bar> {
    assert!(tf_secs > 0, "tf_secs must be positive");
    let mut out: Vec<Bar> = Vec::new();
    let mut cur_bucket: i64 = i64::MIN;
    let mut cur: Option<Bar> = None;

    for b in bars {
        let bucket = b.ts.div_euclid(tf_secs) * tf_secs;
        if bucket < cur_bucket {
            // Defensive: unsorted input. Skip rather than look ahead.
            continue;
        }
        match cur {
            Some(ref mut acc) if bucket == cur_bucket => {
                acc.high = acc.high.max(b.high);
                acc.low = acc.low.min(b.low);
                acc.close = b.close;
                acc.volume += b.volume;
                acc.spread_max = acc.spread_max.max(b.spread_max);
            }
            _ => {
                if let Some(done) = cur.take() {
                    out.push(done);
                }
                cur_bucket = bucket;
                cur = Some(Bar {
                    ts: bucket,
                    open: b.open,
                    high: b.high,
                    low: b.low,
                    close: b.close,
                    volume: b.volume,
                    spread_pts: b.spread_pts,
                    spread_max: b.spread_max,
                });
            }
        }
    }
    if let Some(done) = cur.take() {
        out.push(done);
    }
    out
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
            volume: 1,
            spread_pts: spread,
            spread_max: spread,
        }
    }

    #[test]
    fn aggregates_five_minutes_correctly() {
        // 10:00..10:04 in one M5 bucket, 10:05..10:06 in the next.
        let base = 1_704_000_000; // arbitrary, aligned
        let bars = vec![
            bar(base, 100.0, 100.2, 99.9, 100.1, 20),
            bar(base + 60, 100.1, 100.5, 100.0, 100.4, 30),
            bar(base + 120, 100.4, 100.6, 100.2, 100.5, 25),
            bar(base + 180, 100.5, 100.7, 100.3, 100.6, 22),
            bar(base + 240, 100.6, 100.8, 100.4, 100.7, 21),
            bar(base + 300, 100.7, 101.0, 100.6, 100.9, 40),
            bar(base + 360, 100.9, 101.1, 100.8, 101.0, 35),
        ];
        let m5 = resample(&bars, 300);
        assert_eq!(m5.len(), 2);
        assert_eq!(m5[0].ts, base);
        assert_eq!(m5[0].open, 100.0);
        assert_eq!(m5[0].high, 100.8);
        assert_eq!(m5[0].low, 99.9);
        assert_eq!(m5[0].close, 100.7);
        assert_eq!(m5[0].volume, 5);
        assert_eq!(m5[0].spread_pts, 20); // open spread
        assert_eq!(m5[0].spread_max, 30); // worst in bucket
        assert_eq!(m5[1].ts, base + 300);
        assert_eq!(m5[1].open, 100.7);
        assert_eq!(m5[1].close, 101.0);
        assert_eq!(m5[1].spread_pts, 40);
        assert_eq!(m5[1].spread_max, 40);
    }

    #[test]
    fn bucket_boundary_has_no_lookahead() {
        // The bar at 10:04:59 must NOT influence the 10:05 bucket even though
        // its close is "closer" to 10:05.
        let base = 1_704_000_000;
        let bars = vec![
            bar(base + 299, 100.0, 105.0, 99.0, 104.0, 20),
            bar(base + 300, 100.0, 100.1, 99.9, 100.0, 20),
        ];
        let m5 = resample(&bars, 300);
        assert_eq!(m5.len(), 2);
        assert_eq!(m5[0].ts, base);
        assert_eq!(m5[0].high, 105.0);
        assert_eq!(m5[1].ts, base + 300);
        assert_eq!(m5[1].high, 100.1); // no leakage of the 105 high
    }

    #[test]
    fn partial_final_bucket_kept() {
        // Last bucket only has 2 of 5 bars (e.g. session end) — still emitted.
        let base = 1_704_000_000;
        let bars = vec![
            bar(base, 100.0, 100.2, 99.9, 100.1, 20),
            bar(base + 60, 100.1, 100.5, 100.0, 100.4, 25),
        ];
        let m5 = resample(&bars, 300);
        assert_eq!(m5.len(), 1);
        assert_eq!(m5[0].close, 100.4);
        assert_eq!(m5[0].spread_max, 25);
    }

    #[test]
    fn unsorted_input_is_skipped_not_merged() {
        let base = 1_704_000_000;
        let bars = vec![
            bar(base + 300, 100.0, 100.1, 99.9, 100.0, 20),
            bar(base, 100.0, 100.2, 99.9, 100.1, 25), // older than previous
        ];
        let m5 = resample(&bars, 300);
        assert_eq!(m5.len(), 1);
        assert_eq!(m5[0].ts, base + 300);
    }
}

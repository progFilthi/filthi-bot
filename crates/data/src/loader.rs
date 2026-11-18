//! CSV loader for the `ExportHistory.mq5` bar export.
//!
//! Columns: `time,open,high,low,close,tick_volume,spread,real_volume,`
//! `time_epoch` (`time` = server-local "yyyy.mm.dd hh:mi", `time_epoch` =
//! Unix seconds in the same server-local clock).
//!
//! Conversion to UTC: `ts_utc = epoch_server - server_utc_offset_s`.
//! Rows are sorted by ts and exact-timestamp duplicates are dropped
//! (first kept). Native bars carry `spread_max = spread_pts`.

use crate::config::SymbolSpec;
use anyhow::Context;
use chrono::NaiveDateTime;
use std::io::Read;
use std::path::Path;
use xau_core::Bar;

#[derive(Debug, Default, Clone)]
pub struct LoadReport {
    pub path: String,
    /// Data rows seen (excluding header).
    pub rows_total: usize,
    /// Rows dropped for unparseable numbers/timestamps.
    pub rows_bad_parse: usize,
    /// Rows dropped as exact-timestamp duplicates (first kept).
    pub duplicates: usize,
    /// Adjacent out-of-order pairs seen before sorting.
    pub out_of_order: usize,
    /// `time` column vs `time_epoch` mismatches (>60s) when both present.
    pub time_epoch_mismatch: usize,
    /// Bars after sort + dedup.
    pub bars: usize,
    pub first_ts: Option<i64>,
    pub last_ts: Option<i64>,
}

/// Load the raw CSV file into UTC bars.
pub fn load_bars_csv(path: &Path, spec: &SymbolSpec) -> anyhow::Result<(Vec<Bar>, LoadReport)> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("cannot open raw CSV {}", path.display()))?;
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_reader(file);
    parse_reader(&mut rdr, spec, path.display().to_string())
}

/// Reader-level core (shared with tests; usable for stdin/bytes).
pub fn load_bars_from_reader<R: Read>(
    rdr: &mut csv::Reader<R>,
    spec: &SymbolSpec,
) -> anyhow::Result<(Vec<Bar>, LoadReport)> {
    parse_reader(rdr, spec, "<reader>".to_string())
}

fn parse_reader<R: Read>(
    rdr: &mut csv::Reader<R>,
    spec: &SymbolSpec,
    path: String,
) -> anyhow::Result<(Vec<Bar>, LoadReport)> {
    let mut rep = LoadReport {
        path,
        ..Default::default()
    };
    let headers = rdr.headers().cloned().context("CSV has no header row")?;
    let has_epoch = headers.iter().any(|h| h.eq_ignore_ascii_case("time_epoch"));
    let mut bars: Vec<Bar> = Vec::new();

    for row in rdr.records() {
        rep.rows_total += 1;
        let rec = match row {
            Ok(r) => r,
            Err(_) => {
                rep.rows_bad_parse += 1;
                continue;
            }
        };
        let col = |name: &str| -> Option<&str> {
            headers
                .iter()
                .position(|h| h.eq_ignore_ascii_case(name))
                .and_then(|i| rec.get(i))
        };
        let parse_f64 = |v: Option<&str>| -> Option<f64> { v.and_then(|s| s.trim().parse().ok()) };

        let (Some(o), Some(h), Some(l), Some(c), Some(tv), Some(sp)) = (
            parse_f64(col("open")),
            parse_f64(col("high")),
            parse_f64(col("low")),
            parse_f64(col("close")),
            col("tick_volume").and_then(|s| s.trim().parse::<u64>().ok()),
            col("spread").and_then(|s| s.trim().parse::<i64>().ok()),
        ) else {
            rep.rows_bad_parse += 1;
            continue;
        };
        // Reject NaN/inf outright (they poison every downstream aggregate);
        // non-positive prices are handled by the cleaner.
        if !(o.is_finite() && h.is_finite() && l.is_finite() && c.is_finite()) {
            rep.rows_bad_parse += 1;
            continue;
        }

        // Server-local epoch seconds: prefer time_epoch, fall back to `time`.
        let time_str = col("time").unwrap_or("").trim().to_string();
        let mut epoch_server: Option<i64> = if has_epoch {
            col("time_epoch").and_then(|s| s.trim().parse::<i64>().ok())
        } else {
            None
        };
        if let (Some(e), Ok(t)) = (
            epoch_server,
            NaiveDateTime::parse_from_str(&time_str, "%Y.%m.%d %H:%M"),
        ) {
            // `and_utc()` treats the clock reading as-if-UTC, which is exactly
            // the MT5 server-local epoch convention.
            let from_str = t.and_utc().timestamp();
            if (from_str - e).abs() > 60 {
                rep.time_epoch_mismatch += 1;
            }
        }
        if epoch_server.is_none() {
            epoch_server = NaiveDateTime::parse_from_str(&time_str, "%Y.%m.%d %H:%M")
                .ok()
                .map(|t| t.and_utc().timestamp());
        }
        let Some(epoch_server) = epoch_server else {
            rep.rows_bad_parse += 1;
            continue;
        };

        bars.push(Bar {
            ts: epoch_server - spec.server_utc_offset_s,
            open: o,
            high: h,
            low: l,
            close: c,
            volume: tv,
            spread_pts: sp,
            spread_max: sp,
        });
    }

    // Sort + dedup exact timestamps (first kept).
    rep.out_of_order = bars.windows(2).filter(|w| w[1].ts < w[0].ts).count();
    bars.sort_by_key(|b| b.ts);
    let mut dups = 0usize;
    bars.dedup_by(|a, b| {
        if a.ts == b.ts {
            dups += 1;
            true
        } else {
            false
        }
    });
    rep.duplicates = dups;
    rep.bars = bars.len();
    rep.first_ts = bars.first().map(|b| b.ts);
    rep.last_ts = bars.last().map(|b| b.ts);

    Ok((bars, rep))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> SymbolSpec {
        SymbolSpec {
            name: "XAUUSD".into(),
            digits: 2,
            point: 0.01,
            contract_size: 100.0,
            tick_size: 0.01,
            tick_value: 0.1,
            volume_min: 0.01,
            volume_max: 100.0,
            volume_step: 0.01,
            stops_level_pts: 0,
            freeze_level_pts: 0,
            swap_mode: 1,
            swap_long_pts: -12.6,
            swap_short_pts: -4.6,
            swap_rollover3day_dow: 3,
            margin_initial: 0.0,
            margin_maintenance: 0.0,
            server_utc_offset_s: 7200,
        }
    }

    fn load_from_str(csv_text: &str, s: &SymbolSpec) -> (Vec<Bar>, LoadReport) {
        let cursor = std::io::Cursor::new(csv_text.as_bytes().to_vec());
        let mut rdr = csv::ReaderBuilder::new().flexible(true).from_reader(cursor);
        load_bars_from_reader(&mut rdr, s).unwrap()
    }

    #[test]
    fn converts_to_utc_and_sorts_dedups() {
        // Server time = UTC+2h. epoch_server values are clock-readings as-if-UTC.
        // ("2024.01.02 10:00" as-if-UTC == 1704189600.)
        let csv = "time,open,high,low,close,tick_volume,spread,real_volume,time_epoch\n\
                   2024.01.02 10:02,2050.0,2051.0,2049.0,2050.5,10,20,0,1704189720\n\
                   2024.01.02 10:00,2050.0,2050.4,2049.8,2050.2,10,20,0,1704189600\n\
                   2024.01.02 10:01,2050.1,2050.5,2049.9,2050.3,11,21,0,1704189660\n\
                   2024.01.02 10:01,2050.1,2050.5,2049.9,2050.3,11,21,0,1704189660\n";
        let (bars, rep) = load_from_str(csv, &spec());
        assert_eq!(rep.rows_total, 4);
        assert_eq!(rep.duplicates, 1);
        assert_eq!(rep.out_of_order, 1);
        assert_eq!(rep.time_epoch_mismatch, 0);
        assert_eq!(rep.bars, 3);
        assert_eq!(bars[0].ts, 1704189600 - 7200);
        assert_eq!(bars[2].ts, 1704189720 - 7200);
        assert_eq!(bars[1].ts - bars[0].ts, 60);
        // Native bars: spread_max == spread_pts
        assert_eq!(bars[0].spread_max, bars[0].spread_pts);
    }

    #[test]
    fn time_without_epoch_column_falls_back_to_parse() {
        let csv = "time,open,high,low,close,tick_volume,spread,real_volume\n\
                   2024.01.02 10:00,2050.0,2050.4,2049.8,2050.2,10,20,0\n";
        let (bars, rep) = load_from_str(csv, &spec());
        assert_eq!(rep.rows_bad_parse, 0);
        assert_eq!(bars[0].ts, 1704189600 - 7200);
    }

    #[test]
    fn bad_rows_counted_not_fatal() {
        let csv = "time,open,high,low,close,tick_volume,spread,real_volume,time_epoch\n\
                   2024.01.02 10:00,2050.0,2050.4,2049.8,2050.2,10,20,0,1704189600\n\
                   not-a-row\n\
                   2024.01.02 10:01,NaN,2050.5,2049.9,2050.3,11,21,0,1704189660\n\
                   2024.01.02 10:02,2050.0,2051.0,2049.0,2050.5,10,20,0,notanumber\n\
                   2024.01.02 10:03,2050.0,2050.4,2049.8,2050.2,10,20,0,1704191000\n";
        let (_bars, rep) = load_from_str(csv, &spec());
        assert_eq!(rep.rows_total, 5);
        // "not-a-row" and the NaN row are unusable; the bad-epoch row falls
        // back to parsing `time`; the last row loads but its epoch disagrees
        // with `time` (10:03 vs 11:23) and is counted as a mismatch.
        assert_eq!(rep.rows_bad_parse, 2);
        assert_eq!(rep.time_epoch_mismatch, 1);
        assert_eq!(rep.bars, 3);
    }
}

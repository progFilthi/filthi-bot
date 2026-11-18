//! Data-quality report generation (README §3 "data-quality report"):
//! parse/clean counts, spread statistics, gap classification with the
//! daily-break hour histogram used to VERIFY the broker timezone offset.

use crate::clean::CleanReport;
use crate::loader::LoadReport;
use chrono::{Datelike, TimeZone, Utc};
use xau_core::Bar;

pub struct QualityData {
    pub symbol: String,
    pub tf_name: String,
    pub load: LoadReport,
    pub clean: CleanReport,
    pub bars: Vec<Bar>,
    pub server_utc_offset_s: i64,
    pub resampled_counts: Vec<(String, usize)>,
}

fn fmt_ts(ts: i64) -> String {
    Utc.timestamp_opt(ts, 0)
        .single()
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| ts.to_string())
}

fn weekday_name(ts: i64) -> &'static str {
    match Utc.timestamp_opt(ts, 0).single().map(|d| d.weekday()) {
        Some(chrono::Weekday::Mon) => "Mon",
        Some(chrono::Weekday::Tue) => "Tue",
        Some(chrono::Weekday::Wed) => "Wed",
        Some(chrono::Weekday::Thu) => "Thu",
        Some(chrono::Weekday::Fri) => "Fri",
        Some(chrono::Weekday::Sat) => "Sat",
        Some(chrono::Weekday::Sun) => "Sun",
        None => "?",
    }
}

pub fn render_quality_markdown(q: &QualityData) -> String {
    let mut s = String::new();
    s.push_str(&format!("# Data quality: {} {}\n\n", q.symbol, q.tf_name));
    s.push_str(&format!(
        "Generated: {} UTC · server_utc_offset_s = {} (server = UTC + {}s)\n\n",
        Utc::now().format("%Y-%m-%d %H:%M"),
        q.server_utc_offset_s,
        q.server_utc_offset_s
    ));

    // Load
    let l = &q.load;
    s.push_str("## Load (CSV -> UTC)\n\n");
    s.push_str("| metric | value |\n|---|---|\n");
    s.push_str(&format!("| source | `{}` |\n", l.path));
    s.push_str(&format!("| rows | {} |\n", l.rows_total));
    s.push_str(&format!("| unparseable rows | {} |\n", l.rows_bad_parse));
    s.push_str(&format!(
        "| duplicate timestamps (dropped) | {} |\n",
        l.duplicates
    ));
    s.push_str(&format!(
        "| out-of-order pairs before sort | {} |\n",
        l.out_of_order
    ));
    s.push_str(&format!(
        "| time/time_epoch mismatches | {} |\n",
        l.time_epoch_mismatch
    ));
    s.push_str(&format!("| bars after sort+dedup | {} |\n", l.bars));
    if let (Some(f), Some(t)) = (l.first_ts, l.last_ts) {
        s.push_str(&format!(
            "| first bar (UTC) | {} ({}) |\n",
            fmt_ts(f),
            weekday_name(f)
        ));
        s.push_str(&format!(
            "| last bar (UTC) | {} ({}) |\n",
            fmt_ts(t),
            weekday_name(t)
        ));
    }
    s.push('\n');

    // Clean
    let c = &q.clean;
    s.push_str("## Cleaning\n\n");
    s.push_str("| metric | value |\n|---|---|\n");
    s.push_str(&format!(
        "| dropped: inconsistent OHLC | {} |\n",
        c.dropped_bad_ohlc
    ));
    s.push_str(&format!(
        "| dropped: non-positive price | {} |\n",
        c.dropped_bad_price
    ));
    s.push_str(&format!(
        "| zero-spread bars (kept, counted) | {} |\n",
        c.zero_spread
    ));
    s.push_str(&format!(
        "| negative-spread bars (kept, counted) | {} |\n",
        c.negative_spread
    ));
    s.push_str(&format!(
        "| outliers flagged (kept) | {} |\n",
        c.outliers_flagged
    ));
    s.push_str(&format!("| bars kept | {} |\n\n", c.output_bars));

    if !c.outlier_examples.is_empty() {
        s.push_str("### Outlier examples (largest single-bar ranges, kept)\n\n");
        s.push_str("| time (UTC) | low | high | range | close |\n|---|---|---|---|---|\n");
        for b in &c.outlier_examples {
            s.push_str(&format!(
                "| {} | {:.2} | {:.2} | {:.2} | {:.2} |\n",
                fmt_ts(b.ts),
                b.low,
                b.high,
                b.high - b.low,
                b.close
            ));
        }
        s.push('\n');
    }

    // Gaps
    let g = &c.gaps;
    s.push_str("## Gaps (cadence = 1 bar)\n\n");
    s.push_str("| class | count |\n|---|---|\n");
    s.push_str(&format!(
        "| weekend (Fri -> Sun/Mon, 40..72h) | {} |\n",
        g.weekend
    ));
    s.push_str(&format!(
        "| daily break (<= 2h, rollover) | {} |\n",
        g.daily_break
    ));
    s.push_str(&format!("| unexpected | {} |\n", g.unexpected));
    s.push_str(&format!(
        "| longest gap | {} ({:.1} h) |\n\n",
        g.longest_gap_s,
        g.longest_gap_s as f64 / 3600.0
    ));

    if !g.unexpected_examples.is_empty() {
        s.push_str("### Unexpected gap examples (first 10)\n\n");
        s.push_str("| before (UTC) | after (UTC) | gap |\n|---|---|---|\n");
        for e in &g.unexpected_examples {
            s.push_str(&format!(
                "| {} ({}) | {} ({}) | {:.1} h |\n",
                fmt_ts(e.ts_before),
                weekday_name(e.ts_before),
                fmt_ts(e.ts_after),
                weekday_name(e.ts_after),
                e.gap_s as f64 / 3600.0
            ));
        }
        s.push('\n');
    }

    // Timezone verification
    s.push_str("## Timezone-offset verification (daily-break histogram)\n\n");
    s.push_str("| hour (UTC) | count | hour (server) | count |\n|---|---|---|---|\n");
    for h in 0..24 {
        if g.daily_break_start_hour_utc[h] > 0 || g.daily_break_start_hour_server[h] > 0 {
            let sh = (h as i64 + q.server_utc_offset_s).rem_euclid(86_400) / 3_600;
            s.push_str(&format!(
                "| {:02}:00 | {} | {:02}:00 | {} |\n",
                h, g.daily_break_start_hour_utc[h], sh, g.daily_break_start_hour_server[h]
            ));
        }
    }
    s.push_str("\nDaily rollover gaps should cluster at ONE server hour (the broker's\n");
    s.push_str("00:00 server time) and one UTC hour that shifts with DST. A consistent\n");
    s.push_str("server hour validates `server_utc_offset_s`; a split across two server\n");
    s.push_str("hours mid-year indicates the configured offset ignores DST.\n\n");

    // Spread stats
    if !q.bars.is_empty() {
        let mut sp: Vec<i64> = q.bars.iter().map(|b| b.spread_pts).collect();
        sp.sort_unstable();
        let p = |f: f64| -> i64 { sp[((sp.len() - 1) as f64 * f) as usize] };
        s.push_str("## Spread (points)\n\n");
        s.push_str("| min | median | p95 | p99 | max |\n|---|---|---|---|---|\n");
        s.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n\n",
            sp[0],
            p(0.50),
            p(0.95),
            p(0.99),
            sp[sp.len() - 1]
        ));
    }

    // Resample summary
    if !q.resampled_counts.is_empty() {
        s.push_str("## Resampled caches written\n\n");
        s.push_str("| timeframe | bars |\n|---|---|\n");
        for (name, n) in &q.resampled_counts {
            s.push_str(&format!("| {} | {} |\n", name, n));
        }
        s.push('\n');
    }

    s
}

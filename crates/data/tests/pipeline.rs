//! Phase 1 integration test: raw CSV -> loader -> cleaner -> resampler ->
//! parquet cache -> readable back, plus report render smoke test.

use std::io::Cursor;
use xau_core::Bar;
use xau_data::{
    clean_bars, load_bars_from_reader, read_bars_parquet, render_quality_markdown, resample,
    write_bars_parquet, QualityData, SymbolSpec,
};

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

#[test]
fn full_pipeline_csv_to_parquet() {
    let csv = "time,open,high,low,close,tick_volume,spread,real_volume,time_epoch\n\
               2024.01.02 10:00,2050.0,2050.4,2049.8,2050.2,10,20,0,1704194400\n\
               2024.01.02 10:01,2050.1,2050.5,2049.9,2050.3,11,21,0,1704194460\n\
               2024.01.02 10:02,2050.2,2050.6,2050.0,2050.4,12,22,0,1704194520\n\
               2024.01.02 10:03,2050.3,2050.7,2050.1,2050.5,13,23,0,1704194580\n\
               2024.01.02 10:04,2050.4,2050.8,2050.2,2050.6,14,24,0,1704194640\n\
               2024.01.02 10:05,2050.5,2050.9,2050.3,2050.7,15,25,0,1704194700\n";
    let cursor = Cursor::new(csv.as_bytes().to_vec());
    let mut rdr = csv::ReaderBuilder::new().flexible(true).from_reader(cursor);
    let (bars, _load) = load_bars_from_reader(&mut rdr, &spec()).unwrap();
    assert_eq!(bars.len(), 6);

    let (cleaned, clean_rep) = clean_bars(&bars, 60, 0.01, 7200);
    assert_eq!(clean_rep.output_bars, 6);

    let m5 = resample(&cleaned, 300);
    assert_eq!(m5.len(), 2);
    assert_eq!(m5[0].ts, 1704194400 - 7200);
    assert_eq!(m5[0].volume, 60); // 10+11+12+13+14
    assert_eq!(m5[0].spread_max, 24);
    assert_eq!(m5[1].spread_pts, 25);

    let dir = std::env::temp_dir().join(format!("xau-pipeline-test-{}", std::process::id()));
    let path = dir.join("xauusd_m5.parquet");
    write_bars_parquet(&path, &m5).unwrap();
    let back: Vec<Bar> = read_bars_parquet(&path).unwrap();
    assert_eq!(back.len(), 2);
    assert_eq!(back[0].ts, m5[0].ts);
    assert_eq!(back[0].spread_max, m5[0].spread_max);

    // Report renders with the key sections.
    let q = QualityData {
        symbol: "XAUUSD".into(),
        tf_name: "M1".into(),
        load: xau_data::LoadReport::default(),
        clean: clean_rep,
        bars: cleaned,
        server_utc_offset_s: 7200,
        resampled_counts: vec![("m5".into(), m5.len())],
    };
    let md = render_quality_markdown(&q);
    assert!(md.contains("# Data quality: XAUUSD M1"));
    assert!(md.contains("## Timezone-offset verification"));
    assert!(md.contains("## Resampled caches written"));

    let _ = std::fs::remove_dir_all(&dir);
}

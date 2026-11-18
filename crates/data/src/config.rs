//! Broker contract specs recorded from `get_marketwatch_symbols` (README §3:
//! "Record contract specs in config"). Values for MetaQuotes-Demo XAUUSD,
//! captured 2026-11-17. `server_utc_offset_s` is verified in the data-quality
//! report via the daily-break histogram.

use anyhow::Context;
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SymbolSpec {
    pub name: String,
    pub digits: i32,
    pub point: f64,
    pub contract_size: f64,
    pub tick_size: f64,
    pub tick_value: f64,
    pub volume_min: f64,
    pub volume_max: f64,
    pub volume_step: f64,
    pub stops_level_pts: i64,
    pub freeze_level_pts: i64,
    /// Swap mode as reported by the broker (1 = points per day).
    #[serde(default)]
    pub swap_mode: i64,
    pub swap_long_pts: f64,
    pub swap_short_pts: f64,
    /// Day of week (1=Mon..7=Sun) the triple swap is applied to.
    #[serde(default)]
    pub swap_rollover3day_dow: i64,
    #[serde(default)]
    pub margin_initial: f64,
    #[serde(default)]
    pub margin_maintenance: f64,
    /// Broker server clock offset from UTC in seconds (server = UTC + this).
    pub server_utc_offset_s: i64,
}

/// Load specs from a `configs/symbols/*.toml` file (`[symbol]` table).
pub fn load_symbol_spec(path: &std::path::Path) -> anyhow::Result<SymbolSpec> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read symbol spec {}", path.display()))?;
    let v: toml::Value = toml::from_str(&raw).context("symbol spec TOML parse error")?;
    let table = v
        .get("symbol")
        .with_context(|| format!("{} missing [symbol] table", path.display()))?;
    let spec: SymbolSpec = table.clone().try_into().context("bad [symbol] fields")?;
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_xauusd_spec() {
        let toml_text = r#"
[symbol]
name = "XAUUSD"
digits = 2
point = 0.01
contract_size = 100.0
tick_size = 0.01
tick_value = 0.1
volume_min = 0.01
volume_max = 100.0
volume_step = 0.01
stops_level_pts = 0
freeze_level_pts = 0
swap_mode = 1
swap_long_pts = -12.6
swap_short_pts = -4.6
swap_rollover3day_dow = 3
margin_initial = 0.0
margin_maintenance = 0.0
server_utc_offset_s = 7200
"#;
        let v: toml::Value = toml::from_str(toml_text).unwrap();
        let spec: SymbolSpec = v.get("symbol").unwrap().clone().try_into().unwrap();
        assert_eq!(spec.name, "XAUUSD");
        assert_eq!(spec.server_utc_offset_s, 7200);
        assert_eq!(spec.volume_min, 0.01);
    }
}

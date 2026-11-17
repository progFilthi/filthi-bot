//! Phase 0 server configuration (TOML).

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default = "default_test_order")]
    pub test_order: TestOrderConfig,
    #[serde(default)]
    pub timeouts: TimeoutConfig,
    #[serde(default = "default_transcript")]
    pub transcript: TranscriptConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct TestOrderConfig {
    /// Send the single test market order that proves the round trip.
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub lots: f64,
    /// Stop distance in points from the entry side.
    pub sl_distance_pts: i64,
    /// Take-profit as R multiple of the stop distance.
    pub tp_rr: f64,
    pub max_slippage_pts: i64,
    pub magic: i64,
    /// Fail fast if the EA reports a non-demo account.
    #[serde(default = "default_true")]
    pub expect_demo: bool,
    /// After the entry fill, exercise the `modify` command (SL closer by 10
    /// points) and then `close`, leaving the account flat.
    #[serde(default = "default_true")]
    pub close_after_fill: bool,
}

impl Default for TestOrderConfig {
    fn default() -> Self {
        default_test_order()
    }
}

fn default_test_order() -> TestOrderConfig {
    TestOrderConfig {
        enabled: true,
        lots: 0.01,
        sl_distance_pts: 100,
        tp_rr: 2.0,
        max_slippage_pts: 30,
        magic: 424242,
        expect_demo: true,
        close_after_fill: true,
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct TimeoutConfig {
    #[serde(default = "default_hello_s")]
    pub hello_s: u64,
    #[serde(default = "default_first_tick_s")]
    pub first_tick_s: u64,
    #[serde(default = "default_fill_s")]
    pub fill_s: u64,
    #[serde(default = "default_overall_s")]
    pub overall_s: u64,
    /// Inbound-activity staleness window (heartbeats are 1/s; README says
    /// halt on >5s silence).
    #[serde(default = "default_stale_ms")]
    pub stale_ms: u64,
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        TimeoutConfig {
            hello_s: default_hello_s(),
            first_tick_s: default_first_tick_s(),
            fill_s: default_fill_s(),
            overall_s: default_overall_s(),
            stale_ms: default_stale_ms(),
        }
    }
}

fn default_hello_s() -> u64 {
    10
}
fn default_first_tick_s() -> u64 {
    30
}
fn default_fill_s() -> u64 {
    30
}
fn default_overall_s() -> u64 {
    180
}
fn default_stale_ms() -> u64 {
    5000
}

#[derive(Debug, Clone, Deserialize)]
pub struct TranscriptConfig {
    pub path: String,
}

impl Default for TranscriptConfig {
    fn default() -> Self {
        default_transcript()
    }
}

fn default_transcript() -> TranscriptConfig {
    TranscriptConfig {
        path: "runs/phase0/transcript.jsonl".into(),
    }
}

fn default_host() -> String {
    "127.0.0.1".into()
}
fn default_port() -> u16 {
    5555
}
fn default_true() -> bool {
    true
}

impl Config {
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("cannot read config {path}: {e}"))?;
        toml::from_str(&raw).map_err(|e| anyhow::anyhow!("bad config {path}: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_toml() {
        let raw = r#"
[server]
host = "127.0.0.1"
port = 5555

[test_order]
lots = 0.01
sl_distance_pts = 100
tp_rr = 2.0
max_slippage_pts = 30
magic = 424242

[timeouts]
overall_s = 120

[transcript]
path = "runs/phase0/transcript.jsonl"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.server.port, 5555);
        assert!(cfg.test_order.enabled);
        assert_eq!(cfg.test_order.magic, 424242);
        assert_eq!(cfg.timeouts.overall_s, 120);
        assert_eq!(cfg.timeouts.stale_ms, 5000);
    }
}

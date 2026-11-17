//! NDJSON wire protocol between the MT5 Bridge EA and the Rust engine.
//!
//! Spec: README section 4. One JSON object per line, every message has
//! `type` and `seq`. Unknown fields are tolerated (serde default) so the EA
//! can carry extra diagnostics without breaking the engine.

use serde::{Deserialize, Serialize};
use xau_core::{OrderRequest, Side};

/// Messages sent by the EA to the engine.
///
/// `seq` fields are parsed for wire fidelity (README section 4) but read at
/// the envelope level (see `SeqEnvelope` in session.rs); the allow silences
/// the dead-code lint for those fields only.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EaMessage {
    Hello {
        seq: i64,
        account: i64,
        server: String,
        symbol: String,
        digits: i32,
        point: f64,
        contract_size: f64,
        min_lot: f64,
        lot_step: f64,
        server_utc_offset_s: i64,
        /// Extension (not in README section 4 yet): broker minimum stop
        /// distance in points, so the engine can pre-validate SL/TP.
        #[serde(default)]
        stops_level_pts: i64,
        demo: bool,
    },
    Tick {
        seq: i64,
        /// Epoch milliseconds as reported by the EA.
        t: i64,
        bid: f64,
        ask: f64,
    },
    Bar {
        seq: i64,
        tf: String,
        t: i64,
        o: f64,
        h: f64,
        l: f64,
        c: f64,
        v: f64,
        spread: i64,
    },
    Fill {
        seq: i64,
        order_id: String,
        ticket: i64,
        price: f64,
        lots: f64,
        time: i64,
        slippage_pts: i64,
    },
    OrderReject {
        seq: i64,
        order_id: String,
        retcode: i64,
        reason: String,
    },
    Account {
        seq: i64,
        balance: f64,
        equity: f64,
        margin: f64,
        free_margin: f64,
    },
    Position {
        seq: i64,
        ticket: i64,
        side: Side,
        lots: f64,
        open_price: f64,
        sl: f64,
        tp: f64,
    },
    Heartbeat {
        seq: i64,
        t: i64,
    },
}

/// Commands sent by the engine to the EA.
///
/// `close_all` is part of the wire protocol (README section 4) but is only
/// sent once the kill switch exists (Phase 3+); the allow documents that.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EaCommand {
    Order {
        seq: i64,
        order_id: String,
        side: Side,
        lots: f64,
        sl: f64,
        tp: f64,
        max_slippage_pts: i64,
        magic: i64,
    },
    Modify {
        seq: i64,
        ticket: i64,
        sl: f64,
        tp: f64,
    },
    Close {
        seq: i64,
        ticket: i64,
    },
    CloseAll {
        seq: i64,
        reason: String,
    },
    Heartbeat {
        seq: i64,
    },
}

impl EaCommand {
    /// Serialize to one NDJSON line (no trailing newline).
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("command serialization cannot fail")
    }
}

/// Build the wire `order` command from a domain `OrderRequest`.
pub fn order_cmd(seq: i64, o: &OrderRequest) -> EaCommand {
    EaCommand::Order {
        seq,
        order_id: o.order_id.clone(),
        side: o.side,
        lots: o.lots,
        sl: o.sl,
        tp: o.tp,
        max_slippage_pts: o.max_slippage_pts,
        magic: o.magic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xau_core::Tick;

    /// Convert a wire tick message into the domain `Tick`.
    fn tick_from_wire(t: &EaMessage) -> Option<Tick> {
        if let EaMessage::Tick {
            t: ts_ms, bid, ask, ..
        } = t
        {
            Some(Tick {
                ts_ms: *ts_ms,
                bid: *bid,
                ask: *ask,
            })
        } else {
            None
        }
    }

    #[test]
    fn parse_hello_with_extra_fields() {
        let line = r#"{"type":"hello","seq":1,"account":115694190,"server":"MetaQuotes-Demo","symbol":"XAUUSD","digits":2,"point":0.01,"contract_size":100,"min_lot":0.01,"lot_step":0.01,"server_utc_offset_s":7200,"stops_level_pts":30,"demo":true,"extra":"ignored"}"#;
        let m: EaMessage = serde_json::from_str(line).unwrap();
        match m {
            EaMessage::Hello {
                account,
                demo,
                stops_level_pts,
                ..
            } => {
                assert_eq!(account, 115694190);
                assert!(demo);
                assert_eq!(stops_level_pts, 30);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn hello_without_extension_field_defaults() {
        // README section 4 example verbatim (no stops_level_pts).
        let line = r#"{"type":"hello","seq":1,"account":12345,"server":"Broker-Demo","symbol":"XAUUSD","digits":2,"point":0.01,"contract_size":100,"min_lot":0.01,"lot_step":0.01,"server_utc_offset_s":7200,"demo":true}"#;
        let m: EaMessage = serde_json::from_str(line).unwrap();
        match m {
            EaMessage::Hello {
                stops_level_pts, ..
            } => assert_eq!(stops_level_pts, 0),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn parse_tick() {
        let line = r#"{"type":"tick","seq":2,"t":1731850000123,"bid":2650.12,"ask":2650.32}"#;
        let m: EaMessage = serde_json::from_str(line).unwrap();
        let t = tick_from_wire(&m).unwrap();
        assert_eq!(t.ts_ms, 1731850000123);
        assert_eq!(t.bid, 2650.12);
        assert_eq!(t.ask, 2650.32);
    }

    #[test]
    fn command_wire_format_matches_readme() {
        let req = OrderRequest {
            order_id: "abc123".into(),
            side: Side::Buy,
            lots: 0.10,
            sl: 2640.0,
            tp: 2670.0,
            max_slippage_pts: 30,
            magic: 424242,
        };
        let line = order_cmd(1, &req).to_line();
        assert_eq!(
            line,
            r#"{"type":"order","seq":1,"order_id":"abc123","side":"buy","lots":0.1,"sl":2640.0,"tp":2670.0,"max_slippage_pts":30,"magic":424242}"#
        );
    }

    #[test]
    fn close_all_wire_format() {
        let cmd = EaCommand::CloseAll {
            seq: 4,
            reason: "kill_switch".into(),
        };
        assert_eq!(
            cmd.to_line(),
            r#"{"type":"close_all","seq":4,"reason":"kill_switch"}"#
        );
    }

    #[test]
    fn unknown_message_type_is_rejected_cleanly() {
        let line = r#"{"type":"warp_drive","seq":9}"#;
        assert!(serde_json::from_str::<EaMessage>(line).is_err());
    }
}

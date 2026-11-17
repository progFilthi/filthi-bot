//! Core domain types shared by every layer of the system.
//!
//! Phase 0 scope: the minimal shapes the live bridge needs (ticks, bars,
//! orders, fills). Backtest/strategy types (events, signals, positions,
//! configs) are added in later phases.

use serde::{Deserialize, Serialize};

/// Instrument this system trades. Hard-coded for now; parameterized if we
/// ever trade more than gold.
pub const SYMBOL: &str = "XAUUSD";

/// Direction of an order or position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn as_str(self) -> &'static str {
        match self {
            Side::Buy => "buy",
            Side::Sell => "sell",
        }
    }

    /// The opposite direction.
    pub fn opposite(self) -> Side {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }
}

/// A market tick: last known bid/ask at millisecond resolution.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Tick {
    /// Unix epoch milliseconds (as reported by the source).
    pub ts_ms: i64,
    pub bid: f64,
    pub ask: f64,
}

impl Tick {
    /// Mid price (bid + ask) / 2.
    pub fn mid(&self) -> f64 {
        (self.bid + self.ask) / 2.0
    }

    /// Spread in price units (ask - bid).
    pub fn spread(&self) -> f64 {
        self.ask - self.bid
    }
}

/// A closed OHLC bar with volume and spread in points.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Bar {
    /// Unix epoch seconds of the bar open time (UTC).
    pub ts: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    /// Tick volume as reported by the broker.
    pub volume: u64,
    /// Spread at the bar in points (per the broker's definition of point).
    pub spread_pts: i64,
}

impl Bar {
    /// True if the bar is self-consistent: high >= max(o,c), low <= min(o,c).
    pub fn is_consistent(&self) -> bool {
        let hi = self.open.max(self.close);
        let lo = self.open.min(self.close);
        self.high >= hi - f64::EPSILON.max(hi.abs() * 1e-9)
            && self.low <= lo + f64::EPSILON.max(lo.abs() * 1e-9)
    }
}

/// A request for the risk layer / executor to place an order.
/// Strategies never build this directly; the risk manager converts signals
/// into sized order requests. Every order MUST carry a stop loss.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderRequest {
    /// Client-generated unique id (UUID). Idempotency key.
    pub order_id: String,
    pub side: Side,
    /// Volume in lots.
    pub lots: f64,
    /// Absolute stop-loss price. Must be > 0.
    pub sl: f64,
    /// Absolute take-profit price, or 0.0 for none.
    pub tp: f64,
    /// Maximum acceptable slippage in points.
    pub max_slippage_pts: i64,
    /// EA magic number to tag the order with.
    pub magic: i64,
}

/// A confirmed execution (or partial execution) reported by the executor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fill {
    pub order_id: String,
    /// Broker ticket (position ticket in hedging mode for market orders).
    pub ticket: i64,
    pub price: f64,
    pub lots: f64,
    /// Unix epoch seconds of the fill.
    pub ts: i64,
    /// Realized slippage in points vs. the reference quote.
    pub slippage_pts: i64,
}

impl Fill {
    /// True if this fill is the acknowledgment of a close/cancel operation
    /// rather than an entry order.
    pub fn is_close_ack(&self) -> bool {
        self.order_id.starts_with("close")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_roundtrip() {
        for s in [Side::Buy, Side::Sell] {
            let json = serde_json::to_string(&s).unwrap();
            assert_eq!(json, format!("\"{}\"", s.as_str()));
            assert_eq!(serde_json::from_str::<Side>(&json).unwrap(), s);
        }
    }

    #[test]
    fn tick_math() {
        let t = Tick {
            ts_ms: 1,
            bid: 2650.12,
            ask: 2650.32,
        };
        assert!((t.mid() - 2650.22).abs() < 1e-9);
        assert!((t.spread() - 0.2).abs() < 1e-9);
    }

    #[test]
    fn bar_consistency() {
        let good = Bar {
            ts: 0,
            open: 2650.0,
            high: 2651.0,
            low: 2649.0,
            close: 2650.5,
            volume: 100,
            spread_pts: 20,
        };
        assert!(good.is_consistent());
        let bad = Bar {
            high: 2648.0,
            ..good
        };
        assert!(!bad.is_consistent());
    }

    #[test]
    fn close_ack_detection() {
        let f = Fill {
            order_id: "close-all-1".into(),
            ticket: 1,
            price: 1.0,
            lots: 0.01,
            ts: 0,
            slippage_pts: 0,
        };
        assert!(f.is_close_ack());
    }
}

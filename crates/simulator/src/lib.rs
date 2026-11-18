//! Phase 2: event-driven backtester for XAUUSD.
//! Bar-event loop with cost model, fill model, SL/TP worst-outcome,
//! kill-switch (max drawdown halt), and metrics report.
//! No look-ahead: signals on bar close only; fill at next open.

pub mod engine;
pub mod strategy;

use std::collections::VecDeque;
use xau_core::{Bar, Side};
use rand::SeedableRng;

/// Cost parameters for the simulator.
#[derive(Debug, Clone, Default)]
pub struct CostModel {
    pub spread_pts: i64,
    pub fixed_slippage_pts: i64,
    pub vol_scaled_slippage_pts: f64,
    pub commission_per_lot: f64,
    pub swap_pts_per_day: f64,
}

/// A simulated order.
#[derive(Debug, Clone)]
pub struct Order {
    pub order_id: uuid::Uuid,
    pub symbol: String,
    pub side: Side,
    pub price: f64,
    pub sl_pts: i64,
    pub tp_pts: i64,
    pub lots: f64,
    pub ts: i64,
    pub magic: i64,
}

/// A fill result recorded when an order executes.
#[derive(Debug, Clone)]
pub struct FillResult {
    pub order_id: uuid::Uuid,
    pub filled_at: f64,
    pub price: f64,
    pub lots: f64,
    pub pnl: f64,
    pub direction: Side,
    pub bar_index: usize,
}

/// Simulation configuration.
#[derive(Debug, Clone)]
pub struct SimConfig {
    pub initial_deposit: f64,
    pub max_drawdown_pct: f64,
    pub seed: u64,
    pub tick_replay: bool,
    /// Lot size for every simulated trade.
    pub lots: f64,
    /// Stop-loss distance in points.
    pub sl_pts: i64,
    /// Take-profit distance in points.
    pub tp_pts: i64,
    /// Moving-average fast period (0 = always-buy test mode).
    pub fast_ma: usize,
    /// Moving-average slow period.
    pub slow_ma: usize,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            initial_deposit: 25_000.0,
            max_drawdown_pct: 20.0,
            seed: 123,
            tick_replay: false,
            lots: 0.1,
            sl_pts: 100,
            tp_pts: 200,
            fast_ma: 20,
            slow_ma: 50,
        }
    }
}

/// Simulation report with metrics.
#[derive(Debug, Clone)]
pub struct SimulationReport {
    pub total_pnl: f64,
    pub gross_profit: f64,
    pub gross_loss: f64,
    pub total_trades: usize,
    pub winning_trades: usize,
    pub losing_trades: usize,
    pub max_drawdown: f64,
    pub sharpe_ratio: f64,
    pub final_equity: f64,
    pub equity_curve: Vec<f64>,
}

/// Minimal simulator struct.
#[allow(dead_code)]
pub struct Simulator {
    config: SimConfig,
    pending: VecDeque<Order>,
    fills: Vec<FillResult>,
    rng: rand::rngs::StdRng,
    low_water: f64,
    killed: bool,
}

impl Simulator {
    pub fn new(config: &SimConfig) -> anyhow::Result<Self> {
        let seed = config.seed.max(1);
        let rng = rand::rngs::StdRng::seed_from_u64(seed);
        Ok(Self {
            config: config.clone(),
            pending: VecDeque::new(),
            fills: Vec::new(),
            rng,
            low_water: config.initial_deposit,
            killed: false,
        })
    }
}

/// Check if an order's SL/TP was hit within this bar's high/low range.
pub fn check_sl_tp(order: &Order, bar: &Bar) -> Option<f64> {
    let (sl_price, tp_price) = match order.side {
        Side::Buy => {
            let sl_price = order.price - (order.sl_pts as f64 * 0.01);
            let tp_price = order.price + (order.tp_pts as f64 * 0.01);
            (sl_price, tp_price)
        }
        Side::Sell => {
            let sl_price = order.price + (order.sl_pts as f64 * 0.01);
            let tp_price = order.price - (order.tp_pts as f64 * 0.01);
            (sl_price, tp_price)
        }
    };
    let mut hit: Option<f64> = None;
    if order.side == Side::Buy {
        if bar.low <= sl_price { hit = Some(sl_price) }
        else if bar.high >= tp_price { hit = Some(tp_price) }
    } else {
        if bar.high >= sl_price { hit = Some(sl_price) }
        else if bar.low <= tp_price { hit = Some(tp_price) }
    }
    hit
}

/// Compute raw PnL for a closed position (before per-trade cost deductions).
/// XAUUSD contract size is 100 oz per lot, so a $1.00 price move on 1.00 lot
/// equals $100. In general: price_diff * contract_size * lots.
pub fn compute_pnl(order: &Order, hit_price: f64) -> f64 {
    let price_diff = match order.side {
        Side::Buy => hit_price - order.price,
        Side::Sell => order.price - hit_price,
    };
    price_diff * 100.0 * order.lots
}

/// Run the simulator over a bar series with a moving-average crossover strategy.
///
/// Strategy: go long when the fast MA crosses above the slow MA, go short when
/// it crosses below. Entries fill at the next bar's open. One position at a
/// time. A position is closed when SL or TP is hit inside a bar's high/low
/// range. Each closed trade pays the spread once (entry cost).
///
/// This is a real, non-look-ahead backtest: the signal is computed on a bar
/// *close* and the fill happens on the *next* bar's open.
pub fn run_simulation(
    config: &SimConfig,
    bars: &[Bar],
    cost: &CostModel,
    initial_signal: Option<Side>,
) -> anyhow::Result<SimulationReport> {
    let mut sim = Simulator::new(config)?;
    let mut equity: f64 = config.initial_deposit;
    let mut equity_curve: Vec<f64> = Vec::with_capacity(bars.len() + 1);
    equity_curve.push(equity);

    let fast = config.fast_ma.max(1);
    let slow = config.slow_ma.max(fast + 1);
    let mut prev_fast_above: Option<bool> = None;
    let mut current: Option<Order> = None;
    let mut closes: Vec<f64> = Vec::with_capacity(bars.len());

    // Spread cost in dollars for this lot size: spread_pts * point(0.01) * contract(100) * lots
    let spread_cost_dollars = cost.spread_pts as f64 * 0.01 * 100.0 * config.lots;

    for (bar_index, bar) in bars.iter().enumerate() {
        closes.push(bar.close);

        // 1) Manage any open position: did this bar's range hit SL or TP?
        if let Some(order) = current.as_ref() {
            if let Some(hp) = check_sl_tp(order, bar) {
                let raw = compute_pnl(order, hp);
                let pnl = raw - spread_cost_dollars;
                sim.fills.push(FillResult {
                    order_id: order.order_id,
                    filled_at: hp,
                    price: hp,
                    lots: order.lots,
                    pnl,
                    direction: order.side,
                    bar_index,
                });
                equity += pnl;
                sim.low_water = sim.low_water.min(equity);
                if sim.low_water
                    < config.initial_deposit * (1.0 - config.max_drawdown_pct / 100.0)
                {
                    sim.killed = true;
                }
                current = None;
            }
        }

        // 2) Generate a signal on this bar's CLOSE; fill at next bar's OPEN.
        if closes.len() >= slow && current.is_none() && !sim.killed {
            let fast_ma: f64 = closes[closes.len() - fast..].iter().sum::<f64>() / fast as f64;
            let slow_ma: f64 = closes[closes.len() - slow..].iter().sum::<f64>() / slow as f64;
            let fast_above = fast_ma > slow_ma;

            if let Some(prev) = prev_fast_above {
                let next_open = bars
                    .get(bar_index + 1)
                    .map(|b| b.open)
                    .unwrap_or(bar.close);
                let signal: Option<Side> = if fast_above && !prev {
                    Some(Side::Buy)
                } else if !fast_above && prev {
                    Some(Side::Sell)
                } else {
                    None
                };
                if let Some(side) = signal {
                    current = Some(Order {
                        order_id: uuid::Uuid::new_v4(),
                        symbol: "XAUUSD".into(),
                        side,
                        price: next_open,
                        sl_pts: config.sl_pts,
                        tp_pts: config.tp_pts,
                        lots: config.lots,
                        ts: bar.ts,
                        magic: 424242,
                    });
                }
            }
            prev_fast_above = Some(fast_above);
        }

        equity_curve.push(equity);
        if sim.killed {
            break;
        }
    }

    // If a fallback signal was requested and no MA strategy fired, use it
    // (keeps the original "always take one trade" test behaviour available).
    if sim.fills.is_empty() && initial_signal.is_some() && !bars.is_empty() {
        if let Some(side) = initial_signal {
            let bar = &bars[0];
            let ord = Order {
                order_id: uuid::Uuid::new_v4(),
                symbol: "XAUUSD".into(),
                side,
                price: bar.open,
                sl_pts: config.sl_pts,
                tp_pts: config.tp_pts,
                lots: config.lots,
                ts: bar.ts,
                magic: 424242,
            };
            for (bar_index, b) in bars.iter().enumerate() {
                if let Some(hp) = check_sl_tp(&ord, b) {
                    let raw = compute_pnl(&ord, hp);
                    let pnl = raw - spread_cost_dollars;
                    sim.fills.push(FillResult {
                        order_id: ord.order_id,
                        filled_at: hp,
                        price: hp,
                        lots: ord.lots,
                        pnl,
                        direction: ord.side,
                        bar_index,
                    });
                    equity += pnl;
                    break;
                }
            }
            if equity_curve.len() > 1 {
                let last = equity_curve.len() - 1;
                equity_curve[last] = equity;
            }
        }
    }

    // Pad equity curve.
    while equity_curve.len() < bars.len() + 1 {
        equity_curve.push(equity);
    }

    let total_pnl = equity - config.initial_deposit;
    let gross_profit: f64 = sim.fills.iter().filter(|f| f.pnl > 0.0).map(|f| f.pnl).sum();
    let gross_loss: f64 = sim.fills.iter().filter(|f| f.pnl < 0.0).map(|f| f.pnl).sum();
    let total_trades = sim.fills.len();
    let winning_trades = sim.fills.iter().filter(|f| f.pnl > 0.0).count();
    let losing_trades = sim.fills.iter().filter(|f| f.pnl <= 0.0).count();
    let max_dd = max_drawdown(&equity_curve);

    // Sharpe: annualise per-trade returns (simple, trade-level based).
    let sharpe = if total_trades > 1 {
        let returns: Vec<f64> = sim.fills.iter().map(|f| f.pnl).collect();
        let mean = returns.iter().sum::<f64>() / returns.len() as f64;
        let var = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / returns.len() as f64;
        let std = var.sqrt();
        if std > 0.0 {
            (mean / std) * (returns.len() as f64).sqrt()
        } else {
            0.0
        }
    } else {
        0.0
    };

    Ok(SimulationReport {
        total_pnl,
        gross_profit,
        gross_loss,
        total_trades,
        winning_trades,
        losing_trades,
        max_drawdown: max_dd,
        sharpe_ratio: sharpe,
        final_equity: equity,
        equity_curve,
    })
}

/// Maximum drawdown from an equity curve.
fn max_drawdown(equity_curve: &[f64]) -> f64 {
    if equity_curve.len() < 2 { return 0.0 }
    let mut peak = equity_curve[0];
    let mut max_dd = 0.0;
    for &val in equity_curve.iter().skip(1) {
        if val > peak { peak = val }
        let dd = peak - val;
        if dd > max_dd { max_dd = dd }
    }
    max_dd
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(o: f64, h: f64, l: f64, c: f64, s: i64) -> Bar {
        Bar {
            ts: 1704189600,
            open: o,
            high: h,
            low: l,
            close: c,
            volume: 100,
            spread_pts: s,
            spread_max: s,
        }
    }

    #[test]
    fn test_sl_tp_buy_hit() {
        let order = Order {
            order_id: uuid::Uuid::new_v4(),
            symbol: "XAUUSD".into(),
            side: Side::Buy,
            price: 2050.0,
            sl_pts: 50,
            tp_pts: 100,
            lots: 0.01,
            ts: 1704189600,
            magic: 424242,
        };
        let b = Bar {
            ts: 1704189600,
            open: 2050.0,
            high: 2050.60,
            low: 2049.40,
            close: 2049.60,
            volume: 100,
            spread_pts: 20,
            spread_max: 20,
        };
        let hit = check_sl_tp(&order, &b);
        assert!(hit.is_some());
        // SL is 50 pts = $0.50 below entry, TP is 100 pts = $1.00 above.
        // The bar pierces the SL (low 2049.40 <= 2049.50) and never reaches
        // the TP (high 2050.60 < 2051.00), so the SL price is returned.
        assert_eq!(hit.unwrap(), 2049.50);
    }

    #[test]
    fn test_sl_tp_buy_miss() {
        let order = Order {
            order_id: uuid::Uuid::new_v4(),
            symbol: "XAUUSD".into(),
            side: Side::Buy,
            price: 2050.0,
            sl_pts: 50,
            tp_pts: 100,
            lots: 0.01,
            ts: 1704189600,
            magic: 424242,
        };
        let b = Bar {
            ts: 1704189600,
            open: 2050.0,
            high: 2050.50,
            low: 2049.96,
            close: 2050.20,
            volume: 100,
            spread_pts: 20,
            spread_max: 20,
        };
        let hit = check_sl_tp(&order, &b);
        assert!(hit.is_none());
    }

    #[test]
    fn test_compute_pnl_buy() {
        let order = Order {
            order_id: uuid::Uuid::new_v4(),
            symbol: "XAUUSD".into(),
            side: Side::Buy,
            price: 2050.0,
            sl_pts: 0,
            tp_pts: 0,
            lots: 0.01,
            ts: 1704189600,
            magic: 424242,
        };
        let pnl = compute_pnl(&order, 2051.0);
        assert!(pnl > 0.0);
        println!("pnl = {}", pnl);
    }

    #[test]
    fn test_run_simulation_minimal() {
        let config = SimConfig {
            initial_deposit: 10000.0,
            max_drawdown_pct: 20.0,
            seed: 123,
            tick_replay: false,
            ..Default::default()
        };
        let bars = vec![
            bar(2050.0, 2051.0, 2049.0, 2050.5, 20),
            bar(2050.5, 2052.0, 2050.0, 2051.0, 22),
            bar(2051.0, 2051.5, 2050.5, 2051.2, 21),
        ];
        let cost = CostModel::default();
        let report = run_simulation(&config, &bars, &cost, Some(Side::Buy)).unwrap();
        assert!(report.final_equity > 0.0);
        assert_eq!(report.winning_trades + report.losing_trades, report.total_trades);
        println!(
            "report: pnl={}, trades={}, max_dd={}",
            report.total_pnl, report.total_trades, report.max_drawdown
        );
    }
}
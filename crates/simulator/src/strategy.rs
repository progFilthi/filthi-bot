//! Strategy library and indicator math for the XAUUSD backtester.
//!
//! Every signal here is computed from closed bars only and is consumed on the
//! *next* bar's open, so there is no look-ahead.

use xau_core::{Bar, Side};

// ---------------------------------------------------------------------------
// Indicators
// ---------------------------------------------------------------------------

/// Simple moving average. Entries before the window is full are NaN.
pub fn sma(values: &[f64], period: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; values.len()];
    if period == 0 || values.len() < period {
        return out;
    }
    let mut sum: f64 = values[..period].iter().sum();
    out[period - 1] = sum / period as f64;
    for i in period..values.len() {
        sum += values[i] - values[i - period];
        out[i] = sum / period as f64;
    }
    out
}

/// Exponential moving average, seeded with the SMA of the first window.
pub fn ema(values: &[f64], period: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; values.len()];
    if period == 0 || values.len() < period {
        return out;
    }
    let k = 2.0 / (period as f64 + 1.0);
    let seed: f64 = values[..period].iter().sum::<f64>() / period as f64;
    out[period - 1] = seed;
    let mut prev = seed;
    for i in period..values.len() {
        prev = values[i] * k + prev * (1.0 - k);
        out[i] = prev;
    }
    out
}

/// Rolling population standard deviation.
pub fn stddev(values: &[f64], period: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; values.len()];
    if period < 2 || values.len() < period {
        return out;
    }
    for i in (period - 1)..values.len() {
        let win = &values[i + 1 - period..=i];
        let m = win.iter().sum::<f64>() / period as f64;
        let var = win.iter().map(|v| (v - m).powi(2)).sum::<f64>() / period as f64;
        out[i] = var.sqrt();
    }
    out
}

/// Wilder's ATR in price units.
pub fn atr(bars: &[Bar], period: usize) -> Vec<f64> {
    let n = bars.len();
    let mut out = vec![f64::NAN; n];
    if period == 0 || n < period + 1 {
        return out;
    }
    let mut trs = Vec::with_capacity(n);
    trs.push(bars[0].high - bars[0].low);
    for i in 1..n {
        let p = &bars[i - 1];
        let b = &bars[i];
        trs.push(
            (b.high - b.low)
                .max((b.high - p.close).abs())
                .max((b.low - p.close).abs()),
        );
    }
    let seed: f64 = trs[1..=period].iter().sum::<f64>() / period as f64;
    out[period] = seed;
    let mut prev = seed;
    for i in (period + 1)..n {
        prev = (prev * (period as f64 - 1.0) + trs[i]) / period as f64;
        out[i] = prev;
    }
    out
}

/// Highest high / lowest low of the `lookback` bars *before* index i.
/// Excludes bar i so a breakout is genuine.
pub fn donchian(bars: &[Bar], lookback: usize) -> (Vec<f64>, Vec<f64>) {
    let n = bars.len();
    let mut hi = vec![f64::NAN; n];
    let mut lo = vec![f64::NAN; n];
    if lookback == 0 || n <= lookback {
        return (hi, lo);
    }
    for i in lookback..n {
        let win = &bars[i - lookback..i];
        hi[i] = win.iter().map(|b| b.high).fold(f64::NEG_INFINITY, f64::max);
        lo[i] = win.iter().map(|b| b.low).fold(f64::INFINITY, f64::min);
    }
    (hi, lo)
}

/// Relative strength index.
pub fn rsi(values: &[f64], period: usize) -> Vec<f64> {
    let n = values.len();
    let mut out = vec![f64::NAN; n];
    if period == 0 || n <= period {
        return out;
    }
    let mut gain = 0.0;
    let mut loss = 0.0;
    for i in 1..=period {
        let d = values[i] - values[i - 1];
        if d >= 0.0 { gain += d } else { loss -= d }
    }
    let mut ag = gain / period as f64;
    let mut al = loss / period as f64;
    out[period] = if al == 0.0 { 100.0 } else { 100.0 - 100.0 / (1.0 + ag / al) };
    for i in (period + 1)..n {
        let d = values[i] - values[i - 1];
        let (g, l) = if d >= 0.0 { (d, 0.0) } else { (0.0, -d) };
        ag = (ag * (period as f64 - 1.0) + g) / period as f64;
        al = (al * (period as f64 - 1.0) + l) / period as f64;
        out[i] = if al == 0.0 { 100.0 } else { 100.0 - 100.0 / (1.0 + ag / al) };
    }
    out
}

// ---------------------------------------------------------------------------
// Strategy definitions
// ---------------------------------------------------------------------------

/// Which strategy family to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StratKind {
    /// Fast/slow MA crossover (momentum). The original baseline.
    MaCross,
    /// Donchian channel breakout (turtle-style trend following).
    Donchian,
    /// Bollinger band fade (mean reversion).
    Bollinger,
    /// Trend filter + pullback entry (buy dips inside an uptrend).
    TrendPullback,
    /// RSI extreme reversal (mean reversion).
    RsiReversion,
}

impl StratKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "ma" | "macross" => Some(Self::MaCross),
            "donchian" | "breakout" | "turtle" => Some(Self::Donchian),
            "bollinger" | "boll" | "mr" => Some(Self::Bollinger),
            "pullback" | "trend" => Some(Self::TrendPullback),
            "rsi" => Some(Self::RsiReversion),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::MaCross => "ma-cross",
            Self::Donchian => "donchian-breakout",
            Self::Bollinger => "bollinger-fade",
            Self::TrendPullback => "trend-pullback",
            Self::RsiReversion => "rsi-reversion",
        }
    }
}

/// Tunable parameters. Unused fields are ignored per strategy family.
#[derive(Debug, Clone, Copy)]
pub struct StratParams {
    pub fast: usize,
    pub slow: usize,
    pub lookback: usize,
    pub band_k: f64,
    pub trend_ma: usize,
    pub rsi_period: usize,
    pub rsi_buy: f64,
    pub rsi_sell: f64,
}

impl Default for StratParams {
    fn default() -> Self {
        Self {
            fast: 20,
            slow: 50,
            lookback: 20,
            band_k: 2.0,
            trend_ma: 200,
            rsi_period: 14,
            rsi_buy: 30.0,
            rsi_sell: 70.0,
        }
    }
}

/// Entry signals, one per bar. `Some(Side)` at index i means "enter at the
/// open of bar i+1 in this direction".
pub fn entry_signals(bars: &[Bar], kind: StratKind, p: &StratParams) -> Vec<Option<Side>> {
    let n = bars.len();
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let mut sig = vec![None; n];

    match kind {
        StratKind::MaCross => {
            let f = sma(&closes, p.fast.max(1));
            let s = sma(&closes, p.slow.max(2));
            for i in 1..n {
                if f[i].is_nan() || s[i].is_nan() || f[i - 1].is_nan() || s[i - 1].is_nan() {
                    continue;
                }
                let above = f[i] > s[i];
                let was = f[i - 1] > s[i - 1];
                if above && !was {
                    sig[i] = Some(Side::Buy);
                } else if !above && was {
                    sig[i] = Some(Side::Sell);
                }
            }
        }
        StratKind::Donchian => {
            let (hi, lo) = donchian(bars, p.lookback.max(2));
            let trend = ema(&closes, p.trend_ma.max(2));
            for i in 1..n {
                if hi[i].is_nan() || lo[i].is_nan() {
                    continue;
                }
                // Breakout confirmed by the close, filtered by the trend EMA
                // when the filter period is available (closes above EMA = up).
                let up_ok = trend[i].is_nan() || closes[i] > trend[i];
                let dn_ok = trend[i].is_nan() || closes[i] < trend[i];
                if closes[i] > hi[i] && up_ok {
                    sig[i] = Some(Side::Buy);
                } else if closes[i] < lo[i] && dn_ok {
                    sig[i] = Some(Side::Sell);
                }
            }
        }
        StratKind::Bollinger => {
            let mid = sma(&closes, p.lookback.max(2));
            let sd = stddev(&closes, p.lookback.max(2));
            for i in 1..n {
                if mid[i].is_nan() || sd[i].is_nan() || sd[i] <= 0.0 {
                    continue;
                }
                let upper = mid[i] + p.band_k * sd[i];
                let lower = mid[i] - p.band_k * sd[i];
                if closes[i] < lower {
                    sig[i] = Some(Side::Buy);
                } else if closes[i] > upper {
                    sig[i] = Some(Side::Sell);
                }
            }
        }
        StratKind::TrendPullback => {
            let trend = ema(&closes, p.trend_ma.max(2));
            let fast = ema(&closes, p.fast.max(1));
            let slow = ema(&closes, p.slow.max(2));
            for i in 1..n {
                if trend[i].is_nan() || fast[i].is_nan() || slow[i].is_nan() {
                    continue;
                }
                if fast[i - 1].is_nan() || slow[i - 1].is_nan() {
                    continue;
                }
                // Uptrend: price above trend EMA, momentum turns back up.
                let turned_up = fast[i] > slow[i] && fast[i - 1] <= slow[i - 1];
                let turned_dn = fast[i] < slow[i] && fast[i - 1] >= slow[i - 1];
                if closes[i] > trend[i] && turned_up {
                    sig[i] = Some(Side::Buy);
                } else if closes[i] < trend[i] && turned_dn {
                    sig[i] = Some(Side::Sell);
                }
            }
        }
        StratKind::RsiReversion => {
            let r = rsi(&closes, p.rsi_period.max(2));
            let trend = ema(&closes, p.trend_ma.max(2));
            for i in 1..n {
                if r[i].is_nan() {
                    continue;
                }
                // Only fade extremes that are aligned with the larger trend,
                // which avoids standing in front of a freight train.
                let up_ok = trend[i].is_nan() || closes[i] > trend[i];
                let dn_ok = trend[i].is_nan() || closes[i] < trend[i];
                if r[i] < p.rsi_buy && up_ok {
                    sig[i] = Some(Side::Buy);
                } else if r[i] > p.rsi_sell && dn_ok {
                    sig[i] = Some(Side::Sell);
                }
            }
        }
    }

    sig
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bars_from(closes: &[f64]) -> Vec<Bar> {
        closes
            .iter()
            .enumerate()
            .map(|(i, c)| Bar {
                ts: 1_700_000_000 + i as i64 * 60,
                open: *c,
                high: c + 1.0,
                low: c - 1.0,
                close: *c,
                volume: 100,
                spread_pts: 20,
                spread_max: 20,
            })
            .collect()
    }

    #[test]
    fn sma_matches_manual() {
        let v = [1.0, 2.0, 3.0, 4.0, 5.0];
        let s = sma(&v, 3);
        assert!(s[0].is_nan() && s[1].is_nan());
        assert!((s[2] - 2.0).abs() < 1e-12);
        assert!((s[4] - 4.0).abs() < 1e-12);
    }

    #[test]
    fn ema_seeds_with_sma() {
        let v = [1.0, 2.0, 3.0, 4.0];
        let e = ema(&v, 2);
        assert!((e[1] - 1.5).abs() < 1e-12);
        assert!(e[3] > e[1]);
    }

    #[test]
    fn rsi_bounds_and_extremes() {
        let up: Vec<f64> = (0..40).map(|i| 100.0 + i as f64).collect();
        let r = rsi(&up, 14);
        assert!(r[20] > 99.0, "monotonic rise should pin RSI high: {}", r[20]);
        let down: Vec<f64> = (0..40).map(|i| 100.0 - i as f64).collect();
        let r2 = rsi(&down, 14);
        assert!(r2[20] < 1.0, "monotonic fall should pin RSI low: {}", r2[20]);
    }

    #[test]
    fn donchian_excludes_current_bar() {
        let mut bars = bars_from(&[10.0; 10]);
        bars[3].high = 500.0;
        bars[5].high = 900.0; // spike on the bar we will evaluate
        let (hi, _) = donchian(&bars, 5);
        // hi[5] looks at bars 0..5, so it sees bar 3's spike but must NOT see
        // bar 5's own spike.
        assert_eq!(hi[5], 500.0);
        // hi[6] looks at bars 1..6, so bar 5's spike is now inside the window.
        assert_eq!(hi[6], 900.0);
    }

    #[test]
    fn donchian_breakout_fires_long() {
        let mut bars = bars_from(&[100.0; 30]);
        // Flat regime then a strong breakout close.
        bars[29].close = 130.0;
        bars[29].high = 131.0;
        let p = StratParams {
            lookback: 10,
            trend_ma: 5,
            ..Default::default()
        };
        let sig = entry_signals(&bars, StratKind::Donchian, &p);
        assert_eq!(sig[29], Some(Side::Buy));
    }

    #[test]
    fn bollinger_fades_upper_band() {
        let mut closes: Vec<f64> = vec![100.0; 40];
        closes[39] = 200.0;
        let bars = bars_from(&closes);
        let p = StratParams {
            lookback: 20,
            band_k: 2.0,
            ..Default::default()
        };
        let sig = entry_signals(&bars, StratKind::Bollinger, &p);
        assert_eq!(sig[39], Some(Side::Sell));
    }

    #[test]
    fn ma_cross_fires_on_crossover_only() {
        let mut closes: Vec<f64> = vec![100.0; 30];
        for i in 30..50 {
            closes.push(100.0 + (i - 29) as f64 * 2.0);
        }
        let bars = bars_from(&closes);
        let p = StratParams {
            fast: 3,
            slow: 8,
            ..Default::default()
        };
        let sig = entry_signals(&bars, StratKind::MaCross, &p);
        let longs = sig.iter().filter(|s| **s == Some(Side::Buy)).count();
        assert_eq!(longs, 1, "one clean crossover should fire exactly once");
    }
}

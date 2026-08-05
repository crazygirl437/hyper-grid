use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

use crate::{GridError, GridResult};

/// Minimal OHLCV bar used for ATR computation (shared shape with exchange candles).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OhlcBar {
    /// Open time in unix seconds.
    pub time: i64,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AtrMetrics {
    pub atr: Decimal,
    /// ATR as percent of last close.
    pub atr_pct: Decimal,
    pub period: u32,
    pub bar_count: usize,
}

/// Suggest grid half-width as percent of mid from ATR.
/// Mirrors `suggestGridRangePct` in apps/desktop/src/lib/pairScore.ts.
pub fn suggest_half_width_pct(atr_pct: Decimal, atr_mult: Decimal) -> Decimal {
    let raw = (atr_pct * atr_mult).max(dec!(2)).min(dec!(12));
    // Round to 1 decimal place.
    (raw * dec!(10)).round() / dec!(10)
}

/// Compute simple ATR(period) from chronological OHLC bars.
pub fn compute_atr(bars: &[OhlcBar], period: u32) -> GridResult<AtrMetrics> {
    let period = period.max(1) as usize;
    if bars.len() < period + 1 {
        return Err(GridError::InvalidConfig(format!(
            "need at least {} bars for ATR({period}), got {}",
            period + 1,
            bars.len()
        )));
    }
    let start = bars.len().saturating_sub(period + 2).max(0);
    let window = &bars[start..];
    let mut trs = Vec::with_capacity(window.len());
    for i in 1..window.len() {
        let h = window[i].high;
        let l = window[i].low;
        let prev = window[i - 1].close;
        let tr = (h - l)
            .max((h - prev).abs())
            .max((l - prev).abs());
        trs.push(tr);
    }
    if trs.len() < period {
        return Err(GridError::InvalidConfig(format!(
            "insufficient true ranges for ATR({period})"
        )));
    }
    let atr_window = &trs[trs.len() - period..];
    let atr = atr_window.iter().copied().sum::<Decimal>() / Decimal::from(period as u64);
    let last_close = window.last().map(|b| b.close).unwrap_or(Decimal::ZERO);
    if last_close <= Decimal::ZERO {
        return Err(GridError::InvalidConfig("last close must be positive".into()));
    }
    let atr_pct = (atr / last_close) * Decimal::from(100);
    Ok(AtrMetrics {
        atr: atr.round_dp(8),
        atr_pct: atr_pct.round_dp(6),
        period: period as u32,
        bar_count: window.len(),
    })
}

/// Derive [lower, upper] around mid using ATR half-width percent.
pub fn derive_bounds(
    mid: Decimal,
    half_width_pct: Decimal,
) -> GridResult<(Decimal, Decimal)> {
    if mid <= Decimal::ZERO {
        return Err(GridError::InvalidConfig("mid must be positive".into()));
    }
    if half_width_pct <= Decimal::ZERO || half_width_pct >= Decimal::from(90) {
        return Err(GridError::InvalidConfig(
            "half_width_pct must be in (0, 90)".into(),
        ));
    }
    let factor = half_width_pct / Decimal::from(100);
    let lower = mid * (Decimal::ONE - factor);
    let upper = mid * (Decimal::ONE + factor);
    if lower >= upper {
        return Err(GridError::InvalidConfig("derived bounds invalid".into()));
    }
    Ok((lower.round_dp(8), upper.round_dp(8)))
}

/// True when `price` is outside [lower, upper].
pub fn is_outside_bounds(price: Decimal, lower: Decimal, upper: Decimal) -> bool {
    price < lower || price > upper
}

/// True when price has re-entered with hysteresis (inside by `hysteresis_pct` of width).
pub fn reentered_with_hysteresis(
    price: Decimal,
    lower: Decimal,
    upper: Decimal,
    hysteresis_pct: Decimal,
) -> bool {
    if upper <= lower {
        return false;
    }
    let width = upper - lower;
    let pad = width * (hysteresis_pct.max(Decimal::ZERO) / Decimal::from(100));
    price >= lower + pad && price <= upper - pad
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn bars_flat(close: Decimal, n: usize) -> Vec<OhlcBar> {
        (0..n)
            .map(|i| OhlcBar {
                time: i as i64,
                open: close,
                high: close + dec!(1),
                low: close - dec!(1),
                close,
            })
            .collect()
    }

    #[test]
    fn atr_on_flat_range() {
        let bars = bars_flat(dec!(100), 20);
        let m = compute_atr(&bars, 14).unwrap();
        assert_eq!(m.atr, dec!(2)); // high-low = 2
        assert_eq!(m.atr_pct, dec!(2));
    }

    #[test]
    fn derive_bounds_symmetric() {
        let (lo, hi) = derive_bounds(dec!(100), dec!(5)).unwrap();
        assert_eq!(lo, dec!(95));
        assert_eq!(hi, dec!(105));
    }

    #[test]
    fn suggest_half_width_clamped() {
        assert_eq!(suggest_half_width_pct(dec!(1), dec!(1.25)), dec!(2));
        assert_eq!(suggest_half_width_pct(dec!(20), dec!(1.25)), dec!(12));
    }

    #[test]
    fn hysteresis_reentry() {
        assert!(!reentered_with_hysteresis(dec!(90), dec!(90), dec!(110), dec!(10)));
        assert!(reentered_with_hysteresis(dec!(100), dec!(90), dec!(110), dec!(10)));
    }
}

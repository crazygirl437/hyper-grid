use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::{
    types::{GridConfig, GridLevel, GridSpacing, Side},
    GridError, GridResult,
};

pub fn generate_levels(config: &GridConfig, mid_price: Decimal) -> GridResult<Vec<GridLevel>> {
    generate_levels_with_bounds(config, mid_price, config.lower_price, config.upper_price)
}

/// Generate levels using explicit active bounds (for recentered grids).
pub fn generate_levels_with_bounds(
    config: &GridConfig,
    mid_price: Decimal,
    lower: Decimal,
    upper: Decimal,
) -> GridResult<Vec<GridLevel>> {
    config.validate()?;
    if lower <= Decimal::ZERO || upper <= Decimal::ZERO || lower >= upper {
        return Err(GridError::InvalidConfig(
            "active bounds must be positive with lower < upper".into(),
        ));
    }
    let n = config.grid_count;
    let size = config.size_per_level()?;
    let mut prices = Vec::with_capacity(n as usize);

    match config.spacing {
        GridSpacing::Arithmetic => {
            let step = (upper - lower) / Decimal::from(n - 1);
            for i in 0..n {
                prices.push(lower + step * Decimal::from(i));
            }
        }
        GridSpacing::Geometric => {
            if lower <= Decimal::ZERO {
                return Err(GridError::InvalidConfig(
                    "geometric grid requires positive lower_price".into(),
                ));
            }
            let ratio = (upper / lower)
                .powf(((n - 1) as f64).recip())
                .map_err(|_| GridError::InvalidConfig("invalid geometric ratio".into()))?;
            let mut p = lower;
            for _ in 0..n {
                prices.push(p);
                p *= ratio;
            }
            if let Some(last) = prices.last_mut() {
                *last = upper;
            }
        }
    }

    let mut levels = Vec::new();
    for (index, price) in prices.into_iter().enumerate() {
        let side = if price < mid_price {
            Side::Buy
        } else if price > mid_price {
            Side::Sell
        } else if mid_price - lower <= upper - mid_price {
            Side::Sell
        } else {
            Side::Buy
        };
        levels.push(GridLevel {
            index: index as u32,
            price: price.round_dp(8),
            side,
            size: (size / price.max(dec!(0.00000001))).round_dp(8),
        });
    }
    Ok(levels)
}

trait DecimalPow {
    fn powf(self, exp: f64) -> Result<Decimal, ()>;
}

impl DecimalPow for Decimal {
    fn powf(self, exp: f64) -> Result<Decimal, ()> {
        let v = self.to_string().parse::<f64>().map_err(|_| ())?;
        if v <= 0.0 {
            return Err(());
        }
        Decimal::from_f64_retain(v.powf(exp)).ok_or(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn sample() -> GridConfig {
        GridConfig {
            symbol: "BTC".into(),
            lower_price: dec!(90000),
            upper_price: dec!(100000),
            grid_count: 5,
            total_budget: dec!(1000),
            spacing: GridSpacing::Arithmetic,
            breakout_action: crate::BreakoutAction::Pause,
            max_drawdown_pct: dec!(20),
            max_daily_loss: dec!(100),
            max_order_failures: 5,
            market: crate::MarketKind::Perp,
            leverage: 5,
            is_cross: true,
            grid_mode: crate::GridMode::Fixed,
            dynamic: crate::DynamicGridConfig::default(),
        }
    }

    #[test]
    fn arithmetic_levels() {
        let levels = generate_levels(&sample(), dec!(95000)).unwrap();
        assert_eq!(levels.len(), 5);
        assert_eq!(levels[0].price, dec!(90000));
        assert_eq!(levels[4].price, dec!(100000));
        assert!(levels.iter().any(|l| l.side == Side::Buy));
        assert!(levels.iter().any(|l| l.side == Side::Sell));
    }

    #[test]
    fn geometric_levels() {
        let mut cfg = sample();
        cfg.spacing = GridSpacing::Geometric;
        let levels = generate_levels(&cfg, dec!(95000)).unwrap();
        assert_eq!(levels.len(), 5);
        assert_eq!(levels[0].price, dec!(90000));
        assert_eq!(levels[4].price, dec!(100000));
    }
}

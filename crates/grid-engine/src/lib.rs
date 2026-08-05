use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

mod engine;
mod levels;
mod risk;
mod types;
mod volatility;

pub use engine::{EngineEvent, GridEngine, RecenterPlan};
pub use levels::{generate_levels, generate_levels_with_bounds};
pub use risk::{RiskConfig, RiskState};
pub use types::*;
pub use volatility::{
    compute_atr, derive_bounds, is_outside_bounds, reentered_with_hysteresis,
    suggest_half_width_pct, AtrMetrics, OhlcBar,
};

#[derive(Debug, Error)]
pub enum GridError {
    #[error("invalid config: {0}")]
    InvalidConfig(String),
    #[error("engine not running")]
    NotRunning,
    #[error("engine already running")]
    AlreadyRunning,
    #[error("risk halt: {0}")]
    RiskHalt(String),
    #[error("{0}")]
    Other(String),
}

pub type GridResult<T> = Result<T, GridError>;

/// Hyperliquid: maintenance margin ≈ half of initial margin at the asset's max leverage.
/// `mmr = 1 / (2 * max_leverage)`.
pub fn mmr_from_max_leverage(max_leverage: u32) -> Decimal {
    let max_lev = Decimal::from(max_leverage.max(1));
    (Decimal::ONE / (Decimal::from(2) * max_lev)).round_dp(8)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GridPreview {
    pub levels: Vec<GridLevel>,
    pub buy_count: usize,
    pub sell_count: usize,
    pub size_per_level: Decimal,
    pub estimated_quote_needed: Decimal,
    pub estimated_base_needed: Decimal,
    /// Worst loss inside the range (USDC): liquidation margin wipe if triggered, else floating loss at bound.
    pub max_loss_in_range: Decimal,
    /// What drives max loss: `"lower"` | `"upper"` | `"long_liq"` | `"short_liq"`.
    pub max_loss_at: String,
    /// Estimated initial margin for the larger one-sided fill (`side_notional / leverage`).
    pub estimated_margin: Decimal,
    /// Account equity remaining at the worst bound, assuming isolated IM = `estimated_margin`.
    pub worst_equity_isolated: Decimal,
    /// Margin ratio (%) at the worst bound under isolated IM assumptions.
    pub worst_margin_ratio_pct: Decimal,
    /// Isolated: liquidation price enters the grid range along the fill path.
    pub isolated_liquidation_risk: bool,
    /// Cross: same path check if usable equity were only `estimated_margin`.
    pub cross_liq_risk_on_strategy_margin: bool,
    /// Cross: path check using `account_equity` (when provided).
    pub cross_liquidation_risk: Option<bool>,
    /// Estimated mark price where a long would be liquidated while walking down the grid.
    #[serde(default)]
    pub estimated_long_liq_price: Option<Decimal>,
    /// Estimated mark price where a short would be liquidated while walking up the grid.
    #[serde(default)]
    pub estimated_short_liq_price: Option<Decimal>,
    pub leverage: u32,
    pub is_cross: bool,
    /// Maintenance margin rate used for the check.
    pub assumed_mmr: Decimal,
    /// Exchange max leverage used to derive MMR.
    #[serde(default)]
    pub max_leverage: u32,
}

#[derive(Debug, Clone, Copy)]
struct SideExtreme {
    entry_notional: Decimal,
    mark_notional: Decimal,
    upnl: Decimal,
}

fn side_extreme(levels: &[GridLevel], side: Side, mark: Decimal) -> Option<SideExtreme> {
    let mut qty = Decimal::ZERO;
    let mut entry_notional = Decimal::ZERO;
    for l in levels.iter().filter(|l| l.side == side) {
        qty += l.size;
        entry_notional += l.price * l.size;
    }
    if qty <= Decimal::ZERO {
        return None;
    }
    let avg = entry_notional / qty;
    let upnl = match side {
        Side::Buy => qty * (mark - avg),
        Side::Sell => qty * (avg - mark),
    };
    Some(SideExtreme {
        entry_notional,
        mark_notional: qty * mark,
        upnl,
    })
}

/// Isolated / fixed-margin long liquidation price.
/// `liq = (qty * entry - im) / (qty * (1 - mmr))`
fn liq_price_long(im: Decimal, qty: Decimal, entry: Decimal, mmr: Decimal) -> Option<Decimal> {
    if qty <= Decimal::ZERO || mmr >= Decimal::ONE {
        return None;
    }
    let denom = qty * (Decimal::ONE - mmr);
    if denom <= Decimal::ZERO {
        return None;
    }
    let px = (qty * entry - im) / denom;
    if px > Decimal::ZERO {
        Some(px)
    } else {
        None
    }
}

/// Isolated / fixed-margin short liquidation price.
/// `liq = (im / qty + entry) / (1 + mmr)`
fn liq_price_short(im: Decimal, qty: Decimal, entry: Decimal, mmr: Decimal) -> Option<Decimal> {
    if qty <= Decimal::ZERO {
        return None;
    }
    let px = (im / qty + entry) / (Decimal::ONE + mmr);
    if px > Decimal::ZERO {
        Some(px)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy)]
struct PathLiqResult {
    risk: bool,
    /// Mark price where liquidation would actually trigger while walking the grid.
    trigger_liq: Option<Decimal>,
    /// Margin locked in the position when liquidation triggers (isolated wipe ≈ this).
    margin_at_trigger: Option<Decimal>,
}

/// Simulate buys filling as price falls (highest buy first). Isolated IM grows with each fill.
fn path_long_isolated(
    levels: &[GridLevel],
    lower: Decimal,
    leverage: Decimal,
    mmr: Decimal,
) -> PathLiqResult {
    let mut buys: Vec<&GridLevel> = levels.iter().filter(|l| l.side == Side::Buy).collect();
    buys.sort_by(|a, b| b.price.cmp(&a.price));
    let mut qty = Decimal::ZERO;
    let mut cost = Decimal::ZERO;
    let mut im = Decimal::ZERO;
    for (i, l) in buys.iter().enumerate() {
        let fill_notional = l.price * l.size;
        qty += l.size;
        cost += fill_notional;
        im += fill_notional / leverage;
        let entry = cost / qty;
        let Some(liq) = liq_price_long(im, qty, entry, mmr) else {
            continue;
        };
        let next_mark = buys.get(i + 1).map(|n| n.price).unwrap_or(lower);
        // Price falls from current fill toward next_mark/lower; liquidate if liq is reached first.
        if liq <= l.price && liq >= next_mark {
            return PathLiqResult {
                risk: liq > lower,
                trigger_liq: Some(liq),
                margin_at_trigger: Some(im),
            };
        }
    }
    PathLiqResult {
        risk: false,
        trigger_liq: None,
        margin_at_trigger: None,
    }
}

/// Simulate sells filling as price rises (lowest sell first). Isolated IM grows with each fill.
fn path_short_isolated(
    levels: &[GridLevel],
    upper: Decimal,
    leverage: Decimal,
    mmr: Decimal,
) -> PathLiqResult {
    let mut sells: Vec<&GridLevel> = levels.iter().filter(|l| l.side == Side::Sell).collect();
    sells.sort_by(|a, b| a.price.cmp(&b.price));
    let mut qty = Decimal::ZERO;
    let mut cost = Decimal::ZERO;
    let mut im = Decimal::ZERO;
    for (i, l) in sells.iter().enumerate() {
        let fill_notional = l.price * l.size;
        qty += l.size;
        cost += fill_notional;
        im += fill_notional / leverage;
        let entry = cost / qty;
        let Some(liq) = liq_price_short(im, qty, entry, mmr) else {
            continue;
        };
        let next_mark = sells.get(i + 1).map(|n| n.price).unwrap_or(upper);
        // Price rises from current fill toward next_mark/upper; liquidate if liq is reached first.
        if liq >= l.price && liq <= next_mark {
            return PathLiqResult {
                risk: liq < upper,
                trigger_liq: Some(liq),
                margin_at_trigger: Some(im),
            };
        }
    }
    PathLiqResult {
        risk: false,
        trigger_liq: None,
        margin_at_trigger: None,
    }
}

/// Cross-style path: margin pool is fixed (account equity or strategy budget), does not grow with fills.
fn path_long_fixed_margin(
    levels: &[GridLevel],
    lower: Decimal,
    margin: Decimal,
    mmr: Decimal,
) -> PathLiqResult {
    let mut buys: Vec<&GridLevel> = levels.iter().filter(|l| l.side == Side::Buy).collect();
    buys.sort_by(|a, b| b.price.cmp(&a.price));
    let mut qty = Decimal::ZERO;
    let mut cost = Decimal::ZERO;
    for (i, l) in buys.iter().enumerate() {
        qty += l.size;
        cost += l.price * l.size;
        let entry = cost / qty;
        let Some(liq) = liq_price_long(margin, qty, entry, mmr) else {
            continue;
        };
        let next_mark = buys.get(i + 1).map(|n| n.price).unwrap_or(lower);
        if liq <= l.price && liq >= next_mark {
            return PathLiqResult {
                risk: liq > lower,
                trigger_liq: Some(liq),
                margin_at_trigger: Some(margin),
            };
        }
    }
    PathLiqResult {
        risk: false,
        trigger_liq: None,
        margin_at_trigger: None,
    }
}

fn path_short_fixed_margin(
    levels: &[GridLevel],
    upper: Decimal,
    margin: Decimal,
    mmr: Decimal,
) -> PathLiqResult {
    let mut sells: Vec<&GridLevel> = levels.iter().filter(|l| l.side == Side::Sell).collect();
    sells.sort_by(|a, b| a.price.cmp(&b.price));
    let mut qty = Decimal::ZERO;
    let mut cost = Decimal::ZERO;
    for (i, l) in sells.iter().enumerate() {
        qty += l.size;
        cost += l.price * l.size;
        let entry = cost / qty;
        let Some(liq) = liq_price_short(margin, qty, entry, mmr) else {
            continue;
        };
        let next_mark = sells.get(i + 1).map(|n| n.price).unwrap_or(upper);
        if liq >= l.price && liq <= next_mark {
            return PathLiqResult {
                risk: liq < upper,
                trigger_liq: Some(liq),
                margin_at_trigger: Some(margin),
            };
        }
    }
    PathLiqResult {
        risk: false,
        trigger_liq: None,
        margin_at_trigger: None,
    }
}

/// Worst loss for one direction: liquidation wipe if it triggers, else floating loss at the bound.
fn side_max_loss(
    float_upnl: Option<Decimal>,
    path: &PathLiqResult,
) -> (Decimal, &'static str) {
    if path.risk {
        if let Some(im) = path.margin_at_trigger {
            // Isolated/cross liquidation typically wipes the margin locked for that position.
            return (im.max(Decimal::ZERO), "liq");
        }
    }
    let loss = float_upnl
        .map(|u| (-u).max(Decimal::ZERO))
        .unwrap_or(Decimal::ZERO);
    (loss, "float")
}

pub fn preview_grid(config: &GridConfig, mid_price: Decimal) -> GridResult<GridPreview> {
    preview_grid_with_options(config, mid_price, None, None)
}

/// Preview with optional account equity and exchange max leverage (for MMR).
pub fn preview_grid_with_equity(
    config: &GridConfig,
    mid_price: Decimal,
    account_equity: Option<Decimal>,
) -> GridResult<GridPreview> {
    preview_grid_with_options(config, mid_price, account_equity, None)
}

pub fn preview_grid_with_options(
    config: &GridConfig,
    mid_price: Decimal,
    account_equity: Option<Decimal>,
    max_leverage: Option<u32>,
) -> GridResult<GridPreview> {
    config.validate()?;
    let levels = generate_levels(config, mid_price)?;
    let size_per_level = config.size_per_level()?;
    let buy_count = levels.iter().filter(|l| l.side == Side::Buy).count();
    let sell_count = levels.iter().filter(|l| l.side == Side::Sell).count();
    let estimated_quote_needed: Decimal = levels
        .iter()
        .filter(|l| l.side == Side::Buy)
        .map(|l| l.price * l.size)
        .fold(Decimal::ZERO, |a, b| a + b);
    let estimated_base_needed: Decimal = levels
        .iter()
        .filter(|l| l.side == Side::Sell)
        .map(|l| l.size)
        .fold(Decimal::ZERO, |a, b| a + b);

    let leverage = config.leverage.max(1);
    let lev = Decimal::from(leverage);
    // Prefer exchange max leverage; fall back to user leverage (more conservative MMR).
    let max_lev = max_leverage.unwrap_or(leverage).max(leverage).max(1);
    let mmr = mmr_from_max_leverage(max_lev);

    let long_ex = side_extreme(&levels, Side::Buy, config.lower_price);
    let short_ex = side_extreme(&levels, Side::Sell, config.upper_price);

    let max_side_notional = long_ex
        .map(|e| e.entry_notional)
        .unwrap_or(Decimal::ZERO)
        .max(
            short_ex
                .map(|e| e.entry_notional)
                .unwrap_or(Decimal::ZERO),
        );
    let estimated_margin = (max_side_notional / lev).round_dp(4);

    let long_iso = path_long_isolated(&levels, config.lower_price, lev, mmr);
    let short_iso = path_short_isolated(&levels, config.upper_price, lev, mmr);
    let isolated_liquidation_risk = long_iso.risk || short_iso.risk;

    let long_cross_budget =
        path_long_fixed_margin(&levels, config.lower_price, estimated_margin, mmr);
    let short_cross_budget =
        path_short_fixed_margin(&levels, config.upper_price, estimated_margin, mmr);
    let cross_liq_risk_on_strategy_margin = long_cross_budget.risk || short_cross_budget.risk;

    let cross_liquidation_risk = account_equity.map(|eq| {
        let long = path_long_fixed_margin(&levels, config.lower_price, eq, mmr);
        let short = path_short_fixed_margin(&levels, config.upper_price, eq, mmr);
        long.risk || short.risk
    });

    // Max loss must include liquidation wipe when the walk triggers in-range.
    // Isolated: use growing-IM path. Cross (preview): use strategy-margin / equity path.
    let (long_path, short_path) = if config.is_cross {
        if let Some(eq) = account_equity {
            (
                path_long_fixed_margin(&levels, config.lower_price, eq, mmr),
                path_short_fixed_margin(&levels, config.upper_price, eq, mmr),
            )
        } else {
            (long_cross_budget, short_cross_budget)
        }
    } else {
        (long_iso, short_iso)
    };

    let (long_loss, long_kind) = side_max_loss(long_ex.map(|e| e.upnl), &long_path);
    let (short_loss, short_kind) = side_max_loss(short_ex.map(|e| e.upnl), &short_path);
    let (max_loss_in_range, max_loss_at) = if long_loss >= short_loss {
        let at = if long_kind == "liq" { "long_liq" } else { "lower" };
        (long_loss, at)
    } else {
        let at = if short_kind == "liq" {
            "short_liq"
        } else {
            "upper"
        };
        (short_loss, at)
    };
    let max_loss_in_range = max_loss_in_range.round_dp(4);

    // Equity / margin ratio at worst bound still useful as a soft diagnostic (survive-to-bound).
    let worst_float = long_ex
        .map(|e| e.upnl)
        .unwrap_or(Decimal::ZERO)
        .min(short_ex.map(|e| e.upnl).unwrap_or(Decimal::ZERO));
    let worst_equity_isolated = (estimated_margin + worst_float).round_dp(4);
    let worst_mark = if long_ex.map(|e| e.upnl).unwrap_or(Decimal::ZERO)
        <= short_ex.map(|e| e.upnl).unwrap_or(Decimal::ZERO)
    {
        long_ex.map(|e| e.mark_notional).unwrap_or(Decimal::ZERO)
    } else {
        short_ex.map(|e| e.mark_notional).unwrap_or(Decimal::ZERO)
    };
    let worst_margin_ratio_pct = if worst_mark > Decimal::ZERO {
        ((estimated_margin + worst_float) / worst_mark * Decimal::from(100)).round_dp(2)
    } else {
        Decimal::ZERO
    };

    Ok(GridPreview {
        estimated_quote_needed,
        estimated_base_needed,
        levels,
        buy_count,
        sell_count,
        size_per_level,
        max_loss_in_range,
        max_loss_at: max_loss_at.into(),
        estimated_margin,
        worst_equity_isolated,
        worst_margin_ratio_pct,
        isolated_liquidation_risk,
        cross_liq_risk_on_strategy_margin,
        cross_liquidation_risk,
        estimated_long_liq_price: long_iso.trigger_liq.map(|p| p.round_dp(8)),
        estimated_short_liq_price: short_iso.trigger_liq.map(|p| p.round_dp(8)),
        leverage,
        is_cross: config.is_cross,
        assumed_mmr: mmr,
        max_leverage: max_lev,
    })
}

pub fn new_order_id() -> String {
    Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn cfg(leverage: u32) -> GridConfig {
        GridConfig {
            symbol: "BTC".into(),
            lower_price: dec!(90),
            upper_price: dec!(110),
            grid_count: 40,
            total_budget: dec!(1000),
            spacing: GridSpacing::Arithmetic,
            breakout_action: BreakoutAction::Pause,
            max_drawdown_pct: Decimal::ZERO,
            max_daily_loss: Decimal::ZERO,
            max_order_failures: 5,
            market: MarketKind::Perp,
            leverage,
            is_cross: false,
        grid_mode: GridMode::Fixed,
        dynamic: DynamicGridConfig::default(),
        }
    }

    fn cxmt_cfg() -> GridConfig {
        GridConfig {
            symbol: "xyz:CXMT".into(),
            lower_price: dec!(6.3972),
            upper_price: dec!(7.8983),
            grid_count: 40,
            total_budget: dec!(2000),
            spacing: GridSpacing::Arithmetic,
            breakout_action: BreakoutAction::CancelCloseAndStop,
            max_drawdown_pct: Decimal::ZERO,
            max_daily_loss: dec!(100),
            max_order_failures: 5,
            market: MarketKind::Perp,
            leverage: 10,
            is_cross: false,
        grid_mode: GridMode::Fixed,
        dynamic: DynamicGridConfig::default(),
        }
    }

    #[test]
    fn mmr_matches_hyperliquid_half_im_at_max_lev() {
        assert_eq!(mmr_from_max_leverage(50), dec!(0.01));
        assert_eq!(mmr_from_max_leverage(10), dec!(0.05));
        assert_eq!(mmr_from_max_leverage(20), dec!(0.025));
    }

    #[test]
    fn preview_keeps_float_max_loss_when_no_liq() {
        // 2x ±10%: no walk liquidation → max loss is floating PnL at bound (~25).
        let p = preview_grid_with_options(&cfg(2), dec!(100), None, Some(50)).unwrap();
        assert!(!p.isolated_liquidation_risk);
        assert!(p.max_loss_in_range > dec!(20));
        assert!(p.max_loss_in_range < dec!(30));
        assert!(p.max_loss_at == "lower" || p.max_loss_at == "upper");
        assert!(p.estimated_margin >= dec!(249) && p.estimated_margin <= dec!(251));
    }

    #[test]
    fn max_loss_uses_liq_wipe_when_isolated_triggers() {
        let cfg = GridConfig {
            symbol: "TEST".into(),
            lower_price: dec!(903.83),
            upper_price: dec!(1104.68),
            grid_count: 40,
            total_budget: dec!(2000),
            spacing: GridSpacing::Arithmetic,
            breakout_action: BreakoutAction::Pause,
            max_drawdown_pct: Decimal::ZERO,
            max_daily_loss: Decimal::ZERO,
            max_order_failures: 5,
            market: MarketKind::Perp,
            leverage: 10,
            is_cross: false,
        grid_mode: GridMode::Fixed,
        dynamic: DynamicGridConfig::default(),
        };
        let mid = (cfg.lower_price + cfg.upper_price) / Decimal::from(2);
        let p = preview_grid_with_options(&cfg, mid, None, Some(10)).unwrap();
        assert!(p.isolated_liquidation_risk);
        // Float-at-bound was ~50; true wipe at trigger IM ≈ 95.
        assert!(p.max_loss_in_range >= dec!(90));
        assert!(p.max_loss_in_range <= dec!(100));
        assert!(p.max_loss_at == "long_liq" || p.max_loss_at == "short_liq");
    }

    #[test]
    fn ten_x_with_matching_max_lev_has_walk_liq_risk() {
        // ±10% at 10x when asset max leverage is also 10 (MMR 5%): walk hits liq in range.
        let p = preview_grid_with_options(&cfg(10), dec!(100), None, Some(10)).unwrap();
        assert_eq!(p.assumed_mmr, dec!(0.05));
        assert!(p.isolated_liquidation_risk);
        assert!(
            p.estimated_long_liq_price.is_some() || p.estimated_short_liq_price.is_some()
        );
    }

    #[test]
    fn ten_x_with_high_max_lev_may_survive_smooth_walk() {
        // Same ±10%/10x but maxLev=50 → MMR 1%; smooth fill path may not trigger.
        let p = preview_grid_with_options(&cfg(10), dec!(100), None, Some(50)).unwrap();
        assert_eq!(p.assumed_mmr, dec!(0.01));
        assert!(!p.isolated_liquidation_risk);
    }

    #[test]
    fn high_leverage_flags_liq() {
        let p = preview_grid(&cfg(50), dec!(100)).unwrap();
        assert!(p.isolated_liquidation_risk);
    }

    #[test]
    fn cxmt_style_isolated_flags_liq_in_range() {
        let p = preview_grid_with_options(&cxmt_cfg(), dec!(7.476), None, Some(10)).unwrap();
        assert_eq!(p.assumed_mmr, dec!(0.05));
        assert!(p.isolated_liquidation_risk);
        // Mid near upper → more buys; long walk triggers inside the range.
        let long_liq = p.estimated_long_liq_price.expect("long trigger");
        assert!(long_liq > cxmt_cfg().lower_price);
        assert!(long_liq < dec!(7.476));
    }

    #[test]
    fn short_trigger_is_walk_hit_not_first_fill_liq() {
        let cfg = GridConfig {
            symbol: "TEST".into(),
            lower_price: dec!(903.83),
            upper_price: dec!(1104.68),
            grid_count: 40,
            total_budget: dec!(2000),
            spacing: GridSpacing::Arithmetic,
            breakout_action: BreakoutAction::Pause,
            max_drawdown_pct: Decimal::ZERO,
            max_daily_loss: Decimal::ZERO,
            max_order_failures: 5,
            market: MarketKind::Perp,
            leverage: 10,
            is_cross: false,
        grid_mode: GridMode::Fixed,
        dynamic: DynamicGridConfig::default(),
        };
        let mid = (cfg.lower_price + cfg.upper_price) / Decimal::from(2);
        let p = preview_grid_with_options(&cfg, mid, None, Some(10)).unwrap();
        assert!(p.isolated_liquidation_risk);
        let short_liq = p.estimated_short_liq_price.expect("short trigger");
        // First-fill theoretical liq ≈ 1054; walk trigger should be much higher (~1102).
        assert!(short_liq > dec!(1090));
        assert!(short_liq < cfg.upper_price);
    }

    #[test]
    fn cross_large_equity_can_clear_risk() {
        // Huge account equity as fixed margin pushes liq outside a modest range.
        let p = preview_grid_with_options(&cfg(10), dec!(100), Some(dec!(10000)), Some(50)).unwrap();
        assert_eq!(p.cross_liquidation_risk, Some(false));
    }

    #[test]
    fn cross_tiny_equity_flags_risk() {
        let p = preview_grid_with_options(&cfg(10), dec!(100), Some(dec!(5)), Some(50)).unwrap();
        assert_eq!(p.cross_liquidation_risk, Some(true));
    }

    #[test]
    fn low_leverage_wide_range_can_be_safe() {
        // 2x in ±10%: liq distance ≈ 49%, outside the range.
        let p = preview_grid_with_options(&cfg(2), dec!(100), None, Some(50)).unwrap();
        assert!(!p.isolated_liquidation_risk);
    }
}

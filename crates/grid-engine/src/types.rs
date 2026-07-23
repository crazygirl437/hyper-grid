use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::{GridError, GridResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn opposite(self) -> Self {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GridSpacing {
    Arithmetic,
    Geometric,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    Simulation,
    Testnet,
    Mainnet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BreakoutAction {
    AlertOnly,
    Pause,
    CancelAndPause,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotStatus {
    Idle,
    Running,
    Paused,
    Halted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketKind {
    /// Hyperliquid perpetual (default). Can open long/short without base inventory.
    Perp,
    /// Spot (legacy). Buy-first inventory style.
    Spot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GridConfig {
    pub symbol: String,
    pub lower_price: Decimal,
    pub upper_price: Decimal,
    pub grid_count: u32,
    pub total_budget: Decimal,
    pub spacing: GridSpacing,
    pub breakout_action: BreakoutAction,
    pub max_drawdown_pct: Decimal,
    pub max_daily_loss: Decimal,
    pub max_order_failures: u32,
    #[serde(default = "default_market_kind")]
    pub market: MarketKind,
    /// Perp leverage (1–50). Ignored for spot.
    #[serde(default = "default_leverage")]
    pub leverage: u32,
    /// Cross margin when true; isolated when false.
    #[serde(default = "default_cross")]
    pub is_cross: bool,
}

fn default_market_kind() -> MarketKind {
    MarketKind::Perp
}
fn default_leverage() -> u32 {
    5
}
fn default_cross() -> bool {
    true
}

impl GridConfig {
    pub fn validate(&self) -> GridResult<()> {
        if self.symbol.trim().is_empty() {
            return Err(GridError::InvalidConfig("symbol is required".into()));
        }
        if self.lower_price <= Decimal::ZERO || self.upper_price <= Decimal::ZERO {
            return Err(GridError::InvalidConfig("prices must be positive".into()));
        }
        if self.lower_price >= self.upper_price {
            return Err(GridError::InvalidConfig(
                "lower_price must be < upper_price".into(),
            ));
        }
        if self.grid_count < 2 {
            return Err(GridError::InvalidConfig(
                "grid_count must be at least 2".into(),
            ));
        }
        if self.total_budget <= Decimal::ZERO {
            return Err(GridError::InvalidConfig(
                "total_budget must be positive".into(),
            ));
        }
        if matches!(self.market, MarketKind::Perp) && !(1..=50).contains(&self.leverage) {
            return Err(GridError::InvalidConfig(
                "leverage must be between 1 and 50".into(),
            ));
        }
        // Hyperliquid rejects orders under ~$10 notional.
        let per_level = self.total_budget / Decimal::from(self.grid_count);
        if per_level < Decimal::from(10) {
            return Err(GridError::InvalidConfig(format!(
                "每格名义约 {per_level} USDC，低于交易所最低约 $10。请提高总投入或减少网格数量。"
            )));
        }
        Ok(())
    }

    pub fn size_per_level(&self) -> GridResult<Decimal> {
        self.validate()?;
        Ok(self.total_budget / Decimal::from(self.grid_count))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GridLevel {
    pub index: u32,
    pub price: Decimal,
    pub side: Side,
    pub size: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderIntent {
    pub client_id: String,
    pub symbol: String,
    pub side: Side,
    pub price: Decimal,
    pub size: Decimal,
    pub level_index: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveOrder {
    pub client_id: String,
    pub exchange_id: Option<String>,
    pub symbol: String,
    pub side: Side,
    pub price: Decimal,
    /// Remaining size (base coin).
    pub size: Decimal,
    /// Original size when placed (base coin). Used for correct replenish after partials.
    #[serde(default)]
    pub orig_size: Decimal,
    pub level_index: u32,
}

impl LiveOrder {
    pub fn new(
        client_id: String,
        exchange_id: Option<String>,
        symbol: String,
        side: Side,
        price: Decimal,
        size: Decimal,
        level_index: u32,
    ) -> Self {
        Self {
            client_id,
            exchange_id,
            symbol,
            side,
            price,
            size,
            orig_size: size,
            level_index,
        }
    }

    pub fn level_size(&self) -> Decimal {
        if self.orig_size > Decimal::ZERO {
            self.orig_size
        } else {
            self.size
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FillEvent {
    pub client_id: String,
    pub symbol: String,
    pub side: Side,
    pub price: Decimal,
    pub size: Decimal,
    pub level_index: u32,
    pub fee: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestingOrderView {
    pub side: Side,
    pub price: Decimal,
    pub size: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotSnapshot {
    pub status: BotStatus,
    pub mode: RunMode,
    pub symbol: String,
    pub mid_price: Option<Decimal>,
    pub open_orders: usize,
    /// Live resting orders for chart price lines.
    #[serde(default)]
    pub resting_orders: Vec<RestingOrderView>,
    /// Net position in base coin. Perp: long > 0, short < 0. Spot: long-only ≥ 0.
    pub position_base: Decimal,
    /// Average entry price of current long inventory, if any.
    pub avg_entry_price: Option<Decimal>,
    pub realized_pnl: Decimal,
    /// Mark-to-mid unrealized PnL on open position.
    pub unrealized_pnl: Decimal,
    pub events_tail: Vec<String>,
}

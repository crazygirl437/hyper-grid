use rust_decimal::Decimal;
use rust_decimal_macros::dec;
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
    /// Cancel this strategy's symbol orders, close its position, and require a fresh start.
    CancelCloseAndStop,
    /// Soft-confirm breakout then recenter grid while retaining position.
    Recenter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotStatus {
    Idle,
    Running,
    Paused,
    SoftBreakout,
    Recentering,
    Recovering,
    ProtectiveExit,
    BreakoutStopped,
    /// Local loop detached; exchange orders/position retained for resume.
    Detached,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GridMode {
    Fixed,
    #[default]
    Dynamic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TimeInForce {
    #[default]
    Gtc,
    Ioc,
    Alo,
}

/// Runtime parameters for ATR-driven dynamic grids.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicGridConfig {
    /// Candle interval string, e.g. "15m" / "1h".
    #[serde(default = "default_atr_interval")]
    pub atr_interval: String,
    #[serde(default = "default_atr_period")]
    pub atr_period: u32,
    /// Half-width = ATR% × this multiplier (clamped by min/max).
    #[serde(default = "default_atr_mult")]
    pub atr_mult: Decimal,
    #[serde(default = "default_min_half_width")]
    pub min_half_width_pct: Decimal,
    #[serde(default = "default_max_half_width")]
    pub max_half_width_pct: Decimal,
    /// Consecutive closed candles outside bounds before hard recenter.
    #[serde(default = "default_confirm_bars")]
    pub confirm_bars: u32,
    /// Minimum seconds between recenters.
    #[serde(default = "default_cooldown_secs")]
    pub recenter_cooldown_secs: u64,
    #[serde(default = "default_max_recenters_day")]
    pub max_recenters_per_day: u32,
    /// Hysteresis percent of band width for re-entry after soft breakout.
    #[serde(default = "default_hysteresis")]
    pub reentry_hysteresis_pct: Decimal,
}

fn default_atr_interval() -> String {
    "1h".into()
}
fn default_atr_period() -> u32 {
    14
}
fn default_atr_mult() -> Decimal {
    dec!(5)
}
fn default_min_half_width() -> Decimal {
    dec!(2)
}
fn default_max_half_width() -> Decimal {
    dec!(12)
}
fn default_confirm_bars() -> u32 {
    2
}
fn default_cooldown_secs() -> u64 {
    3600
}
fn default_max_recenters_day() -> u32 {
    4
}
fn default_hysteresis() -> Decimal {
    dec!(10)
}

impl Default for DynamicGridConfig {
    fn default() -> Self {
        Self {
            atr_interval: default_atr_interval(),
            atr_period: default_atr_period(),
            atr_mult: default_atr_mult(),
            min_half_width_pct: default_min_half_width(),
            max_half_width_pct: default_max_half_width(),
            confirm_bars: default_confirm_bars(),
            recenter_cooldown_secs: default_cooldown_secs(),
            max_recenters_per_day: default_max_recenters_day(),
            reentry_hysteresis_pct: default_hysteresis(),
        }
    }
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
    #[serde(default)]
    pub grid_mode: GridMode,
    #[serde(default)]
    pub dynamic: DynamicGridConfig,
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
        if matches!(self.grid_mode, GridMode::Dynamic) {
            let d = &self.dynamic;
            if d.atr_period < 2 {
                return Err(GridError::InvalidConfig(
                    "atr_period must be at least 2".into(),
                ));
            }
            if d.min_half_width_pct <= Decimal::ZERO
                || d.max_half_width_pct <= d.min_half_width_pct
            {
                return Err(GridError::InvalidConfig(
                    "invalid dynamic half-width bounds".into(),
                ));
            }
        }
        Ok(())
    }

    pub fn size_per_level(&self) -> GridResult<Decimal> {
        self.validate()?;
        Ok(self.total_budget / Decimal::from(self.grid_count))
    }

    pub fn is_dynamic(&self) -> bool {
        matches!(self.grid_mode, GridMode::Dynamic)
            || matches!(self.breakout_action, BreakoutAction::Recenter)
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
    #[serde(default)]
    pub reduce_only: bool,
    #[serde(default)]
    pub tif: TimeInForce,
    /// Stable exchange client order id (Hyperliquid cloid), when set.
    #[serde(default)]
    pub cloid: Option<String>,
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
    #[serde(default)]
    pub reduce_only: bool,
    #[serde(default)]
    pub cloid: Option<String>,
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
            reduce_only: false,
            cloid: None,
        }
    }

    pub fn from_intent(intent: &OrderIntent, exchange_id: Option<String>) -> Self {
        Self {
            client_id: intent.client_id.clone(),
            exchange_id,
            symbol: intent.symbol.clone(),
            side: intent.side,
            price: intent.price,
            size: intent.size,
            orig_size: intent.size,
            level_index: intent.level_index,
            reduce_only: intent.reduce_only,
            cloid: intent.cloid.clone(),
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
    #[serde(default)]
    pub fee_token: Option<String>,
    /// Exchange trade id when known (Hyperliquid tid).
    #[serde(default)]
    pub exchange_tid: Option<String>,
    #[serde(default)]
    pub exchange_oid: Option<String>,
    #[serde(default)]
    pub cloid: Option<String>,
    /// Exchange fill time in unix ms.
    #[serde(default)]
    pub exchange_time_ms: Option<i64>,
    #[serde(default)]
    pub crossed: bool,
    #[serde(default)]
    pub dir: Option<String>,
    #[serde(default)]
    pub closed_pnl: Option<Decimal>,
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
    #[serde(default)]
    pub status_note: Option<String>,
    pub mode: RunMode,
    pub symbol: String,
    pub mid_price: Option<Decimal>,
    pub open_orders: usize,
    /// Fills processed during this strategy session.
    #[serde(default)]
    pub fill_count: usize,
    /// Live resting orders for chart price lines.
    #[serde(default)]
    pub resting_orders: Vec<RestingOrderView>,
    /// Net position in base coin. Perp: long > 0, short < 0. Spot: long-only ≥ 0.
    pub position_base: Decimal,
    /// Average entry price of current long inventory, if any.
    pub avg_entry_price: Option<Decimal>,
    /// Exchange-reported liquidation price when available (perps).
    #[serde(default)]
    pub liquidation_price: Option<Decimal>,
    pub realized_pnl: Decimal,
    /// Mark-to-mid unrealized PnL on open position.
    pub unrealized_pnl: Decimal,
    /// Net funding cash flow for this strategy session (negative paid, positive received).
    #[serde(default)]
    pub funding_pnl: Decimal,
    pub events_tail: Vec<String>,
    /// Active (possibly recentered) lower bound.
    #[serde(default)]
    pub active_lower: Option<Decimal>,
    #[serde(default)]
    pub active_upper: Option<Decimal>,
    #[serde(default)]
    pub atr: Option<Decimal>,
    #[serde(default)]
    pub atr_pct: Option<Decimal>,
    #[serde(default)]
    pub recenter_generation: u32,
    #[serde(default)]
    pub recenters_today: u32,
    #[serde(default)]
    pub last_recenter_ms: Option<i64>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub last_tick_ms: Option<i64>,
    #[serde(default)]
    pub health_note: Option<String>,
    #[serde(default)]
    pub grid_mode: GridMode,
}

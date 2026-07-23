use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

mod engine;
mod levels;
mod risk;
mod types;

pub use engine::{EngineEvent, GridEngine};
pub use levels::generate_levels;
pub use risk::{RiskConfig, RiskState};
pub use types::*;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GridPreview {
    pub levels: Vec<GridLevel>,
    pub buy_count: usize,
    pub sell_count: usize,
    pub size_per_level: Decimal,
    pub estimated_quote_needed: Decimal,
    pub estimated_base_needed: Decimal,
}

pub fn preview_grid(config: &GridConfig, mid_price: Decimal) -> GridResult<GridPreview> {
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
    Ok(GridPreview {
        estimated_quote_needed,
        estimated_base_needed,
        levels,
        buy_count,
        sell_count,
        size_per_level,
    })
}

pub fn new_order_id() -> String {
    Uuid::new_v4().to_string()
}

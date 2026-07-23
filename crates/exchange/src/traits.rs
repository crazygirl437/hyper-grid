use async_trait::async_trait;
use grid_engine::{FillEvent, LiveOrder, OrderIntent, RunMode};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExchangeError {
    #[error("not connected")]
    NotConnected,
    #[error("insufficient balance: {0}")]
    InsufficientBalance(String),
    #[error("api error: {0}")]
    Api(String),
    #[error("invalid key: {0}")]
    InvalidKey(String),
    #[error("{0}")]
    Other(String),
}

pub type ExchangeResult<T> = Result<T, ExchangeError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Balance {
    /// Coin / symbol code, e.g. USDC, BTC. For account mode rows use the mode id.
    pub asset: String,
    pub total: Decimal,
    pub available: Decimal,
    /// Display category for i18n: unified | spot | perp | position | mode | sim
    #[serde(default = "default_balance_kind")]
    pub kind: String,
}

fn default_balance_kind() -> String {
    "spot".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticker {
    pub symbol: String,
    pub mid: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketInfo {
    /// Key used for mid lookup / orders (e.g. BTC, PURR/USDC, @1)
    pub symbol: String,
    /// Human label shown in UI
    pub label: String,
    /// "perp" or "spot"
    pub kind: String,
    pub mid: Decimal,
    /// Exchange min leverage (usually 1 for perps).
    #[serde(default = "default_min_leverage")]
    pub min_leverage: u32,
    /// Exchange max leverage for this coin (from meta.maxLeverage).
    #[serde(default = "default_max_leverage")]
    pub max_leverage: u32,
    /// If true, only isolated margin is allowed.
    #[serde(default)]
    pub only_isolated: bool,
}

fn default_min_leverage() -> u32 {
    1
}
fn default_max_leverage() -> u32 {
    50
}

#[async_trait]
pub trait Exchange: Send + Sync {
    fn mode(&self) -> RunMode;

    async fn connect(&mut self) -> ExchangeResult<()>;

    async fn get_mid(&self, symbol: &str) -> ExchangeResult<Decimal>;

    async fn get_balances(&self) -> ExchangeResult<Vec<Balance>>;

    async fn place_order(&mut self, intent: OrderIntent) -> ExchangeResult<LiveOrder>;

    /// Place many orders. Default: sequential. Exchanges may override with a true batch.
    async fn place_orders(&mut self, intents: Vec<OrderIntent>) -> ExchangeResult<Vec<LiveOrder>> {
        let mut out = Vec::with_capacity(intents.len());
        for intent in intents {
            out.push(self.place_order(intent).await?);
        }
        Ok(out)
    }

    async fn cancel_order(&mut self, client_id: &str) -> ExchangeResult<()>;

    async fn cancel_all(&mut self, symbol: &str) -> ExchangeResult<()>;

    /// Cancel all open orders and close all positions (account flatten).
    async fn flatten(&mut self) -> ExchangeResult<()> {
        self.cancel_all("").await?;
        Ok(())
    }

    async fn drain_fills(&mut self) -> ExchangeResult<Vec<FillEvent>>;

    async fn list_open_orders(&self, symbol: &str) -> ExchangeResult<Vec<LiveOrder>>;

    async fn list_spot_symbols(&self) -> ExchangeResult<Vec<String>>;

    async fn list_markets(&self) -> ExchangeResult<Vec<MarketInfo>> {
        let syms = self.list_spot_symbols().await?;
        let mut out = Vec::new();
        for symbol in syms {
            let mid = self.get_mid(&symbol).await.unwrap_or(Decimal::ZERO);
            out.push(MarketInfo {
                label: symbol.clone(),
                kind: "unknown".into(),
                symbol,
                mid,
                min_leverage: 1,
                max_leverage: 50,
                only_isolated: false,
            });
        }
        Ok(out)
    }
}

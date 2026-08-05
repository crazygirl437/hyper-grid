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
    /// Current perpetual funding rate per funding interval (typically hourly).
    #[serde(default)]
    pub funding_rate: Option<Decimal>,
    /// 24h notional volume in quote (USDC), when available from asset contexts.
    #[serde(default)]
    pub day_ntl_vlm: Option<Decimal>,
    /// Previous day mid/mark price, used for 24h change estimates.
    #[serde(default)]
    pub prev_day_px: Option<Decimal>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionSnapshot {
    pub symbol: String,
    pub size: Decimal,
    pub entry_price: Option<Decimal>,
    pub unrealized_pnl: Option<Decimal>,
    pub liquidation_price: Option<Decimal>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CancelReport {
    pub canceled: usize,
    pub remaining_oids: Vec<String>,
    pub confirmed_flat: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconcileReport {
    pub exchange_orders: Vec<LiveOrder>,
    pub local_only: Vec<String>,
    pub exchange_only_oids: Vec<String>,
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

    /// Cancel symbol orders and poll until exchange reports none remaining.
    async fn cancel_all_confirmed(
        &mut self,
        symbol: &str,
        max_attempts: u32,
    ) -> ExchangeResult<CancelReport> {
        self.cancel_all(symbol).await?;
        let mut remaining = Vec::new();
        for attempt in 0..max_attempts.max(1) {
            let open = self.list_exchange_open_orders(symbol).await?;
            if open.is_empty() {
                return Ok(CancelReport {
                    canceled: 0,
                    remaining_oids: vec![],
                    confirmed_flat: true,
                });
            }
            remaining = open
                .iter()
                .filter_map(|o| o.exchange_id.clone())
                .collect();
            if attempt + 1 < max_attempts {
                self.cancel_all(symbol).await?;
                tokio::time::sleep(std::time::Duration::from_millis(
                    200 * (attempt as u64 + 1),
                ))
                .await;
            }
        }
        Ok(CancelReport {
            canceled: 0,
            remaining_oids: remaining,
            confirmed_flat: false,
        })
    }

    /// Close only the net position for `symbol`, leaving other markets untouched.
    async fn close_position(&mut self, symbol: &str) -> ExchangeResult<()>;

    /// Cancel all open orders and close all positions (account flatten).
    async fn flatten(&mut self) -> ExchangeResult<()> {
        self.cancel_all("").await?;
        Ok(())
    }

    async fn drain_fills(&mut self) -> ExchangeResult<Vec<FillEvent>>;

    async fn list_open_orders(&self, symbol: &str) -> ExchangeResult<Vec<LiveOrder>>;

    /// Live open orders from the exchange (not just local map).
    async fn list_exchange_open_orders(&self, symbol: &str) -> ExchangeResult<Vec<LiveOrder>> {
        self.list_open_orders(symbol).await
    }

    async fn get_position(&self, symbol: &str) -> ExchangeResult<PositionSnapshot> {
        Ok(PositionSnapshot {
            symbol: symbol.to_string(),
            size: Decimal::ZERO,
            entry_price: None,
            unrealized_pnl: None,
            liquidation_price: None,
        })
    }

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
                funding_rate: None,
                day_ntl_vlm: None,
                prev_day_px: None,
                min_leverage: 1,
                max_leverage: 50,
                only_isolated: false,
            });
        }
        Ok(out)
    }
}

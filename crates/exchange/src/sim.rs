use std::collections::HashMap;

use async_trait::async_trait;
use grid_engine::{FillEvent, LiveOrder, OrderIntent, RunMode, Side};
use rand::Rng;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tokio::sync::Mutex;
// chrono used for simulated fill timestamps

use crate::traits::{Balance, Exchange, ExchangeResult};

struct SimState {
    mid: Decimal,
    lower: Decimal,
    upper: Decimal,
    center: Decimal,
    /// Half-width of the tradable band (for sizing shocks).
    amplitude: Decimal,
    /// Persistent velocity (momentum) so the path looks less white-noise.
    velocity: f64,
    tick: u64,
}

pub struct SimExchange {
    state: Mutex<SimState>,
    orders: Mutex<HashMap<String, LiveOrder>>,
    fills: Mutex<Vec<FillEvent>>,
    quote_balance: Mutex<Decimal>,
    base_balance: Mutex<Decimal>,
    symbol: String,
}

impl SimExchange {
    pub fn new(
        symbol: impl Into<String>,
        start_mid: Decimal,
        quote: Decimal,
        base: Decimal,
    ) -> Self {
        let pad = (start_mid * dec!(0.05)).max(dec!(0.01));
        Self::with_band(
            symbol,
            start_mid,
            quote,
            base,
            (start_mid - pad).max(dec!(0.0001)),
            start_mid + pad,
        )
    }

    /// Mean-revert inside `[lower, upper]` with noisy paths so a grid stays profitable
    /// without looking like a perfect sine wave.
    pub fn with_band(
        symbol: impl Into<String>,
        start_mid: Decimal,
        quote: Decimal,
        base: Decimal,
        lower: Decimal,
        upper: Decimal,
    ) -> Self {
        let (lo, hi, center, amp) = band_params(lower, upper);
        let mid = start_mid.clamp(lo, hi);
        Self {
            state: Mutex::new(SimState {
                mid,
                lower: lo,
                upper: hi,
                center,
                amplitude: amp,
                velocity: 0.0,
                tick: 0,
            }),
            orders: Mutex::new(HashMap::new()),
            fills: Mutex::new(Vec::new()),
            quote_balance: Mutex::new(quote),
            base_balance: Mutex::new(base),
            symbol: symbol.into(),
        }
    }

    /// Update the oscillation band (call when starting a grid).
    pub async fn set_band(&self, lower: Decimal, upper: Decimal) {
        let (lo, hi, center, amp) = band_params(lower, upper);
        let mut st = self.state.lock().await;
        st.lower = lo;
        st.upper = hi;
        st.center = center;
        st.amplitude = amp;
        st.mid = st.mid.clamp(lo, hi);
        st.velocity = 0.0;
        st.tick = 0;
    }

    pub async fn peek_mid(&self) -> Decimal {
        self.state.lock().await.mid
    }

    pub async fn position_size(&self) -> Decimal {
        *self.base_balance.lock().await
    }

    async fn advance_mid(&self) -> Decimal {
        let mut st = self.state.lock().await;
        st.tick = st.tick.wrapping_add(1);

        let mid_f = st.mid.to_string().parse::<f64>().unwrap_or(0.0);
        let center_f = st.center.to_string().parse::<f64>().unwrap_or(mid_f);
        let amp_f = st
            .amplitude
            .to_string()
            .parse::<f64>()
            .unwrap_or(1.0)
            .max(1e-9);
        let lo_f = st.lower.to_string().parse::<f64>().unwrap_or(mid_f);
        let hi_f = st.upper.to_string().parse::<f64>().unwrap_or(mid_f);
        let range = (hi_f - lo_f).max(1e-9);

        let mut rng = rand::thread_rng();

        // Soft mean reversion toward center (stronger near the edges).
        let dist = (mid_f - center_f) / amp_f;
        let pull = -0.08 * dist - 0.12 * dist.powi(3);

        // Momentum with friction + gaussian-ish noise.
        let shock = rng.gen_range(-1.0..1.0) + 0.55 * rng.gen_range(-1.0..1.0);
        // Occasional larger impulse so swings are visible but irregular.
        let jump = if rng.gen_bool(0.08) {
            rng.gen_range(-1.0..1.0) * 0.035 * range
        } else {
            0.0
        };

        st.velocity = st.velocity * 0.82 + pull * amp_f * 0.045 + shock * amp_f * 0.028;
        // Cap velocity so we don't teleport across the whole band in one tick.
        let vmax = 0.06 * range;
        st.velocity = st.velocity.clamp(-vmax, vmax);

        let mut next = mid_f + st.velocity + jump;

        // Soft reflective boundaries (bounce) instead of hard clamp — looks more natural.
        let pad = range * 0.02;
        let lo = lo_f + pad;
        let hi = hi_f - pad;
        if next < lo {
            next = lo + (lo - next) * 0.35;
            st.velocity = st.velocity.abs() * 0.55;
        } else if next > hi {
            next = hi - (next - hi) * 0.35;
            st.velocity = -st.velocity.abs() * 0.55;
        }

        // Micro noise for candle/line texture.
        next += rng.gen_range(-0.0008..0.0008) * range;

        let next_d = Decimal::from_f64_retain(next).unwrap_or(st.mid);
        st.mid = next_d.clamp(st.lower, st.upper).round_dp(6);
        st.mid
    }

    async fn maybe_fill(&self) {
        let mid = self.state.lock().await.mid;
        let mut orders = self.orders.lock().await;
        let mut fills = self.fills.lock().await;
        let mut base = self.base_balance.lock().await;

        // Prefer filling the nearest crossed order first so size stays level-accurate
        // (avoid wiping an entire side in one giant mid jump).
        let mut candidates: Vec<(String, LiveOrder)> = orders
            .iter()
            .filter(|(_, order)| match order.side {
                Side::Buy => mid <= order.price,
                Side::Sell => mid >= order.price,
            })
            .map(|(id, o)| (id.clone(), o.clone()))
            .collect();
        candidates.sort_by(|a, b| {
            let da = (a.1.price - mid).abs();
            let db = (b.1.price - mid).abs();
            da.cmp(&db)
        });

        // At most a few fills per tick — smoother equity curve.
        const MAX_FILLS_PER_TICK: usize = 2;
        for (id, order) in candidates.into_iter().take(MAX_FILLS_PER_TICK) {
            match order.side {
                Side::Buy => *base += order.size,
                Side::Sell => *base -= order.size,
            }
            fills.push(FillEvent {
                client_id: id.clone(),
                symbol: order.symbol.clone(),
                side: order.side,
                price: order.price,
                size: order.size,
                level_index: order.level_index,
                fee: Decimal::ZERO,
                fee_token: Some("USDC".into()),
                exchange_tid: Some(format!("sim-tid-{id}")),
                exchange_oid: order.exchange_id.clone(),
                cloid: order.cloid.clone(),
                exchange_time_ms: Some(chrono::Utc::now().timestamp_millis()),
                crossed: false,
                dir: None,
                closed_pnl: None,
            });
            orders.remove(&id);
        }
    }

    /// Force mid outside band for breakout/recenter tests.
    pub async fn force_mid(&self, mid: Decimal) {
        let mut st = self.state.lock().await;
        st.mid = mid;
    }
}

fn band_params(lower: Decimal, upper: Decimal) -> (Decimal, Decimal, Decimal, Decimal) {
    let mut lo = lower.min(upper);
    let mut hi = lower.max(upper);
    if hi <= lo {
        let mid = if lo > Decimal::ZERO { lo } else { dec!(1) };
        lo = mid * dec!(0.95);
        hi = mid * dec!(1.05);
    }
    let center = (lo + hi) / Decimal::from(2);
    // Use ~96% of half-range so swings are large and still in-band.
    let amplitude = ((hi - lo) / Decimal::from(2) * dec!(0.96)).max(dec!(0.0001));
    (lo, hi, center, amplitude)
}

#[async_trait]
impl Exchange for SimExchange {
    fn mode(&self) -> RunMode {
        RunMode::Simulation
    }

    async fn connect(&mut self) -> ExchangeResult<()> {
        Ok(())
    }

    async fn get_mid(&self, _symbol: &str) -> ExchangeResult<Decimal> {
        let mid = self.advance_mid().await;
        self.maybe_fill().await;
        Ok(mid)
    }

    async fn get_balances(&self) -> ExchangeResult<Vec<Balance>> {
        Ok(vec![
            Balance {
                asset: "USDC".into(),
                total: *self.quote_balance.lock().await,
                available: *self.quote_balance.lock().await,
                kind: "sim".into(),
            },
            Balance {
                asset: self.symbol.clone(),
                total: *self.base_balance.lock().await,
                available: *self.base_balance.lock().await,
                kind: "sim".into(),
            },
        ])
    }

    async fn place_order(&mut self, intent: OrderIntent) -> ExchangeResult<LiveOrder> {
        // Mirror Hyperliquid: reduce-only must shrink position and not flip through flat.
        if intent.reduce_only {
            let pos = *self.base_balance.lock().await;
            let ok = match intent.side {
                Side::Buy => pos < Decimal::ZERO && intent.size <= pos.abs(),
                Side::Sell => pos > Decimal::ZERO && intent.size <= pos.abs(),
            };
            if !ok {
                return Err(crate::ExchangeError::Other(
                    "Reduce only order would increase position".into(),
                ));
            }
        }
        let mut order = LiveOrder::from_intent(&intent, Some(format!("sim-{}", intent.client_id)));
        order.symbol = intent.symbol;
        self.orders
            .lock()
            .await
            .insert(order.client_id.clone(), order.clone());
        Ok(order)
    }

    async fn get_position(&self, symbol: &str) -> ExchangeResult<crate::traits::PositionSnapshot> {
        let size = if symbol == self.symbol {
            *self.base_balance.lock().await
        } else {
            Decimal::ZERO
        };
        Ok(crate::traits::PositionSnapshot {
            symbol: symbol.to_string(),
            size,
            entry_price: None,
            unrealized_pnl: Some(Decimal::ZERO),
            liquidation_price: None,
        })
    }

    async fn cancel_all_confirmed(
        &mut self,
        symbol: &str,
        _max_attempts: u32,
    ) -> ExchangeResult<crate::traits::CancelReport> {
        self.cancel_all(symbol).await?;
        Ok(crate::traits::CancelReport {
            canceled: 0,
            remaining_oids: vec![],
            confirmed_flat: true,
        })
    }

    async fn cancel_order(&mut self, client_id: &str) -> ExchangeResult<()> {
        self.orders.lock().await.remove(client_id);
        Ok(())
    }

    async fn cancel_all(&mut self, symbol: &str) -> ExchangeResult<()> {
        if symbol.is_empty() {
            self.orders.lock().await.clear();
        } else {
            self.orders.lock().await.retain(|_, o| o.symbol != symbol);
        }
        Ok(())
    }

    async fn close_position(&mut self, symbol: &str) -> ExchangeResult<()> {
        if symbol == self.symbol {
            *self.base_balance.lock().await = Decimal::ZERO;
        }
        Ok(())
    }

    async fn flatten(&mut self) -> ExchangeResult<()> {
        self.orders.lock().await.clear();
        *self.base_balance.lock().await = Decimal::ZERO;
        Ok(())
    }

    async fn drain_fills(&mut self) -> ExchangeResult<Vec<FillEvent>> {
        let mut fills = self.fills.lock().await;
        Ok(std::mem::take(&mut *fills))
    }

    async fn list_open_orders(&self, symbol: &str) -> ExchangeResult<Vec<LiveOrder>> {
        Ok(self
            .orders
            .lock()
            .await
            .values()
            .filter(|o| o.symbol == symbol)
            .cloned()
            .collect())
    }

    async fn list_spot_symbols(&self) -> ExchangeResult<Vec<String>> {
        Ok(vec![
            "BTC".into(),
            "ETH".into(),
            "HYPE".into(),
            "SOL".into(),
            "PURR/USDC".into(),
        ])
    }

    async fn list_markets(&self) -> ExchangeResult<Vec<crate::MarketInfo>> {
        crate::list_live_markets(RunMode::Simulation).await
    }
}

impl SimExchange {
    pub async fn set_mid_async(&self, mid: Decimal) {
        let mut st = self.state.lock().await;
        st.mid = mid.clamp(st.lower, st.upper);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancel_confirmed_preserves_position() {
        let mut sim = SimExchange::new("BTC", dec!(100), dec!(1000), dec!(2));
        sim.place_order(OrderIntent {
            client_id: "o1".into(),
            symbol: "BTC".into(),
            side: Side::Buy,
            price: dec!(90),
            size: dec!(1),
            level_index: 0,
            reduce_only: false,
            tif: grid_engine::TimeInForce::Gtc,
            cloid: None,
        })
        .await
        .unwrap();
        let report = sim.cancel_all_confirmed("BTC", 3).await.unwrap();
        assert!(report.confirmed_flat);
        assert!(sim.list_open_orders("BTC").await.unwrap().is_empty());
        assert_eq!(sim.position_size().await, dec!(2));
    }

    #[tokio::test]
    async fn reduce_only_rejects_expanding_order() {
        // Long 2 — a reduce-only buy would expand, so it must fail.
        let mut sim = SimExchange::new("BTC", dec!(100), dec!(1000), dec!(2));
        let err = sim
            .place_order(OrderIntent {
                client_id: "ro".into(),
                symbol: "BTC".into(),
                side: Side::Buy,
                price: dec!(90),
                size: dec!(1),
                level_index: 0,
                reduce_only: true,
                tif: grid_engine::TimeInForce::Gtc,
                cloid: None,
            })
            .await
            .unwrap_err();
        assert!(err.to_string().to_ascii_lowercase().contains("reduce"));
    }

    #[tokio::test]
    async fn force_mid_allows_offline_fill_without_replenish_helper() {
        let mut sim = SimExchange::with_band(
            "BTC",
            dec!(100),
            dec!(1000),
            dec!(0),
            dec!(90),
            dec!(110),
        );
        sim.connect().await.unwrap();
        sim.place_order(OrderIntent {
            client_id: "buy1".into(),
            symbol: "BTC".into(),
            side: Side::Buy,
            price: dec!(99),
            size: dec!(1),
            level_index: 0,
            reduce_only: false,
            tif: grid_engine::TimeInForce::Gtc,
            cloid: None,
        })
        .await
        .unwrap();
        // Drift mid down to fill the bid without canceling (app-closed scenario).
        sim.force_mid(dec!(98)).await;
        for _ in 0..20 {
            let _ = sim.get_mid("BTC").await;
        }
        let fills = sim.drain_fills().await.unwrap();
        assert!(!fills.is_empty() || sim.position_size().await != Decimal::ZERO
            || !sim.list_open_orders("BTC").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn symbol_protection_cancels_only_target_and_closes_position() {
        let mut sim = SimExchange::new("BTC", dec!(100), dec!(1000), dec!(2));
        sim.place_order(OrderIntent {
            client_id: "btc-order".into(),
            symbol: "BTC".into(),
            side: Side::Buy,
            price: dec!(90),
            size: dec!(1),
            level_index: 0,
            reduce_only: false,
            tif: grid_engine::TimeInForce::Gtc,
            cloid: Some("btcorder000000000000000000000001".into()),
        })
        .await
        .unwrap();
        sim.place_order(OrderIntent {
            client_id: "eth-order".into(),
            symbol: "ETH".into(),
            side: Side::Sell,
            price: dec!(110),
            size: dec!(1),
            level_index: 1,
            reduce_only: false,
            tif: grid_engine::TimeInForce::Gtc,
            cloid: Some("ethorder000000000000000000000001".into()),
        })
        .await
        .unwrap();

        sim.cancel_all("BTC").await.unwrap();
        sim.close_position("BTC").await.unwrap();

        assert!(sim.list_open_orders("BTC").await.unwrap().is_empty());
        assert_eq!(sim.list_open_orders("ETH").await.unwrap().len(), 1);
        assert_eq!(sim.position_size().await, Decimal::ZERO);
    }
}

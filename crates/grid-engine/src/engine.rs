use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::{
    generate_levels, new_order_id,
    risk::{RiskConfig, RiskState},
    types::{
        BotSnapshot, BotStatus, BreakoutAction, FillEvent, GridConfig, LiveOrder, OrderIntent,
        RunMode, Side,
    },
    GridError, GridResult,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineEvent {
    Started,
    Paused,
    Resumed,
    Stopped,
    Halted { reason: String },
    Breakout { price: Decimal },
    OrderPlaced { order: LiveOrder },
    OrderCanceled { client_id: String },
    Filled { fill: FillEvent, realized_pnl: Decimal },
    Message { text: String },
}

pub struct GridEngine {
    pub config: GridConfig,
    pub mode: RunMode,
    pub status: BotStatus,
    pub mid_price: Option<Decimal>,
    pub open_orders: Vec<LiveOrder>,
    pub risk: RiskState,
    pub risk_cfg: RiskConfig,
    pub events: Vec<String>,
    /// Net position (perp: signed; spot: long-only ≥ 0).
    position_base: Decimal,
    /// Volume-weighted average entry of current open position.
    avg_entry: Option<Decimal>,
    /// Exchange-reported unrealized PnL when available (overrides mid-mark estimate).
    exchange_unrealized: Option<Decimal>,
}

impl GridEngine {
    pub fn new(config: GridConfig, mode: RunMode, starting_equity: Decimal) -> GridResult<Self> {
        config.validate()?;
        let risk_cfg = RiskConfig {
            max_drawdown_pct: config.max_drawdown_pct,
            max_daily_loss: config.max_daily_loss,
            max_order_failures: config.max_order_failures,
            starting_equity,
        };
        Ok(Self {
            config,
            mode,
            status: BotStatus::Idle,
            mid_price: None,
            open_orders: Vec::new(),
            risk: RiskState::new(starting_equity),
            risk_cfg,
            events: Vec::new(),
            position_base: Decimal::ZERO,
            avg_entry: None,
            exchange_unrealized: None,
        })
    }

    fn push_event(&mut self, text: impl Into<String>) {
        let text = text.into();
        self.events.push(text);
        if self.events.len() > 200 {
            let drain = self.events.len() - 200;
            self.events.drain(0..drain);
        }
    }

    pub fn ensure_not_halted(&self) -> GridResult<()> {
        if self.risk.halted {
            return Err(GridError::RiskHalt(
                self.risk
                    .halt_reason
                    .clone()
                    .unwrap_or_else(|| "halted".into()),
            ));
        }
        Ok(())
    }

    /// Bootstrap resting grid orders around mid.
    ///
    /// - **Perp**: place buys below mid and sells above mid (neutral grid; no inventory needed).
    /// - **Spot**: buys only below mid; sells appear after buys fill.
    pub fn bootstrap_intents(&mut self, mid_price: Decimal) -> GridResult<Vec<OrderIntent>> {
        self.ensure_not_halted()?;
        self.mid_price = Some(mid_price);
        let levels = generate_levels(&self.config, mid_price)?;
        let intents: Vec<OrderIntent> = levels
            .into_iter()
            .filter(|level| match self.config.market {
                crate::MarketKind::Perp => level.price != mid_price,
                crate::MarketKind::Spot => level.side == Side::Buy && level.price < mid_price,
            })
            .map(|level| OrderIntent {
                client_id: new_order_id(),
                symbol: self.config.symbol.clone(),
                side: level.side,
                price: level.price,
                size: level.size,
                level_index: level.index,
            })
            .collect();
        self.status = BotStatus::Running;
        let buys = intents.iter().filter(|i| i.side == Side::Buy).count();
        let sells = intents.iter().filter(|i| i.side == Side::Sell).count();
        self.push_event(format!(
            "bootstrapped {} orders ({} buy / {} sell) around mid={} market={:?}",
            intents.len(),
            buys,
            sells,
            mid_price,
            self.config.market
        ));
        Ok(intents)
    }

    pub fn register_live_order(&mut self, order: LiveOrder) {
        self.risk.on_order_success();
        self.push_event(format!(
            "placed {:?} {} @ {}",
            order.side, order.size, order.price
        ));
        self.open_orders.push(order);
    }

    pub fn live_orders(&self) -> &[LiveOrder] {
        &self.open_orders
    }

    pub fn note_order_failure(&mut self, err: &str) -> Option<EngineEvent> {
        self.risk.on_order_failure(&self.risk_cfg);
        self.push_event(format!("order failed: {err}"));
        if self.risk.halted {
            self.status = BotStatus::Halted;
            let reason = self
                .risk
                .halt_reason
                .clone()
                .unwrap_or_else(|| "risk halt".into());
            return Some(EngineEvent::Halted { reason });
        }
        None
    }

    pub fn pause(&mut self) {
        if self.status == BotStatus::Running {
            self.status = BotStatus::Paused;
            self.push_event("paused");
        }
    }

    pub fn resume(&mut self) -> GridResult<()> {
        self.ensure_not_halted()?;
        if self.status == BotStatus::Paused {
            self.status = BotStatus::Running;
            self.push_event("resumed");
        }
        Ok(())
    }

    pub fn stop(&mut self) -> Vec<String> {
        let ids: Vec<String> = self
            .open_orders
            .iter()
            .map(|o| o.client_id.clone())
            .collect();
        self.open_orders.clear();
        self.status = BotStatus::Idle;
        self.push_event("stopped");
        ids
    }

    pub fn on_mid_price(&mut self, price: Decimal) -> Vec<EngineEvent> {
        self.mid_price = Some(price);
        let mut events = Vec::new();
        if self.status != BotStatus::Running {
            return events;
        }
        if price < self.config.lower_price || price > self.config.upper_price {
            events.push(EngineEvent::Breakout { price });
            self.push_event(format!("breakout at {price}"));
            match self.config.breakout_action {
                BreakoutAction::AlertOnly => {}
                BreakoutAction::Pause => {
                    self.pause();
                    events.push(EngineEvent::Paused);
                }
                BreakoutAction::CancelAndPause => {
                    self.open_orders.clear();
                    self.pause();
                    events.push(EngineEvent::Paused);
                    events.push(EngineEvent::Message {
                        text: "canceled open orders on breakout".into(),
                    });
                }
            }
        }
        events
    }

    /// Process a fill: update position; replenish only when the resting order is fully filled.
    pub fn on_fill(&mut self, fill: FillEvent) -> GridResult<(Decimal, Option<OrderIntent>)> {
        self.ensure_not_halted()?;
        let mut fully_filled = false;
        // Replenish must use base-coin size, never USDC notional (size_per_level).
        let mut replenish_size = fill.size;
        if let Some(idx) = self
            .open_orders
            .iter()
            .position(|o| o.client_id == fill.client_id)
        {
            let level_size = self.open_orders[idx].level_size();
            let before = self.open_orders[idx].size;
            let remaining = before - fill.size;
            if remaining.abs() <= Decimal::new(1, 8) || remaining <= Decimal::ZERO {
                self.open_orders.remove(idx);
                fully_filled = true;
                replenish_size = level_size;
            } else {
                self.open_orders[idx].size = remaining;
            }
        }

        let signed = match fill.side {
            Side::Buy => fill.size,
            Side::Sell => -fill.size,
        };
        let realized = self.apply_position_delta(fill.price, signed);
        // Local estimate until next exchange sync.
        self.exchange_unrealized = None;

        self.risk.on_fill_pnl(realized, &self.risk_cfg);
        self.push_event(format!(
            "filled {:?} {} @ {} realized={} pos={}",
            fill.side, fill.size, fill.price, realized, self.position_base
        ));

        if self.risk.halted {
            self.status = BotStatus::Halted;
            return Err(GridError::RiskHalt(
                self.risk
                    .halt_reason
                    .clone()
                    .unwrap_or_else(|| "risk halt".into()),
            ));
        }

        if self.status != BotStatus::Running || !fully_filled {
            return Ok((realized, None));
        }

        // Replenish opposite side one grid step away
        let step = (self.config.upper_price - self.config.lower_price)
            / Decimal::from(self.config.grid_count.saturating_sub(1).max(1));
        let repl_side = fill.side.opposite();
        let repl_price = match repl_side {
            Side::Sell => (fill.price + step).min(self.config.upper_price),
            Side::Buy => (fill.price - step).max(self.config.lower_price),
        };
        let intent = OrderIntent {
            client_id: new_order_id(),
            symbol: self.config.symbol.clone(),
            side: repl_side,
            price: repl_price.round_dp(8),
            size: replenish_size,
            level_index: fill.level_index,
        };
        Ok((realized, Some(intent)))
    }

    /// Overwrite position from the exchange (source of truth for the dashboard).
    pub fn sync_position_from_exchange(
        &mut self,
        size: Decimal,
        entry: Option<Decimal>,
        unrealized: Option<Decimal>,
    ) {
        let prev = self.position_base;
        let delta = (prev - size).abs();
        self.position_base = size;
        if size == Decimal::ZERO {
            self.avg_entry = None;
            self.exchange_unrealized = Some(Decimal::ZERO);
        } else {
            if let Some(px) = entry {
                self.avg_entry = Some(px);
            }
            self.exchange_unrealized = unrealized;
        }
        // Only log meaningful drifts (avoid spam on tiny rounding).
        if delta > Decimal::new(1, 6) {
            self.push_event(format!(
                "position synced from exchange: {prev} → {size}"
            ));
        }
    }

    /// Apply signed size delta (+buy / −sell) with VWAP and realize PnL when reducing.
    fn apply_position_delta(&mut self, price: Decimal, signed_qty: Decimal) -> Decimal {
        let mut realized = Decimal::ZERO;
        let pos = self.position_base;
        if pos == Decimal::ZERO {
            self.position_base = signed_qty;
            self.avg_entry = Some(price);
            return realized;
        }
        let same_dir = (pos > Decimal::ZERO && signed_qty > Decimal::ZERO)
            || (pos < Decimal::ZERO && signed_qty < Decimal::ZERO);
        if same_dir {
            let abs_pos = pos.abs();
            let abs_q = signed_qty.abs();
            let entry = self.avg_entry.unwrap_or(price);
            self.avg_entry = Some((entry * abs_pos + price * abs_q) / (abs_pos + abs_q));
            self.position_base = pos + signed_qty;
            return realized;
        }
        // Reducing or flipping
        let close = pos.abs().min(signed_qty.abs());
        if let Some(entry) = self.avg_entry {
            let dir = if pos > Decimal::ZERO {
                Decimal::ONE
            } else {
                -Decimal::ONE
            };
            realized = (price - entry) * close * dir;
        }
        self.position_base = pos + signed_qty;
        if self.position_base == Decimal::ZERO {
            self.avg_entry = None;
        } else if (pos > Decimal::ZERO) != (self.position_base > Decimal::ZERO) {
            // Flipped: remainder opens at this fill price
            self.avg_entry = Some(price);
        }
        realized
    }

    pub fn clear_events(&mut self) {
        self.events.clear();
        self.push_event("logs cleared");
    }

    pub fn note(&mut self, text: impl Into<String>) {
        self.push_event(text);
    }

    fn unrealized_pnl(&self) -> Decimal {
        if let Some(u) = self.exchange_unrealized {
            return u;
        }
        match (self.mid_price, self.avg_entry) {
            (Some(mid), Some(entry)) if self.position_base != Decimal::ZERO => {
                let dir = if self.position_base > Decimal::ZERO {
                    Decimal::ONE
                } else {
                    -Decimal::ONE
                };
                (mid - entry) * self.position_base.abs() * dir
            }
            _ => Decimal::ZERO,
        }
    }

    pub fn snapshot(&self) -> BotSnapshot {
        let mut resting_orders: Vec<crate::RestingOrderView> = self
            .open_orders
            .iter()
            .map(|o| crate::RestingOrderView {
                side: o.side,
                price: o.price,
                size: o.size,
            })
            .collect();
        resting_orders.sort_by(|a, b| a.price.cmp(&b.price));
        BotSnapshot {
            status: self.status,
            mode: self.mode,
            symbol: self.config.symbol.clone(),
            mid_price: self.mid_price,
            open_orders: self.open_orders.len(),
            resting_orders,
            position_base: self.position_base.round_dp(8),
            avg_entry_price: self.avg_entry.map(|p| p.round_dp(8)),
            realized_pnl: self.risk.realized_pnl.round_dp(8),
            unrealized_pnl: self.unrealized_pnl().round_dp(8),
            events_tail: self.events.iter().rev().take(30).cloned().rev().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn sample_cfg() -> GridConfig {
        GridConfig {
            symbol: "BTC".into(),
            lower_price: dec!(90000),
            upper_price: dec!(100000),
            grid_count: 5,
            total_budget: dec!(1000),
            spacing: crate::GridSpacing::Arithmetic,
            breakout_action: BreakoutAction::Pause,
            max_drawdown_pct: dec!(20),
            max_daily_loss: dec!(100),
            max_order_failures: 5,
            market: crate::MarketKind::Perp,
            leverage: 5,
            is_cross: true,
        }
    }

    #[test]
    fn bootstrap_perp_both_sides() {
        let mut engine = GridEngine::new(sample_cfg(), RunMode::Simulation, dec!(1000)).unwrap();
        let intents = engine.bootstrap_intents(dec!(95000)).unwrap();
        assert!(intents.iter().any(|i| i.side == Side::Buy));
        assert!(intents.iter().any(|i| i.side == Side::Sell));
    }

    #[test]
    fn bootstrap_spot_only_buys() {
        let mut cfg = sample_cfg();
        cfg.market = crate::MarketKind::Spot;
        let mut engine = GridEngine::new(cfg, RunMode::Simulation, dec!(1000)).unwrap();
        let intents = engine.bootstrap_intents(dec!(95000)).unwrap();
        assert!(!intents.is_empty());
        assert!(intents.iter().all(|i| i.side == Side::Buy));
    }

    #[test]
    fn short_fill_then_cover_realizes_pnl() {
        let mut engine = GridEngine::new(sample_cfg(), RunMode::Simulation, dec!(1000)).unwrap();
        let _ = engine.bootstrap_intents(dec!(95000)).unwrap();
        let sell = FillEvent {
            client_id: "s1".into(),
            symbol: "BTC".into(),
            side: Side::Sell,
            price: dec!(96000),
            size: dec!(0.01),
            level_index: 3,
            fee: Decimal::ZERO,
        };
        let (pnl0, _) = engine.on_fill(sell).unwrap();
        assert_eq!(pnl0, Decimal::ZERO);
        assert!(engine.snapshot().position_base < Decimal::ZERO);

        let buy = FillEvent {
            client_id: "b1".into(),
            symbol: "BTC".into(),
            side: Side::Buy,
            price: dec!(95000),
            size: dec!(0.01),
            level_index: 2,
            fee: Decimal::ZERO,
        };
        let (pnl1, _) = engine.on_fill(buy).unwrap();
        assert!(pnl1 > Decimal::ZERO); // shorted 96k covered 95k
        assert_eq!(engine.snapshot().position_base, Decimal::ZERO);
    }

    #[test]
    fn buy_fill_replenishes_sell_above() {
        let mut cfg = sample_cfg();
        cfg.market = crate::MarketKind::Spot;
        let mut engine = GridEngine::new(cfg, RunMode::Simulation, dec!(1000)).unwrap();
        let intents = engine.bootstrap_intents(dec!(95000)).unwrap();
        let buy = intents[0].clone();
        engine.register_live_order(LiveOrder::new(
            buy.client_id.clone(),
            Some("t".into()),
            buy.symbol.clone(),
            buy.side,
            buy.price,
            buy.size,
            buy.level_index,
        ));
        let fill = FillEvent {
            client_id: buy.client_id.clone(),
            symbol: buy.symbol.clone(),
            side: Side::Buy,
            price: buy.price,
            size: buy.size,
            level_index: buy.level_index,
            fee: Decimal::ZERO,
        };
        let buy_price = buy.price;
        let buy_size = buy.size;
        let (_pnl, replenish) = engine.on_fill(fill).unwrap();
        let sell = replenish.expect("should place sell after buy");
        assert_eq!(sell.side, Side::Sell);
        assert!(sell.price > buy_price);
        assert_eq!(sell.size, buy_size);

        let snap = engine.snapshot();
        assert_eq!(snap.position_base, buy_size);
        assert_eq!(snap.avg_entry_price, Some(buy_price));

        engine.on_mid_price(buy_price + dec!(1000));
        let snap2 = engine.snapshot();
        assert!(snap2.unrealized_pnl > Decimal::ZERO);
    }
}

use chrono::{Local, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    generate_levels_with_bounds, new_order_id,
    risk::{RiskConfig, RiskState},
    types::{
        BotSnapshot, BotStatus, BreakoutAction, FillEvent, GridConfig, GridMode, LiveOrder,
        OrderIntent, RunMode, Side, TimeInForce,
    },
    volatility::{derive_bounds, is_outside_bounds, reentered_with_hysteresis, AtrMetrics},
    GridError, GridResult,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineEvent {
    Started,
    Paused,
    Resumed,
    Stopped,
    Detached {
        reason: String,
    },
    Halted {
        reason: String,
    },
    Breakout {
        price: Decimal,
    },
    SoftBreakout {
        price: Decimal,
        confirm_count: u32,
    },
    RecenterRequested {
        price: Decimal,
        lower: Decimal,
        upper: Decimal,
        generation: u32,
    },
    Recentered {
        lower: Decimal,
        upper: Decimal,
        generation: u32,
    },
    Recovering {
        reason: String,
    },
    ProtectiveExitRequested {
        price: Decimal,
        close_position: bool,
        risk_triggered: bool,
    },
    OrderPlaced {
        order: LiveOrder,
    },
    OrderCanceled {
        client_id: String,
    },
    Filled {
        fill: FillEvent,
        realized_pnl: Decimal,
    },
    Message {
        text: String,
    },
}

/// Plan produced when the engine decides to recenter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecenterPlan {
    pub mid: Decimal,
    pub lower: Decimal,
    pub upper: Decimal,
    pub generation: u32,
    pub intents: Vec<OrderIntent>,
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
    status_note: Option<String>,
    /// Net position (perp: signed; spot: long-only ≥ 0).
    position_base: Decimal,
    /// Volume-weighted average entry of current open position.
    avg_entry: Option<Decimal>,
    /// Exchange-reported unrealized PnL when available (overrides mid-mark estimate).
    exchange_unrealized: Option<Decimal>,
    /// Net funding cash flow during this strategy session.
    funding_pnl: Decimal,
    /// Fills processed during this strategy session.
    fill_count: usize,
    /// Exchange-reported liquidation price (perps), when known.
    liquidation_price: Option<Decimal>,
    /// Session-level maximum one-sided grid notional; never reset by recenter.
    max_position_notional: Decimal,
    /// Active trading band (may differ from config after recenter).
    active_lower: Decimal,
    active_upper: Decimal,
    atr: Option<Decimal>,
    atr_pct: Option<Decimal>,
    recenter_generation: u32,
    recenters_today: u32,
    recenters_local_date: String,
    last_recenter_ms: Option<i64>,
    soft_breakout_count: u32,
    soft_breakout_side: Option<i8>, // -1 below, +1 above
    session_id: String,
    last_tick_ms: Option<i64>,
    health_note: Option<String>,
    /// Outside-band candle confirmations accumulated by runner.
    outside_confirm_bars: u32,
    /// Desired resting order count (usually bootstrap size). Used to repair holes.
    pub target_resting: usize,
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
        let active_lower = config.lower_price;
        let active_upper = config.upper_price;
        Ok(Self {
            config,
            mode,
            status: BotStatus::Idle,
            mid_price: None,
            open_orders: Vec::new(),
            risk: RiskState::new(starting_equity),
            risk_cfg,
            events: Vec::new(),
            status_note: None,
            position_base: Decimal::ZERO,
            avg_entry: None,
            exchange_unrealized: None,
            funding_pnl: Decimal::ZERO,
            fill_count: 0,
            liquidation_price: None,
            max_position_notional: Decimal::ZERO,
            active_lower,
            active_upper,
            atr: None,
            atr_pct: None,
            recenter_generation: 0,
            recenters_today: 0,
            recenters_local_date: Local::now().format("%Y-%m-%d").to_string(),
            last_recenter_ms: None,
            soft_breakout_count: 0,
            soft_breakout_side: None,
            session_id: Uuid::new_v4().to_string(),
            last_tick_ms: None,
            health_note: None,
            outside_confirm_bars: 0,
            target_resting: 0,
        })
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn set_session_id(&mut self, id: impl Into<String>) {
        self.session_id = id.into();
    }

    pub fn active_bounds(&self) -> (Decimal, Decimal) {
        (self.active_lower, self.active_upper)
    }

    pub fn max_position_notional(&self) -> Decimal {
        self.max_position_notional
    }

    pub fn set_atr(&mut self, metrics: &AtrMetrics) {
        self.atr = Some(metrics.atr);
        self.atr_pct = Some(metrics.atr_pct);
    }

    pub fn set_health_note(&mut self, note: Option<String>) {
        self.health_note = note;
    }

    pub fn touch_tick(&mut self) {
        self.last_tick_ms = Some(Utc::now().timestamp_millis());
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

    fn step(&self) -> Decimal {
        (self.active_upper - self.active_lower)
            / Decimal::from(self.config.grid_count.saturating_sub(1).max(1))
    }

    /// Remaining notional budget for expanding position in `side` direction.
    pub fn expand_budget_notional(&self, side: Side) -> Decimal {
        if self.max_position_notional <= Decimal::ZERO {
            return Decimal::MAX;
        }
        let mid = self.mid_price.unwrap_or(
            (self.active_lower + self.active_upper) / Decimal::from(2),
        );
        let pos_notional = self.position_base.abs() * mid;
        let used = match side {
            Side::Buy if self.position_base >= Decimal::ZERO => pos_notional,
            Side::Sell if self.position_base <= Decimal::ZERO => pos_notional,
            // Reducing opposite inventory: no expand budget consumed.
            _ => Decimal::ZERO,
        };
        (self.max_position_notional - used).max(Decimal::ZERO)
    }

    fn intent_is_expanding(&self, side: Side) -> bool {
        match side {
            Side::Buy => self.position_base >= Decimal::ZERO,
            Side::Sell => self.position_base <= Decimal::ZERO,
        }
    }

    /// Whether `side` reduces the current signed position (buy into short / sell into long).
    fn intent_is_reducing(&self, side: Side) -> bool {
        match side {
            Side::Buy => self.position_base < Decimal::ZERO,
            Side::Sell => self.position_base > Decimal::ZERO,
        }
    }

    /// Assign reduce-only when the order fully fits in remaining reducible inventory.
    ///
    /// Hyperliquid rejects `reduce_only` that would flip through flat
    /// ("Reduce only order would increase position"). Larger opposite-side grid
    /// orders stay non-RO so the book can oscillate through zero.
    fn assign_reduce_only(&self, side: Side, size: Decimal, reduce_budget: &mut Decimal) -> bool {
        if !self.intent_is_reducing(side) || size <= Decimal::ZERO {
            return false;
        }
        if size <= *reduce_budget {
            *reduce_budget -= size;
            true
        } else {
            false
        }
    }

    /// Bootstrap resting grid orders around mid using active bounds.
    pub fn bootstrap_intents(&mut self, mid_price: Decimal) -> GridResult<Vec<OrderIntent>> {
        self.ensure_not_halted()?;
        if mid_price <= self.active_lower || mid_price >= self.active_upper {
            return Err(GridError::InvalidConfig(format!(
                "mid price {mid_price} must be inside grid range {}–{}",
                self.active_lower, self.active_upper
            )));
        }
        self.mid_price = Some(mid_price);
        let levels =
            generate_levels_with_bounds(&self.config, mid_price, self.active_lower, self.active_upper)?;
        let mut reduce_budget = self.position_base.abs();
        let intents: Vec<OrderIntent> = levels
            .into_iter()
            .filter(|level| match self.config.market {
                crate::MarketKind::Perp => level.price != mid_price,
                crate::MarketKind::Spot => level.side == Side::Buy && level.price < mid_price,
            })
            .filter(|level| {
                // Inventory-aware: prefer reduce-only / block expand when over budget.
                if self.intent_is_expanding(level.side) {
                    let notional = level.price * level.size;
                    notional <= self.expand_budget_notional(level.side)
                        || self.position_base == Decimal::ZERO
                            && self.max_position_notional == Decimal::ZERO
                } else {
                    true
                }
            })
            .map(|level| {
                let reduce_only =
                    self.assign_reduce_only(level.side, level.size, &mut reduce_budget);
                let client_id = new_order_id();
                OrderIntent {
                    client_id: client_id.clone(),
                    symbol: self.config.symbol.clone(),
                    side: level.side,
                    price: level.price,
                    size: level.size,
                    level_index: level.index,
                    reduce_only,
                    tif: TimeInForce::Gtc,
                    cloid: Some(client_id.replace('-', "")),
                }
            })
            .collect();

        if self.max_position_notional <= Decimal::ZERO {
            let buy_notional = intents
                .iter()
                .filter(|i| i.side == Side::Buy)
                .map(|i| i.price * i.size)
                .sum::<Decimal>();
            let sell_notional = intents
                .iter()
                .filter(|i| i.side == Side::Sell)
                .map(|i| i.price * i.size)
                .sum::<Decimal>();
            self.max_position_notional = buy_notional.max(sell_notional);
        }

        self.status = BotStatus::Running;
        self.status_note = None;
        self.soft_breakout_count = 0;
        self.soft_breakout_side = None;
        self.outside_confirm_bars = 0;
        self.target_resting = intents.len();
        let buys = intents.iter().filter(|i| i.side == Side::Buy).count();
        let sells = intents.iter().filter(|i| i.side == Side::Sell).count();
        self.push_event(format!(
            "bootstrapped {} orders ({} buy / {} sell) around mid={} band={}–{} market={:?}",
            intents.len(),
            buys,
            sells,
            mid_price,
            self.active_lower,
            self.active_upper,
            self.config.market
        ));
        Ok(intents)
    }

    pub fn register_live_order(&mut self, order: LiveOrder) {
        self.risk.on_order_success();
        self.push_event(format!(
            "placed {:?} {} @ {}{}",
            order.side,
            order.size,
            order.price,
            if order.reduce_only { " reduce_only" } else { "" }
        ));
        self.open_orders.push(order);
    }

    pub fn live_orders(&self) -> &[LiveOrder] {
        &self.open_orders
    }

    pub fn replace_open_orders(&mut self, orders: Vec<LiveOrder>) {
        self.open_orders = orders;
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

    pub fn pause_with_reason(&mut self, reason: impl Into<String>) {
        if matches!(
            self.status,
            BotStatus::Running | BotStatus::SoftBreakout | BotStatus::Recovering
        ) {
            self.status = BotStatus::Paused;
            self.status_note = Some(reason.into());
            self.push_event("paused");
        }
    }

    pub fn pause(&mut self) {
        self.pause_with_reason("manual pause");
    }

    pub fn resume(&mut self) -> GridResult<()> {
        self.ensure_not_halted()?;
        if matches!(
            self.status,
            BotStatus::Paused | BotStatus::SoftBreakout | BotStatus::Detached | BotStatus::Recovering
        ) {
            self.status = BotStatus::Running;
            self.status_note = None;
            self.push_event("resumed");
        }
        Ok(())
    }

    pub fn mark_recovering(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        self.status = BotStatus::Recovering;
        self.status_note = Some(reason.clone());
        self.health_note = Some(reason.clone());
        self.push_event(format!("recovering: {reason}"));
    }

    pub fn clear_recovering(&mut self) {
        if matches!(self.status, BotStatus::Recovering | BotStatus::Detached) {
            self.status = BotStatus::Running;
            self.status_note = None;
            self.health_note = None;
            self.push_event("recovery complete");
        }
    }

    pub fn halt_integrity(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        self.open_orders.clear();
        self.status = BotStatus::Halted;
        self.status_note = Some(format!("integrity halt: {reason}"));
        self.health_note = Some(reason.clone());
        self.push_event(format!("integrity halt: {reason}"));
    }

    pub fn mark_detached(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        self.status = BotStatus::Detached;
        self.status_note = Some(reason.clone());
        self.push_event(format!("detached: {reason}"));
    }

    pub fn stop(&mut self) -> Vec<String> {
        let ids: Vec<String> = self
            .open_orders
            .iter()
            .map(|o| o.client_id.clone())
            .collect();
        self.open_orders.clear();
        self.status = BotStatus::Idle;
        self.status_note = None;
        self.push_event("stopped");
        ids
    }

    /// Apply ATR metrics and optionally refresh active bounds when idle / before bootstrap.
    pub fn apply_dynamic_bounds_from_atr(
        &mut self,
        mid: Decimal,
        atr_pct: Decimal,
    ) -> GridResult<(Decimal, Decimal)> {
        let d = &self.config.dynamic;
        let mut half = atr_pct * d.atr_mult;
        if half < d.min_half_width_pct {
            half = d.min_half_width_pct;
        }
        if half > d.max_half_width_pct {
            half = d.max_half_width_pct;
        }
        let (lower, upper) = derive_bounds(mid, half)?;
        self.active_lower = lower;
        self.active_upper = upper;
        self.config.lower_price = lower;
        self.config.upper_price = upper;
        self.atr_pct = Some(atr_pct);
        Ok((lower, upper))
    }

    fn roll_recenter_day(&mut self) {
        let today = Local::now().format("%Y-%m-%d").to_string();
        if self.recenters_local_date != today {
            self.recenters_local_date = today;
            self.recenters_today = 0;
        }
    }

    pub fn can_recenter_now(&mut self) -> Result<(), String> {
        self.roll_recenter_day();
        let d = &self.config.dynamic;
        if self.recenters_today >= d.max_recenters_per_day {
            return Err(format!(
                "max recenters per day ({}) reached",
                d.max_recenters_per_day
            ));
        }
        if let Some(last) = self.last_recenter_ms {
            let now = Utc::now().timestamp_millis();
            let elapsed = (now - last).max(0) as u64 / 1000;
            if elapsed < d.recenter_cooldown_secs {
                return Err(format!(
                    "recenter cooldown {}s remaining",
                    d.recenter_cooldown_secs.saturating_sub(elapsed)
                ));
            }
        }
        Ok(())
    }

    /// Called by runner when a closed candle confirms outside-band.
    pub fn note_outside_confirm_bar(&mut self, outside: bool) {
        if outside {
            self.outside_confirm_bars = self.outside_confirm_bars.saturating_add(1);
        } else {
            self.outside_confirm_bars = 0;
        }
    }

    pub fn outside_confirm_bars(&self) -> u32 {
        self.outside_confirm_bars
    }

    pub fn on_mid_price(&mut self, price: Decimal) -> Vec<EngineEvent> {
        self.mid_price = Some(price);
        self.exchange_unrealized = None;
        self.touch_tick();
        let mut events = Vec::new();

        // Risk equity updates still run in soft / paused / recovering states.
        let risk_watch = matches!(
            self.status,
            BotStatus::Running
                | BotStatus::SoftBreakout
                | BotStatus::Paused
                | BotStatus::Recovering
                | BotStatus::Recentering
        );
        if risk_watch {
            let unrealized = self.unrealized_pnl();
            self.risk
                .on_strategy_equity(unrealized + self.funding_pnl, &self.risk_cfg);
            if self.risk.halted {
                let reason = self
                    .risk
                    .halt_reason
                    .clone()
                    .unwrap_or_else(|| "strategy equity risk limit reached".into());
                self.status = BotStatus::ProtectiveExit;
                self.push_event(format!("risk protective exit: {reason}"));
                events.push(EngineEvent::Halted {
                    reason: reason.clone(),
                });
                events.push(EngineEvent::ProtectiveExitRequested {
                    price,
                    close_position: true,
                    risk_triggered: true,
                });
                return events;
            }
        }

        if !matches!(
            self.status,
            BotStatus::Running | BotStatus::SoftBreakout
        ) {
            return events;
        }

        let outside = is_outside_bounds(price, self.active_lower, self.active_upper);
        if !outside {
            if self.status == BotStatus::SoftBreakout {
                let reentered = reentered_with_hysteresis(
                    price,
                    self.active_lower,
                    self.active_upper,
                    self.config.dynamic.reentry_hysteresis_pct,
                );
                if reentered {
                    self.status = BotStatus::Running;
                    self.status_note = None;
                    self.soft_breakout_count = 0;
                    self.soft_breakout_side = None;
                    self.outside_confirm_bars = 0;
                    self.push_event("soft breakout cleared; resumed running");
                }
            }
            return events;
        }

        let side: i8 = if price < self.active_lower { -1 } else { 1 };
        if self.soft_breakout_side != Some(side) {
            self.soft_breakout_side = Some(side);
            self.soft_breakout_count = 1;
        } else {
            self.soft_breakout_count = self.soft_breakout_count.saturating_add(1);
        }

        events.push(EngineEvent::Breakout { price });
        self.push_event(format!("breakout at {price}"));

        let action = self.config.breakout_action;
        let wants_recenter = matches!(action, BreakoutAction::Recenter)
            || (self.config.is_dynamic() && matches!(action, BreakoutAction::AlertOnly));

        if wants_recenter || matches!(action, BreakoutAction::Recenter) {
            // Soft phase: stop expanding inventory.
            if self.status == BotStatus::Running {
                self.status = BotStatus::SoftBreakout;
                self.status_note = Some(
                    "soft breakout: expansion paused; waiting for confirmation".into(),
                );
            }
            events.push(EngineEvent::SoftBreakout {
                price,
                confirm_count: self.outside_confirm_bars.max(self.soft_breakout_count),
            });

            let confirmed = self.outside_confirm_bars >= self.config.dynamic.confirm_bars.max(1)
                || self.soft_breakout_count >= self.config.dynamic.confirm_bars.max(1) * 20;
            // soft_breakout_count is tick-based (~3s); prefer candle confirms from runner.
            // Fall back: many ticks ≈ confirm if runner hasn't fed candles yet.
            let tick_fallback = self.soft_breakout_count >= 20; // ~60s at 3s/tick
            if confirmed || tick_fallback {
                match self.plan_recenter(price) {
                    Ok(plan) => {
                        self.status = BotStatus::Recentering;
                        self.status_note = Some("recentering grid; position retained".into());
                        events.push(EngineEvent::RecenterRequested {
                            price,
                            lower: plan.lower,
                            upper: plan.upper,
                            generation: plan.generation,
                        });
                    }
                    Err(e) => {
                        self.push_event(format!("recenter deferred: {e}"));
                    }
                }
            }
            return events;
        }

        match action {
            BreakoutAction::AlertOnly => {}
            BreakoutAction::Pause => {
                self.pause_with_reason(
                    "breakout pause: replenishment stopped; orders and position retained",
                );
                events.push(EngineEvent::Paused);
            }
            BreakoutAction::CancelAndPause => {
                self.status = BotStatus::ProtectiveExit;
                self.status_note =
                    Some("breakout stop: orders canceled; position retained".into());
                self.push_event("breakout protective exit: canceling symbol orders");
                events.push(EngineEvent::ProtectiveExitRequested {
                    price,
                    close_position: false,
                    risk_triggered: false,
                });
            }
            BreakoutAction::CancelCloseAndStop => {
                self.status = BotStatus::ProtectiveExit;
                self.status_note =
                    Some("breakout stop: orders canceled and position closed".into());
                self.push_event(
                    "breakout protective exit: canceling orders and closing position",
                );
                events.push(EngineEvent::ProtectiveExitRequested {
                    price,
                    close_position: true,
                    risk_triggered: false,
                });
            }
            BreakoutAction::Recenter => {}
        }
        events
    }

    /// Build a recenter plan around `mid` without mutating active bounds yet.
    pub fn plan_recenter(&mut self, mid: Decimal) -> Result<RecenterPlan, String> {
        self.can_recenter_now()?;
        let atr_pct = self.atr_pct.unwrap_or_else(|| {
            let width = self.active_upper - self.active_lower;
            if mid > Decimal::ZERO {
                (width / mid / Decimal::from(2)) * Decimal::from(100)
            } else {
                dec!(5)
            }
        });
        let d = &self.config.dynamic;
        let mut half = atr_pct * d.atr_mult;
        if half < d.min_half_width_pct {
            half = d.min_half_width_pct;
        }
        if half > d.max_half_width_pct {
            half = d.max_half_width_pct;
        }
        let (lower, upper) =
            derive_bounds(mid, half).map_err(|e| e.to_string())?;
        let generation = self.recenter_generation.saturating_add(1);
        // Temporarily compute intents with planned bounds.
        let saved = (self.active_lower, self.active_upper);
        self.active_lower = lower;
        self.active_upper = upper;
        let intents = self
            .bootstrap_intents_preview(mid)
            .map_err(|e| e.to_string())?;
        self.active_lower = saved.0;
        self.active_upper = saved.1;
        // bootstrap_intents_preview must not flip status permanently.
        self.status = BotStatus::Recentering;
        Ok(RecenterPlan {
            mid,
            lower,
            upper,
            generation,
            intents,
        })
    }

    fn bootstrap_intents_preview(&self, mid_price: Decimal) -> GridResult<Vec<OrderIntent>> {
        if mid_price <= self.active_lower || mid_price >= self.active_upper {
            return Err(GridError::InvalidConfig(format!(
                "mid price {mid_price} must be inside grid range {}–{}",
                self.active_lower, self.active_upper
            )));
        }
        let levels =
            generate_levels_with_bounds(&self.config, mid_price, self.active_lower, self.active_upper)?;
        let mut reduce_budget = self.position_base.abs();
        let intents = levels
            .into_iter()
            .filter(|level| match self.config.market {
                crate::MarketKind::Perp => level.price != mid_price,
                crate::MarketKind::Spot => level.side == Side::Buy && level.price < mid_price,
            })
            .filter(|level| {
                if self.intent_is_expanding(level.side) {
                    let notional = level.price * level.size;
                    notional <= self.expand_budget_notional(level.side)
                        || self.max_position_notional == Decimal::ZERO
                } else {
                    true
                }
            })
            .map(|level| {
                let reduce_only =
                    self.assign_reduce_only(level.side, level.size, &mut reduce_budget);
                let client_id = new_order_id();
                OrderIntent {
                    client_id: client_id.clone(),
                    symbol: self.config.symbol.clone(),
                    side: level.side,
                    price: level.price,
                    size: level.size,
                    level_index: level.index,
                    reduce_only,
                    tif: TimeInForce::Gtc,
                    cloid: Some(client_id.replace('-', "")),
                }
            })
            .collect();
        Ok(intents)
    }

    /// Commit recenter after exchange cancel/place succeeded.
    pub fn commit_recenter(&mut self, plan: &RecenterPlan) {
        self.roll_recenter_day();
        self.active_lower = plan.lower;
        self.active_upper = plan.upper;
        self.config.lower_price = plan.lower;
        self.config.upper_price = plan.upper;
        self.recenter_generation = plan.generation;
        self.recenters_today = self.recenters_today.saturating_add(1);
        self.last_recenter_ms = Some(Utc::now().timestamp_millis());
        self.soft_breakout_count = 0;
        self.soft_breakout_side = None;
        self.outside_confirm_bars = 0;
        self.open_orders.clear();
        self.status = BotStatus::Running;
        self.status_note = None;
        self.push_event(format!(
            "recentered gen={} band={}–{}",
            plan.generation, plan.lower, plan.upper
        ));
    }

    /// Mark exchange-confirmed cancellation while preserving the existing position.
    pub fn mark_orders_canceled_and_paused(&mut self) {
        self.open_orders.clear();
        self.status = BotStatus::BreakoutStopped;
        self.status_note = Some("breakout stop: orders canceled; position retained".into());
        self.push_event(
            "breakout protection complete: symbol orders canceled; position retained; fresh start required",
        );
    }

    /// Mark exchange-confirmed cancellation and a flat position.
    pub fn mark_breakout_stopped(&mut self) {
        self.open_orders.clear();
        self.position_base = Decimal::ZERO;
        self.avg_entry = None;
        self.exchange_unrealized = Some(Decimal::ZERO);
        self.liquidation_price = None;
        self.status = BotStatus::BreakoutStopped;
        self.status_note = Some("breakout stop: orders canceled and position closed".into());
        self.push_event("breakout protection complete: symbol orders canceled and position closed");
    }

    pub fn mark_risk_stopped(&mut self, reason: impl Into<String>) {
        self.open_orders.clear();
        self.position_base = Decimal::ZERO;
        self.avg_entry = None;
        self.liquidation_price = None;
        self.exchange_unrealized = Some(Decimal::ZERO);
        self.status = BotStatus::Halted;
        self.status_note = Some(reason.into());
        self.push_event(format!(
            "risk protection complete: orders canceled and position closed ({})",
            self.status_note.clone().unwrap_or_default()
        ));
    }

    pub fn mark_protective_exit_failed(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        self.status = BotStatus::Halted;
        self.status_note = Some(reason.clone());
        self.push_event(format!("protective exit failed: {reason}"));
    }

    /// Process a fill: update position; replenish only when the resting order is fully filled.
    pub fn on_fill(&mut self, fill: FillEvent) -> GridResult<(Decimal, Option<OrderIntent>)> {
        self.ensure_not_halted()?;
        let mut fully_filled = false;
        let mut matched_order = false;
        let mut replenish_size = fill.size;
        if let Some(idx) = self
            .open_orders
            .iter()
            .position(|o| o.client_id == fill.client_id || o.cloid.as_ref() == fill.cloid.as_ref().filter(|c| !c.is_empty()))
        {
            matched_order = true;
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
        } else {
            // Immediate-fill / phantom-sync: order already gone from the book.
            fully_filled = true;
            replenish_size = if fill.size > Decimal::ZERO {
                fill.size
            } else {
                replenish_size
            };
        }

        let signed = match fill.side {
            Side::Buy => fill.size,
            Side::Sell => -fill.size,
        };
        let position_before = self.position_base;
        let gross_realized = self.apply_position_delta(fill.price, signed);
        let realized = gross_realized - fill.fee;
        self.exchange_unrealized = None;
        self.fill_count += 1;

        self.risk.on_fill_pnl(realized, &self.risk_cfg);
        let unrealized = self.unrealized_pnl();
        self.risk
            .on_strategy_equity(unrealized + self.funding_pnl, &self.risk_cfg);
        let position_notional = self.position_base.abs() * fill.price;
        let position_limit = self.max_position_notional * Decimal::new(102, 2);
        if matched_order
            && self.max_position_notional > Decimal::ZERO
            && position_notional > position_limit
        {
            self.risk.force_halt(format!(
                "position notional {position_notional} exceeds grid-side limit {}",
                self.max_position_notional
            ));
        }
        self.push_event(format!(
            "filled {:?} {} @ {} realized={} pos={}→{}",
            fill.side, fill.size, fill.price, realized, position_before, self.position_base
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

        // No replenish while soft-breakout / recentering / paused / recovering.
        let allow_replenish = matches!(self.status, BotStatus::Running) && fully_filled;
        if !allow_replenish {
            return Ok((realized, None));
        }

        let step = self.step();
        let repl_side = fill.side.opposite();
        let repl_price = match repl_side {
            Side::Sell => (fill.price + step).min(self.active_upper),
            Side::Buy => (fill.price - step).max(self.active_lower),
        };

        // Always place the opposite grid order after a fill. Skipping on expand-budget
        // left permanent holes (open count drifts 20 → 19 → 17…).

        let mut reduce_budget = self.position_base.abs();
        let reduce_only =
            self.assign_reduce_only(repl_side, replenish_size, &mut reduce_budget);
        let client_id = new_order_id();
        let intent = OrderIntent {
            client_id: client_id.clone(),
            symbol: self.config.symbol.clone(),
            side: repl_side,
            price: repl_price.round_dp(8),
            size: replenish_size,
            level_index: fill.level_index,
            reduce_only,
            tif: TimeInForce::Gtc,
            cloid: Some(client_id.replace('-', "")),
        };
        Ok((realized, Some(intent)))
    }

    /// Build synthetic fills for local orders that vanished on the exchange.
    pub fn synthetic_fill_from_order(order: &LiveOrder) -> FillEvent {
        FillEvent {
            client_id: order.client_id.clone(),
            symbol: order.symbol.clone(),
            side: order.side,
            price: order.price,
            size: order.size,
            level_index: order.level_index,
            fee: Decimal::ZERO,
            fee_token: None,
            exchange_tid: None,
            exchange_oid: order.exchange_id.clone(),
            cloid: order.cloid.clone(),
            exchange_time_ms: Some(Utc::now().timestamp_millis()),
            crossed: true,
            dir: None,
            closed_pnl: None,
        }
    }

    /// Place missing grid levels when resting count drifts below target.
    pub fn repair_hole_intents(&self, mid: Decimal) -> GridResult<Vec<OrderIntent>> {
        if !matches!(self.status, BotStatus::Running) {
            return Ok(vec![]);
        }
        let target = if self.target_resting > 0 {
            self.target_resting
        } else {
            self.config.grid_count.saturating_sub(1).max(1) as usize
        };
        let have = self.open_orders.len();
        if have >= target || mid <= Decimal::ZERO {
            return Ok(vec![]);
        }
        let need = target - have;
        let levels = generate_levels_with_bounds(
            &self.config,
            mid,
            self.active_lower,
            self.active_upper,
        )?;
        let tol = self.step() * dec!(0.15);
        let mut intents = Vec::new();
        let mut reduce_budget = self.position_base.abs();
        for level in levels {
            if intents.len() >= need {
                break;
            }
            // Keep a one-step pocket around mid empty (normal inventory gap).
            if (level.price - mid).abs() <= tol {
                continue;
            }
            let occupied = self.open_orders.iter().any(|o| {
                o.side == level.side && (o.price - level.price).abs() <= tol
            });
            if occupied {
                continue;
            }
            if self.intent_is_expanding(level.side) {
                let notional = level.price * level.size;
                if self.max_position_notional > Decimal::ZERO
                    && notional > self.expand_budget_notional(level.side)
                {
                    continue;
                }
            }
            let reduce_only =
                self.assign_reduce_only(level.side, level.size, &mut reduce_budget);
            let client_id = new_order_id();
            intents.push(OrderIntent {
                client_id: client_id.clone(),
                symbol: self.config.symbol.clone(),
                side: level.side,
                price: level.price,
                size: level.size,
                level_index: level.index,
                reduce_only,
                tif: TimeInForce::Gtc,
                cloid: Some(client_id.replace('-', "")),
            });
        }
        Ok(intents)
    }

    /// Overwrite position from the exchange (source of truth for the dashboard).
    pub fn sync_position_from_exchange(
        &mut self,
        size: Decimal,
        entry: Option<Decimal>,
        unrealized: Option<Decimal>,
        liquidation_price: Option<Decimal>,
    ) {
        let prev = self.position_base;
        let delta = (prev - size).abs();
        self.position_base = size;
        if size == Decimal::ZERO {
            self.avg_entry = None;
            self.exchange_unrealized = Some(Decimal::ZERO);
            self.liquidation_price = None;
        } else {
            if let Some(px) = entry {
                self.avg_entry = Some(px);
            }
            self.exchange_unrealized = unrealized;
            self.liquidation_price = liquidation_price.filter(|px| *px > Decimal::ZERO);
        }
        if delta > Decimal::new(1, 6) {
            self.push_event(format!("position synced from exchange: {prev} → {size}"));
        }
    }

    pub fn sync_funding_pnl(&mut self, funding_pnl: Decimal) {
        self.funding_pnl = funding_pnl;
    }

    pub fn position_base(&self) -> Decimal {
        self.position_base
    }

    pub fn avg_entry(&self) -> Option<Decimal> {
        self.avg_entry
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
            self.liquidation_price = None;
        } else if (pos > Decimal::ZERO) != (self.position_base > Decimal::ZERO) {
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

    pub fn unrealized_pnl(&self) -> Decimal {
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

    /// Serializable state for SQLite checkpoints.
    pub fn checkpoint_payload(&self) -> serde_json::Value {
        serde_json::json!({
            "session_id": self.session_id,
            "status": self.status,
            "status_note": self.status_note,
            "active_lower": self.active_lower,
            "active_upper": self.active_upper,
            "max_position_notional": self.max_position_notional,
            "position_base": self.position_base,
            "avg_entry": self.avg_entry,
            "funding_pnl": self.funding_pnl,
            "fill_count": self.fill_count,
            "recenter_generation": self.recenter_generation,
            "recenters_today": self.recenters_today,
            "recenters_local_date": self.recenters_local_date,
            "last_recenter_ms": self.last_recenter_ms,
            "atr": self.atr,
            "atr_pct": self.atr_pct,
            "open_orders": self.open_orders,
            "target_resting": self.target_resting,
            "risk": self.risk,
            "config": self.config,
            "mode": self.mode,
            "last_tick_ms": self.last_tick_ms,
        })
    }

    pub fn restore_from_checkpoint(
        &mut self,
        payload: &serde_json::Value,
    ) -> Result<(), String> {
        if let Some(id) = payload.get("session_id").and_then(|v| v.as_str()) {
            self.session_id = id.to_string();
        }
        if let Ok(status) = serde_json::from_value::<BotStatus>(
            payload.get("status").cloned().unwrap_or(serde_json::Value::Null),
        ) {
            self.status = status;
        }
        self.status_note = payload
            .get("status_note")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if let Some(v) = payload.get("active_lower").and_then(|v| v.as_str()) {
            self.active_lower = v.parse().map_err(|e| format!("active_lower: {e}"))?;
        } else if let Some(v) = payload.get("active_lower") {
            self.active_lower = serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
        }
        if let Some(v) = payload.get("active_upper") {
            self.active_upper = serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
        }
        if let Some(v) = payload.get("max_position_notional") {
            self.max_position_notional =
                serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
        }
        if let Some(v) = payload.get("position_base") {
            self.position_base = serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
        }
        if let Some(v) = payload.get("avg_entry") {
            self.avg_entry = serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
        }
        if let Some(v) = payload.get("funding_pnl") {
            self.funding_pnl = serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
        }
        if let Some(v) = payload.get("fill_count").and_then(|v| v.as_u64()) {
            self.fill_count = v as usize;
        }
        if let Some(v) = payload.get("recenter_generation").and_then(|v| v.as_u64()) {
            self.recenter_generation = v as u32;
        }
        if let Some(v) = payload.get("recenters_today").and_then(|v| v.as_u64()) {
            self.recenters_today = v as u32;
        }
        if let Some(v) = payload.get("recenters_local_date").and_then(|v| v.as_str()) {
            self.recenters_local_date = v.to_string();
        }
        if let Some(v) = payload.get("last_recenter_ms").and_then(|v| v.as_i64()) {
            self.last_recenter_ms = Some(v);
        }
        if let Some(v) = payload.get("atr") {
            self.atr = serde_json::from_value(v.clone()).ok();
        }
        if let Some(v) = payload.get("atr_pct") {
            self.atr_pct = serde_json::from_value(v.clone()).ok();
        }
        if let Some(v) = payload.get("open_orders") {
            self.open_orders = serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
        }
        if let Some(v) = payload.get("target_resting").and_then(|v| v.as_u64()) {
            self.target_resting = v as usize;
        } else if self.target_resting == 0 {
            self.target_resting = self
                .open_orders
                .len()
                .max(self.config.grid_count.saturating_sub(1) as usize);
        }
        if let Some(v) = payload.get("risk") {
            self.risk = serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
        }
        self.push_event("restored from checkpoint");
        Ok(())
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
            status_note: self.status_note.clone(),
            mode: self.mode,
            symbol: self.config.symbol.clone(),
            mid_price: self.mid_price,
            open_orders: self.open_orders.len(),
            fill_count: self.fill_count,
            resting_orders,
            position_base: self.position_base.round_dp(8),
            avg_entry_price: self.avg_entry.map(|p| p.round_dp(8)),
            liquidation_price: self.liquidation_price.map(|p| p.round_dp(8)),
            realized_pnl: self.risk.realized_pnl.round_dp(8),
            unrealized_pnl: self.unrealized_pnl().round_dp(8),
            funding_pnl: self.funding_pnl.round_dp(8),
            events_tail: self.events.iter().rev().take(30).cloned().rev().collect(),
            active_lower: Some(self.active_lower.round_dp(8)),
            active_upper: Some(self.active_upper.round_dp(8)),
            atr: self.atr.map(|a| a.round_dp(8)),
            atr_pct: self.atr_pct.map(|a| a.round_dp(6)),
            recenter_generation: self.recenter_generation,
            recenters_today: self.recenters_today,
            last_recenter_ms: self.last_recenter_ms,
            session_id: Some(self.session_id.clone()),
            last_tick_ms: self.last_tick_ms,
            health_note: self.health_note.clone(),
            grid_mode: if self.config.is_dynamic() {
                GridMode::Dynamic
            } else {
                self.config.grid_mode
            },
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
            grid_mode: GridMode::Fixed,
            dynamic: crate::DynamicGridConfig::default(),
        }
    }

    #[test]
    fn bootstrap_perp_both_sides() {
        let mut engine = GridEngine::new(sample_cfg(), RunMode::Simulation, dec!(1000)).unwrap();
        let intents = engine.bootstrap_intents(dec!(95000)).unwrap();
        assert!(intents.iter().any(|i| i.side == Side::Buy));
        assert!(intents.iter().any(|i| i.side == Side::Sell));
        assert!(intents.iter().all(|i| i.cloid.is_some()));
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
            fee_token: None,
            exchange_tid: None,
            exchange_oid: None,
            cloid: None,
            exchange_time_ms: None,
            crossed: false,
            dir: None,
            closed_pnl: None,
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
            fee_token: None,
            exchange_tid: None,
            exchange_oid: None,
            cloid: None,
            exchange_time_ms: None,
            crossed: false,
            dir: None,
            closed_pnl: None,
        };
        let (pnl1, _) = engine.on_fill(buy).unwrap();
        assert!(pnl1 > Decimal::ZERO);
        assert_eq!(engine.snapshot().position_base, Decimal::ZERO);
    }

    #[test]
    fn fill_fee_is_deducted_from_realized_pnl() {
        let mut engine = GridEngine::new(sample_cfg(), RunMode::Simulation, dec!(1000)).unwrap();
        engine.bootstrap_intents(dec!(95000)).unwrap();
        let fill = FillEvent {
            client_id: "fee-fill".into(),
            symbol: "BTC".into(),
            side: Side::Buy,
            price: dec!(95000),
            size: dec!(0.001),
            level_index: 2,
            fee: dec!(0.25),
            fee_token: Some("USDC".into()),
            exchange_tid: None,
            exchange_oid: None,
            cloid: None,
            exchange_time_ms: None,
            crossed: false,
            dir: None,
            closed_pnl: None,
        };

        let (net_realized, _) = engine.on_fill(fill).unwrap();
        assert_eq!(net_realized, dec!(-0.25));
        assert_eq!(engine.snapshot().realized_pnl, dec!(-0.25));
    }

    #[test]
    fn buy_fill_replenishes_sell_above() {
        let mut cfg = sample_cfg();
        cfg.market = crate::MarketKind::Spot;
        let mut engine = GridEngine::new(cfg, RunMode::Simulation, dec!(1000)).unwrap();
        let intents = engine.bootstrap_intents(dec!(95000)).unwrap();
        let buy = intents[0].clone();
        engine.register_live_order(LiveOrder::from_intent(&buy, Some("t".into())));
        let fill = FillEvent {
            client_id: buy.client_id.clone(),
            symbol: buy.symbol.clone(),
            side: Side::Buy,
            price: buy.price,
            size: buy.size,
            level_index: buy.level_index,
            fee: Decimal::ZERO,
            fee_token: None,
            exchange_tid: None,
            exchange_oid: None,
            cloid: buy.cloid.clone(),
            exchange_time_ms: None,
            crossed: false,
            dir: None,
            closed_pnl: None,
        };
        let buy_price = buy.price;
        let buy_size = buy.size;
        let (_pnl, replenish) = engine.on_fill(fill).unwrap();
        let sell = replenish.expect("should place sell after buy");
        assert_eq!(sell.side, Side::Sell);
        assert!(sell.price > buy_price);
        assert_eq!(sell.size, buy_size);
        // Spot long after buy: sell size fits inventory → reduce-only.
        assert!(sell.reduce_only);
    }

    #[test]
    fn replenish_skips_reduce_only_when_size_would_flip() {
        let mut engine = GridEngine::new(sample_cfg(), RunMode::Simulation, dec!(1000)).unwrap();
        let intents = engine.bootstrap_intents(dec!(95000)).unwrap();
        let sell = intents
            .iter()
            .find(|i| i.side == Side::Sell)
            .cloned()
            .expect("sell");
        // Tiny long; filling a full-size sell flips short — opposite buy must not be RO.
        engine.position_base = sell.size / Decimal::from(10);
        engine.avg_entry = Some(sell.price);
        engine.register_live_order(LiveOrder::from_intent(&sell, Some("t".into())));
        let fill = FillEvent {
            client_id: sell.client_id.clone(),
            symbol: sell.symbol.clone(),
            side: Side::Sell,
            price: sell.price,
            size: sell.size,
            level_index: sell.level_index,
            fee: Decimal::ZERO,
            fee_token: None,
            exchange_tid: None,
            exchange_oid: None,
            cloid: sell.cloid.clone(),
            exchange_time_ms: None,
            crossed: false,
            dir: None,
            closed_pnl: None,
        };
        let (_pnl, replenish) = engine.on_fill(fill).unwrap();
        let buy = replenish.expect("buy replenish");
        assert_eq!(buy.side, Side::Buy);
        assert!(buy.size > engine.position_base.abs());
        assert!(
            !buy.reduce_only,
            "RO buy larger than short would be rejected by HL"
        );
    }

    #[test]
    fn soft_breakout_blocks_replenish() {
        let mut cfg = sample_cfg();
        cfg.breakout_action = BreakoutAction::Recenter;
        cfg.grid_mode = GridMode::Dynamic;
        cfg.dynamic.confirm_bars = 99;
        let mut engine = GridEngine::new(cfg, RunMode::Simulation, dec!(1000)).unwrap();
        let intents = engine.bootstrap_intents(dec!(95000)).unwrap();
        let buy = intents.iter().find(|i| i.side == Side::Buy).unwrap().clone();
        engine.register_live_order(LiveOrder::from_intent(&buy, None));
        let _ = engine.on_mid_price(dec!(89999));
        assert_eq!(engine.status, BotStatus::SoftBreakout);
        let fill = FillEvent {
            client_id: buy.client_id.clone(),
            symbol: buy.symbol.clone(),
            side: Side::Buy,
            price: buy.price,
            size: buy.size,
            level_index: buy.level_index,
            fee: Decimal::ZERO,
            fee_token: None,
            exchange_tid: None,
            exchange_oid: None,
            cloid: buy.cloid.clone(),
            exchange_time_ms: None,
            crossed: false,
            dir: None,
            closed_pnl: None,
        };
        let (_pnl, replenish) = engine.on_fill(fill).unwrap();
        assert!(replenish.is_none());
    }

    #[test]
    fn detach_keeps_orders_in_checkpoint() {
        let cfg = sample_cfg();
        let mut engine = GridEngine::new(cfg, RunMode::Simulation, dec!(1000)).unwrap();
        let intents = engine.bootstrap_intents(dec!(95000)).unwrap();
        for i in intents.iter().take(3) {
            engine.register_live_order(LiveOrder::from_intent(i, Some("1".into())));
        }
        assert_eq!(engine.live_orders().len(), 3);
        engine.mark_detached("app closed");
        let payload = engine.checkpoint_payload();
        assert_eq!(engine.status, BotStatus::Detached);
        let orders = payload.get("open_orders").unwrap().as_array().unwrap();
        assert_eq!(orders.len(), 3);
        let mut engine2 = GridEngine::new(sample_cfg(), RunMode::Simulation, dec!(1000)).unwrap();
        engine2.restore_from_checkpoint(&payload).unwrap();
        assert_eq!(engine2.live_orders().len(), 3);
    }

    #[test]
    fn recenter_preserves_max_position_notional() {
        let mut cfg = sample_cfg();
        cfg.breakout_action = BreakoutAction::Recenter;
        cfg.grid_mode = GridMode::Dynamic;
        cfg.dynamic.confirm_bars = 1;
        cfg.dynamic.recenter_cooldown_secs = 0;
        cfg.dynamic.max_recenters_per_day = 10;
        let mut engine = GridEngine::new(cfg, RunMode::Simulation, dec!(1000)).unwrap();
        engine.bootstrap_intents(dec!(95000)).unwrap();
        let cap = engine.max_position_notional();
        assert!(cap > Decimal::ZERO);
        engine.atr_pct = Some(dec!(5));
        engine.note_outside_confirm_bar(true);
        for _ in 0..60 {
            let ev = engine.on_mid_price(dec!(100001));
            if ev.iter().any(|e| matches!(e, EngineEvent::RecenterRequested { .. })) {
                break;
            }
        }
        let plan = engine.plan_recenter(dec!(100500)).unwrap();
        engine.commit_recenter(&plan);
        assert_eq!(engine.max_position_notional(), cap);
        assert_eq!(engine.status, BotStatus::Running);
        assert!(engine.active_lower > dec!(90000) || engine.active_upper > dec!(100000));
    }

    #[test]
    fn protective_breakout_requests_cancel_close_once() {
        let mut cfg = sample_cfg();
        cfg.breakout_action = BreakoutAction::CancelCloseAndStop;
        let mut engine = GridEngine::new(cfg, RunMode::Simulation, dec!(1000)).unwrap();
        engine.bootstrap_intents(dec!(95000)).unwrap();

        let first = engine.on_mid_price(dec!(100001));
        assert!(matches!(
            first.as_slice(),
            [
                EngineEvent::Breakout { .. },
                EngineEvent::ProtectiveExitRequested {
                    close_position: true,
                    ..
                }
            ]
        ));
        assert_eq!(engine.status, BotStatus::ProtectiveExit);
        assert!(engine.on_mid_price(dec!(100002)).is_empty());
    }

    #[test]
    fn bootstrap_rejects_mid_outside_range() {
        let mut engine = GridEngine::new(sample_cfg(), RunMode::Simulation, dec!(1000)).unwrap();
        assert!(engine.bootstrap_intents(dec!(100001)).is_err());
        assert_eq!(engine.status, BotStatus::Idle);
    }

    #[test]
    fn breakout_stop_requires_fresh_start() {
        let mut engine = GridEngine::new(sample_cfg(), RunMode::Simulation, dec!(1000)).unwrap();
        engine.bootstrap_intents(dec!(95000)).unwrap();
        engine.status = BotStatus::ProtectiveExit;
        engine.mark_breakout_stopped();

        assert_eq!(engine.status, BotStatus::BreakoutStopped);
        engine.resume().unwrap();
        assert_eq!(engine.status, BotStatus::BreakoutStopped);
        assert!(engine.live_orders().is_empty());
    }

    #[test]
    fn paused_still_runs_risk_check() {
        let mut cfg = sample_cfg();
        cfg.max_drawdown_pct = dec!(5);
        cfg.max_daily_loss = Decimal::ZERO;
        let mut engine = GridEngine::new(cfg, RunMode::Simulation, dec!(1000)).unwrap();
        engine.bootstrap_intents(dec!(95000)).unwrap();
        engine.position_base = dec!(1);
        engine.avg_entry = Some(dec!(95000));
        engine.pause();
        let ev = engine.on_mid_price(dec!(80000));
        assert!(ev.iter().any(|e| matches!(
            e,
            EngineEvent::ProtectiveExitRequested {
                risk_triggered: true,
                ..
            }
        )));
    }

    #[test]
    fn checkpoint_roundtrip() {
        let mut engine = GridEngine::new(sample_cfg(), RunMode::Simulation, dec!(1000)).unwrap();
        engine.bootstrap_intents(dec!(95000)).unwrap();
        engine.position_base = dec!(0.01);
        engine.avg_entry = Some(dec!(94000));
        let payload = engine.checkpoint_payload();
        let mut engine2 = GridEngine::new(sample_cfg(), RunMode::Simulation, dec!(1000)).unwrap();
        engine2.restore_from_checkpoint(&payload).unwrap();
        assert_eq!(engine2.position_base(), dec!(0.01));
        assert_eq!(engine2.session_id(), engine.session_id());
    }
}

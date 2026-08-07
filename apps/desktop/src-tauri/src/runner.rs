//! Recoverable bot tick / recenter / halt helpers for the desktop runner.

/// Main loop period. Keep >= 2.5s to stay under Hyperliquid info rate limits
/// (each tick may call mid + fills; position/ATR/funding are further throttled).
const TICK_INTERVAL_MS: u64 = 3000;
/// Sync exchange position every N ticks (~9s at 3s/tick).
const POSITION_SYNC_EVERY_TICKS: u32 = 3;
/// Sync open orders from exchange every N ticks (~15s) to drop phantoms.
const ORDER_SYNC_EVERY_TICKS: u32 = 5;
/// Refresh ATR / funding about once per minute.
const SLOW_POLL_EVERY_TICKS: u32 = 20;
/// Persist equity/checkpoint about every 30s.
const CHECKPOINT_EVERY_TICKS: u32 = 10;

use exchange::{fetch_candles, CandleInterval, Exchange, HyperliquidExchange};
use grid_engine::{
    compute_atr, derive_bounds, suggest_half_width_pct, AtrMetrics, BotSnapshot, BotStatus,
    BreakoutAction, DynamicGridConfig, EngineEvent, GridConfig, GridEngine, GridMode, MarketKind,
    OhlcBar, RunMode,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use storage::{AppConfig, EquitySnapshotRow, FillLedgerRow, FundingRow, Storage};
use tauri::{AppHandle, Emitter};
use tracing::{error, info, warn};

use crate::i18n_err::{i18n, i18n_kv};
use crate::AppState;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartRequest {
    pub symbol: String,
    pub lower_price: String,
    pub upper_price: String,
    pub grid_count: u32,
    pub total_budget: String,
    pub spacing: String,
    pub breakout_action: String,
    pub max_drawdown_pct: String,
    pub max_daily_loss: String,
    pub max_order_failures: u32,
    #[serde(default = "default_leverage")]
    pub leverage: u32,
    #[serde(default = "default_cross")]
    pub is_cross: bool,
    #[serde(default)]
    pub grid_mode: Option<String>,
    #[serde(default)]
    pub atr_interval: Option<String>,
    #[serde(default)]
    pub atr_period: Option<u32>,
    #[serde(default)]
    pub atr_mult: Option<String>,
    #[serde(default)]
    pub confirm_bars: Option<u32>,
    #[serde(default)]
    pub recenter_cooldown_secs: Option<u64>,
    #[serde(default)]
    pub max_recenters_per_day: Option<u32>,
}

fn default_leverage() -> u32 {
    5
}
fn default_cross() -> bool {
    true
}

pub fn dec(s: &str) -> Result<Decimal, String> {
    s.parse::<Decimal>().map_err(|e| e.to_string())
}

pub fn spacing(s: &str) -> grid_engine::GridSpacing {
    if s == "geometric" {
        grid_engine::GridSpacing::Geometric
    } else {
        grid_engine::GridSpacing::Arithmetic
    }
}

pub fn breakout(s: &str) -> BreakoutAction {
    match s {
        "alert_only" => BreakoutAction::AlertOnly,
        "pause" => BreakoutAction::Pause,
        "cancel_and_pause" => BreakoutAction::CancelAndPause,
        "cancel_close_and_stop" => BreakoutAction::CancelCloseAndStop,
        "recenter" => BreakoutAction::Recenter,
        _ => BreakoutAction::CancelCloseAndStop,
    }
}

pub fn grid_mode(s: &str) -> GridMode {
    if s.eq_ignore_ascii_case("dynamic") {
        GridMode::Dynamic
    } else {
        GridMode::Fixed
    }
}

pub fn idle_snapshot(mode: RunMode, symbol: impl Into<String>) -> BotSnapshot {
    BotSnapshot {
        status: BotStatus::Idle,
        status_note: None,
        mode,
        symbol: symbol.into(),
        mid_price: None,
        open_orders: 0,
        fill_count: 0,
        resting_orders: vec![],
        position_base: Decimal::ZERO,
        avg_entry_price: None,
        liquidation_price: None,
        realized_pnl: Decimal::ZERO,
        unrealized_pnl: Decimal::ZERO,
        funding_pnl: Decimal::ZERO,
        events_tail: vec![],
        active_lower: None,
        active_upper: None,
        atr: None,
        atr_pct: None,
        recenter_generation: 0,
        recenters_today: 0,
        last_recenter_ms: None,
        session_id: None,
        last_tick_ms: None,
        health_note: None,
        grid_mode: GridMode::Fixed,
    }
}

pub fn dynamic_from_app(cfg: &AppConfig) -> DynamicGridConfig {
    let mut d = DynamicGridConfig::default();
    if !cfg.atr_interval.is_empty() {
        d.atr_interval = cfg.atr_interval.clone();
    }
    if cfg.atr_period > 0 {
        d.atr_period = cfg.atr_period;
    }
    if let Ok(m) = cfg.atr_mult.parse::<Decimal>() {
        d.atr_mult = m;
    }
    if cfg.confirm_bars > 0 {
        d.confirm_bars = cfg.confirm_bars;
    }
    if cfg.recenter_cooldown_secs > 0 {
        d.recenter_cooldown_secs = cfg.recenter_cooldown_secs;
    }
    if cfg.max_recenters_per_day > 0 {
        d.max_recenters_per_day = cfg.max_recenters_per_day;
    }
    d
}

pub fn build_grid_config(req: &StartRequest, app_cfg: &AppConfig) -> Result<GridConfig, String> {
    let mode_str = req
        .grid_mode
        .as_deref()
        .unwrap_or(&app_cfg.grid_mode);
    let mut dynamic = dynamic_from_app(app_cfg);
    if let Some(v) = &req.atr_interval {
        if !v.is_empty() {
            dynamic.atr_interval = v.clone();
        }
    }
    if let Some(v) = req.atr_period {
        dynamic.atr_period = v;
    }
    if let Some(v) = &req.atr_mult {
        if let Ok(m) = v.parse::<Decimal>() {
            dynamic.atr_mult = m;
        }
    }
    if let Some(v) = req.confirm_bars {
        dynamic.confirm_bars = v;
    }
    if let Some(v) = req.recenter_cooldown_secs {
        dynamic.recenter_cooldown_secs = v;
    }
    if let Some(v) = req.max_recenters_per_day {
        dynamic.max_recenters_per_day = v;
    }

    let mut breakout_action = breakout(&req.breakout_action);
    let gm = grid_mode(mode_str);
    if matches!(gm, GridMode::Dynamic) && matches!(breakout_action, BreakoutAction::CancelCloseAndStop)
    {
        // Dynamic grids default to recenter instead of hard stop-on-breakout.
        breakout_action = BreakoutAction::Recenter;
    }

    Ok(GridConfig {
        symbol: req.symbol.clone(),
        lower_price: dec(&req.lower_price).unwrap_or(Decimal::ZERO),
        upper_price: dec(&req.upper_price).unwrap_or(Decimal::ZERO),
        grid_count: req.grid_count,
        total_budget: dec(&req.total_budget)?,
        spacing: spacing(&req.spacing),
        breakout_action,
        max_drawdown_pct: dec(&req.max_drawdown_pct).unwrap_or(Decimal::ZERO),
        max_daily_loss: dec(&req.max_daily_loss).unwrap_or(Decimal::ZERO),
        max_order_failures: req.max_order_failures,
        market: MarketKind::Perp,
        leverage: req.leverage.max(1).min(50),
        is_cross: req.is_cross,
        grid_mode: gm,
        dynamic,
    })
}

pub async fn resolve_dynamic_bounds(
    mode: RunMode,
    config: &mut GridConfig,
    mid: Decimal,
) -> Result<Option<AtrMetrics>, String> {
    if !config.is_dynamic() {
        return Ok(None);
    }
    let interval = CandleInterval::parse(&config.dynamic.atr_interval)
        .ok_or_else(|| format!("unsupported ATR interval {}", config.dynamic.atr_interval))?;
    let need = (config.dynamic.atr_period as usize) + 5;
    let candles = fetch_candles(mode, &config.symbol, interval, need.max(40))
        .await
        .map_err(|e| e.to_string())?;
    let bars: Vec<OhlcBar> = candles
        .into_iter()
        .filter_map(|c| {
            Some(OhlcBar {
                time: c.time,
                open: c.open.parse().ok()?,
                high: c.high.parse().ok()?,
                low: c.low.parse().ok()?,
                close: c.close.parse().ok()?,
            })
        })
        .collect();
    let metrics = compute_atr(&bars, config.dynamic.atr_period).map_err(|e| e.to_string())?;
    let half = suggest_half_width_pct(metrics.atr_pct, config.dynamic.atr_mult)
        .max(config.dynamic.min_half_width_pct)
        .min(config.dynamic.max_half_width_pct);
    let (lower, upper) = derive_bounds(mid, half).map_err(|e| e.to_string())?;
    config.lower_price = lower;
    config.upper_price = upper;
    Ok(Some(metrics))
}

/// Persist bot status as serde snake_case (`running`, `idle`, …).
pub fn bot_status_key(status: BotStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{status:?}").to_lowercase())
}

pub fn persist_checkpoint(storage: &Storage, engine: &GridEngine, phase: &str) {
    let sid = engine.session_id().to_string();
    let payload = engine.checkpoint_payload();
    if let Err(e) = storage.save_checkpoint(&sid, phase, &payload) {
        warn!("checkpoint save failed: {e}");
    }
    let cfg_json = serde_json::to_string(&engine.config).unwrap_or_else(|_| "{}".into());
    let status = bot_status_key(engine.snapshot().status);
    if let Err(e) = storage.upsert_bot_session(
        &sid,
        "grid",
        &engine.config.symbol,
        &status,
        &cfg_json,
        true,
    ) {
        warn!("session upsert failed: {e}");
    }
}

pub fn record_fill_ledger(storage: &Storage, session_id: &str, fill: &grid_engine::FillEvent, pnl: Decimal) {
    // `pnl` is net of fees from the engine; ledger stores gross separately.
    let gross = pnl + fill.fee;
    let notional = fill.price * fill.size;
    let row = FillLedgerRow {
        session_id: session_id.to_string(),
        strategy_id: "grid".into(),
        exchange_tid: fill.exchange_tid.clone(),
        exchange_oid: fill.exchange_oid.clone(),
        cloid: fill.cloid.clone(),
        client_id: fill.client_id.clone(),
        exchange_time_ms: fill.exchange_time_ms,
        symbol: fill.symbol.clone(),
        side: format!("{:?}", fill.side).to_ascii_lowercase(),
        direction: fill.dir.clone(),
        price: fill.price,
        size: fill.size,
        notional,
        crossed: fill.crossed,
        fee: fill.fee,
        fee_token: fill.fee_token.clone(),
        gross_closed_pnl: gross,
        position_before: None,
        position_after: None,
        source: "exchange".into(),
    };
    if let Err(e) = storage.record_fill_ledger(&row) {
        warn!("fill ledger: {e}");
    }
}

fn sort_fills_chronologically(fills: &mut [grid_engine::FillEvent]) {
    fills.sort_by_key(|f| f.exchange_time_ms.unwrap_or(0));
}

/// Apply exchange fills in time order. Returns replenish intents from successful fills.
fn apply_fills_to_engine(
    storage: &Storage,
    engine: &mut GridEngine,
    session_id: &str,
    mut fills: Vec<grid_engine::FillEvent>,
    record_ledger: bool,
) -> Vec<grid_engine::OrderIntent> {
    sort_fills_chronologically(&mut fills);
    let mut replenish = Vec::new();
    for fill in fills {
        match engine.on_fill(fill.clone()) {
            Ok((pnl, intent)) => {
                if record_ledger {
                    record_fill_ledger(storage, session_id, &fill, pnl);
                }
                if let Some(i) = intent {
                    replenish.push(i);
                }
            }
            Err(e) => warn!("fill apply failed: {e}"),
        }
    }
    replenish
}

pub fn record_equity(storage: &Storage, engine: &GridEngine) {
    let snap = engine.snapshot();
    let sid = engine.session_id().to_string();
    let fees = storage
        .session_fees_cum(&sid)
        .unwrap_or(Decimal::ZERO);
    // realized already nets fees; mark-to-market curve = realized + unrealized + funding
    let net = snap.realized_pnl + snap.unrealized_pnl + snap.funding_pnl;
    let row = EquitySnapshotRow {
        session_id: sid,
        strategy_id: "grid".into(),
        ts_ms: chrono::Utc::now().timestamp_millis(),
        realized_pnl: snap.realized_pnl,
        unrealized_pnl: snap.unrealized_pnl,
        fees_cum: fees,
        funding_cum: snap.funding_pnl,
        net_pnl: net,
        position_base: snap.position_base,
        avg_entry: snap.avg_entry_price,
        mark: snap.mid_price,
        liquidation_price: snap.liquidation_price,
        account_equity: None,
        margin_used: None,
    };
    let _ = storage.record_equity_snapshot(&row);
}

/// Cancel only this strategy symbol and optionally close its position.
pub async fn protect_symbol(
    st: &mut AppState,
    symbol: &str,
    close_position: bool,
) -> Result<(), String> {
    if st.mode == RunMode::Simulation {
        let sim = st.sim.as_mut().ok_or("simulation exchange unavailable")?;
        let report = sim
            .cancel_all_confirmed(symbol, 5)
            .await
            .map_err(|e| e.to_string())?;
        if !report.confirmed_flat {
            return Err(i18n_kv(
                "simOrdersRemain",
                &[("symbol", symbol.to_string())],
            ));
        }
        if close_position {
            sim.close_position(symbol)
                .await
                .map_err(|e| e.to_string())?;
            if sim.position_size().await != Decimal::ZERO {
                return Err(i18n_kv(
                    "simPositionRemain",
                    &[("symbol", symbol.to_string())],
                ));
            }
        }
        return Ok(());
    }

    let hl = st.hl.as_mut().ok_or("Hyperliquid exchange unavailable")?;
    let report = hl
        .cancel_all_confirmed(symbol, 8)
        .await
        .map_err(|e| e.to_string())?;
    if !report.confirmed_flat {
        return Err(i18n_kv(
            "exchangeOrdersRemain",
            &[("symbol", symbol.to_string())],
        ));
    }
    if close_position {
        hl.close_position(symbol).await.map_err(|e| e.to_string())?;
        let (remaining, _, _, _) = hl
            .get_perp_position(symbol)
            .await
            .map_err(|e| e.to_string())?;
        if remaining != Decimal::ZERO {
            return Err(i18n_kv(
                "exchangePositionRemain",
                &[
                    ("symbol", symbol.to_string()),
                    ("remaining", remaining.to_string()),
                ],
            ));
        }
    }
    Ok(())
}

pub async fn handle_hard_halt(
    app: &AppHandle,
    st: &mut AppState,
    symbol: &str,
    reason: &str,
    close_position: bool,
) {
    st.running_task = false;
    match protect_symbol(st, symbol, close_position).await {
        Ok(()) => {
            if let Some(engine) = st.engine.as_mut() {
                if close_position {
                    engine.mark_risk_stopped(reason);
                } else {
                    engine.mark_orders_canceled_and_paused();
                }
                persist_checkpoint(&st.storage, engine, "halted");
                let status = bot_status_key(engine.snapshot().status);
                let _ = st
                    .storage
                    .deactivate_session(engine.session_id(), Some(&status));
                let _ = app.emit("bot-status", &engine.snapshot());
            }
            let _ = st.storage.record_event("hard_halt", reason);
            let _ = app.emit(
                "bot-alert",
                serde_json::json!({ "kind": "hard_halt", "reason": reason }),
            );
        }
        Err(e) => {
            if let Some(engine) = st.engine.as_mut() {
                engine.mark_protective_exit_failed(&e);
                persist_checkpoint(&st.storage, engine, "halt_failed");
                let _ = app.emit("bot-status", &engine.snapshot());
            }
            let _ = st.storage.record_event(
                "hard_halt_failed",
                &format!("{reason}; protect failed: {e}"),
            );
            let _ = app.emit(
                "bot-alert",
                serde_json::json!({
                    "kind": "hard_halt_failed",
                    "reason": format!("{reason}; {e}")
                }),
            );
        }
    }
}

pub async fn execute_recenter(
    app: &AppHandle,
    st: &mut AppState,
    mid: Decimal,
) -> Result<(), String> {
    let symbol = st
        .engine
        .as_ref()
        .map(|e| e.config.symbol.clone())
        .ok_or("no engine")?;
    let session_id = st
        .engine
        .as_ref()
        .map(|e| e.session_id().to_string())
        .unwrap_or_default();

    // Refresh ATR before planning when possible.
    let mode = st.mode;
    if let Some(engine) = st.engine.as_mut() {
        if engine.config.is_dynamic() {
            let _ = refresh_atr(mode, engine).await;
        }
    }

    let plan = {
        let engine = st.engine.as_mut().ok_or("no engine")?;
        engine
            .plan_recenter(mid)
            .map_err(|e| format!("plan_recenter: {e}"))?
    };

    let intent_json = serde_json::to_string(&serde_json::json!({
        "generation": plan.generation,
        "lower": plan.lower.to_string(),
        "upper": plan.upper.to_string(),
        "mid": plan.mid.to_string(),
        "intent_count": plan.intents.len(),
    }))
    .unwrap_or_else(|_| "{}".into());
    let op_id = st
        .storage
        .begin_recenter_op(&session_id, plan.generation, &intent_json)
        .map_err(|e| e.to_string())?;

    if let Some(engine) = st.engine.as_ref() {
        persist_checkpoint(&st.storage, engine, "recenter_intent");
        let _ = st.storage.save_order_snapshot(
            &symbol,
            &serde_json::to_string(&engine.live_orders()).unwrap_or_default(),
        );
    }

    // 1) Cancel strategy symbol orders with confirmation.
    let cancel_ok = if st.mode == RunMode::Simulation {
        let sim = st.sim.as_mut().ok_or("no sim")?;
        let report = sim
            .cancel_all_confirmed(&symbol, 5)
            .await
            .map_err(|e| e.to_string())?;
        report.confirmed_flat
    } else {
        let hl = st.hl.as_mut().ok_or("no hl")?;
        let report = hl
            .cancel_all_confirmed(&symbol, 8)
            .await
            .map_err(|e| e.to_string())?;
        report.confirmed_flat
    };
    if !cancel_ok {
        let _ = st
            .storage
            .complete_recenter_op(op_id, "failed", r#"{"error":"cancel not confirmed"}"#);
        return Err("recenter cancel not confirmed".into());
    }
    if let Some(engine) = st.engine.as_mut() {
        engine.open_orders.clear();
        persist_checkpoint(&st.storage, engine, "recenter_canceled");
    }

    // 2) Sync late fills + position.
    let _ = sync_fills_and_position(app, st, &symbol, &session_id).await;

    // 3) Place inventory-aware intents.
    let intents = plan.intents.clone();
    if !intents.is_empty() {
        if st.mode != RunMode::Simulation {
            if let Some(hl) = st.hl.as_mut() {
                let lev = st
                    .engine
                    .as_ref()
                    .map(|e| e.config.leverage)
                    .unwrap_or(5);
                if let Err(e) = hl.preflight_grid_notional(&intents, lev).await {
                    let _ = st.storage.complete_recenter_op(
                        op_id,
                        "failed",
                        &format!(r#"{{"error":"{e}"}}"#),
                    );
                    return Err(e.to_string());
                }
            }
        }
        let placed = if st.mode == RunMode::Simulation {
            st.sim
                .as_mut()
                .unwrap()
                .place_orders(intents)
                .await
                .map_err(|e| e.to_string())?
        } else {
            st.hl
                .as_mut()
                .unwrap()
                .place_orders(intents)
                .await
                .map_err(|e| e.to_string())?
        };
        if let Some(engine) = st.engine.as_mut() {
            for order in placed {
                engine.register_live_order(order);
            }
        }
    }

    // 4) Commit + reconcile.
    if let Some(engine) = st.engine.as_mut() {
        engine.commit_recenter(&plan);
        persist_checkpoint(&st.storage, engine, "recenter_committed");
    }
    if let Err(e) = reconcile_orders(st, &symbol).await {
        // Integrity failure: cancel best-effort, keep position, halt.
        warn!("post-recenter reconcile failed: {e}");
        let _ = protect_symbol(st, &symbol, false).await;
        if let Some(engine) = st.engine.as_mut() {
            engine.halt_integrity(&e);
            persist_checkpoint(&st.storage, engine, "integrity_halt");
            let status = bot_status_key(engine.snapshot().status);
            let _ = st
                .storage
                .deactivate_session(engine.session_id(), Some(&status));
            let _ = app.emit("bot-status", &engine.snapshot());
        }
        st.running_task = false;
        let _ = st.storage.complete_recenter_op(
            op_id,
            "failed",
            &format!(r#"{{"error":"{e}"}}"#),
        );
        let _ = app.emit(
            "bot-alert",
            serde_json::json!({ "kind": "integrity_halt", "reason": e }),
        );
        return Err(e);
    }

    let _ = st.storage.complete_recenter_op(
        op_id,
        "committed",
        &serde_json::json!({
            "lower": plan.lower.to_string(),
            "upper": plan.upper.to_string(),
            "generation": plan.generation,
        })
        .to_string(),
    );
    let _ = st
        .storage
        .record_event("recenter", &format!("gen={} {}–{}", plan.generation, plan.lower, plan.upper));
    if let Some(engine) = st.engine.as_ref() {
        let _ = app.emit("bot-status", &engine.snapshot());
    }
    Ok(())
}

/// Sync engine (+ HL cache) open orders from exchange.
/// Stale local orders missing on the exchange are dropped (not simulated as fills).
/// Missing grid levels are repaired only when the book is fully in sync.
async fn sync_open_orders_from_exchange(
    st: &mut AppState,
    symbol: &str,
) -> Result<Vec<grid_engine::OrderIntent>, String> {
    let exchange = if st.mode == RunMode::Simulation {
        st.sim
            .as_mut()
            .ok_or("no sim")?
            .list_exchange_open_orders(symbol)
            .await
            .map_err(|e| e.to_string())?
    } else {
        st.hl
            .as_mut()
            .ok_or("no hl")?
            .list_exchange_open_orders(symbol)
            .await
            .map_err(|e| e.to_string())?
    };

    let Some(engine) = st.engine.as_mut() else {
        return Ok(vec![]);
    };
    let session_id = engine.session_id().to_string();

    // Drain exchange fills we may have missed before inferring phantoms.
    let mut replenish = if st.mode == RunMode::Simulation {
        vec![]
    } else {
        let live = engine.live_orders().to_vec();
        if let Some(hl) = st.hl.as_mut() {
            hl.restore_tracked_orders(&live);
        }
        let fills = st
            .hl
            .as_mut()
            .unwrap()
            .drain_fills()
            .await
            .unwrap_or_default();
        apply_fills_to_engine(&st.storage, engine, &session_id, fills, true)
    };

    let local = engine.live_orders().to_vec();

    // Exchange says no open orders but we still track locals → desync (2nd instance,
    // API glitch). Do not phantom-fill, replace with empty, or repair — that triggers
    // a burst of replenish orders (see repair_hole after replace_open_orders([])).
    if exchange.is_empty() && !local.is_empty() {
        warn!(
            "open-order sync {symbol}: exchange=0 local={} — skipping sync (desync)",
            local.len()
        );
        let _ = st.storage.record_event(
            "order_sync_desync",
            &format!("{symbol}: exchange=0 local={} sync_skipped", local.len()),
        );
        return Ok(replenish);
    }

    let local_only: Vec<_> = local
        .iter()
        .filter(|l| !exchange.iter().any(|e| orders_same(l, e)))
        .cloned()
        .collect();
    for phantom in &local_only {
        // Drop stale local entries only — never simulate fills or replenish from phantoms.
        warn!(
            "local order missing on exchange (not a fill): {:?} {} @ {}",
            phantom.side, phantom.size, phantom.price
        );
        let _ = st.storage.record_event(
            "phantom_drop",
            &format!("{:?} {} @ {}", phantom.side, phantom.size, phantom.price),
        );
    }

    let local_after = engine.live_orders().to_vec();
    let mut merged = Vec::with_capacity(exchange.len());
    for mut exo in exchange {
        if let Some(lo) = local_after.iter().find(|l| orders_same(l, &exo)) {
            exo.client_id = lo.client_id.clone();
            exo.level_index = lo.level_index;
            exo.reduce_only = lo.reduce_only;
            if lo.orig_size > Decimal::ZERO {
                exo.orig_size = lo.orig_size;
            }
            if exo.size <= Decimal::ZERO && lo.size > Decimal::ZERO {
                exo.size = lo.size;
            }
        }
        merged.push(exo);
    }

    let before = local.len();
    let after = merged.len();
    if before != after || !local_only.is_empty() {
        warn!(
            "open-order sync {symbol}: local={before} exchange={after} phantoms={}",
            local_only.len()
        );
        let _ = st.storage.record_event(
            "order_sync",
            &format!(
                "{symbol}: local={before} → exchange={after} phantoms={}",
                local_only.len()
            ),
        );
    }

    engine.replace_open_orders(merged.clone());
    if engine.target_resting == 0 {
        engine.target_resting = after
            .max(engine.config.grid_count.saturating_sub(1) as usize);
    }
    let mid = engine
        .mid_price
        .unwrap_or_else(|| (engine.active_bounds().0 + engine.active_bounds().1) / Decimal::from(2));
    // Only repair when exchange confirmed open orders — never fill the whole grid in one
    // burst after a partial desync (local_only non-empty but exchange had some orders).
    if local_only.is_empty() {
        match engine.repair_hole_intents(mid) {
            Ok(extra) if !extra.is_empty() => {
                info!(
                    "repairing {} missing grid level(s); open={} target={}",
                    extra.len(),
                    engine.live_orders().len(),
                    engine.target_resting
                );
                replenish.extend(extra);
            }
            Ok(_) => {}
            Err(e) => warn!("repair_hole_intents: {e}"),
        }
    } else if !local_only.is_empty() {
        warn!(
            "open-order sync {symbol}: {} local-only order(s) dropped; skipping repair this tick",
            local_only.len()
        );
    }

    if let Some(hl) = st.hl.as_mut() {
        hl.adopt_open_orders(symbol, &merged);
    }
    Ok(replenish)
}

fn orders_same(local: &grid_engine::LiveOrder, exo: &grid_engine::LiveOrder) -> bool {
    match (&local.exchange_id, &exo.exchange_id) {
        (Some(a), Some(b)) if a == b => return true,
        _ => {}
    }
    match (&local.cloid, &exo.cloid) {
        (Some(a), Some(b)) if !a.is_empty() && a == b => return true,
        _ => {}
    }
    local.side == exo.side
        && local.price == exo.price
        && (local.size - exo.size).abs() <= Decimal::new(1, 8)
}

async fn place_and_register_intents(
    app: &AppHandle,
    st: &mut AppState,
    symbol: &str,
    intents: Vec<grid_engine::OrderIntent>,
) -> Result<(), String> {
    if intents.is_empty() {
        return Ok(());
    }
    let mut intents = intents;
    let placed = place_replenish(st, intents.clone()).await;
    let placed = match placed {
        Err(e) if is_reduce_only_increase_error(&e) => {
            warn!("repair/replenish reduce_only rejected ({e}); retrying without reduce_only");
            for intent in &mut intents {
                intent.reduce_only = false;
            }
            place_replenish(st, intents).await
        }
        other => other,
    };
    match placed {
        Ok(orders) => {
            for order in orders {
                st.engine
                    .as_mut()
                    .unwrap()
                    .register_live_order(order.clone());
                let _ = app.emit("bot-event", &EngineEvent::OrderPlaced { order });
            }
            Ok(())
        }
        Err(e) => {
            warn!("{symbol} place intents failed: {e}");
            Err(e)
        }
    }
}

async fn reconcile_orders(st: &mut AppState, symbol: &str) -> Result<(), String> {
    // Exchange is source of truth — prune phantoms / adopt missing.
    let _ = sync_open_orders_from_exchange(st, symbol).await?;
    let local_len = st
        .engine
        .as_ref()
        .map(|e| e.live_orders().len())
        .unwrap_or(0);
    let exchange_len = if st.mode == RunMode::Simulation {
        st.sim
            .as_mut()
            .unwrap()
            .list_exchange_open_orders(symbol)
            .await
            .map_err(|e| e.to_string())?
            .len()
    } else {
        st.hl
            .as_mut()
            .unwrap()
            .list_exchange_open_orders(symbol)
            .await
            .map_err(|e| e.to_string())?
            .len()
    };
    if exchange_len > local_len.saturating_add(2) {
        return Err(format!(
            "order reconcile mismatch: exchange={exchange_len} local={local_len}"
        ));
    }
    Ok(())
}

async fn refresh_atr(mode: RunMode, engine: &mut GridEngine) -> Result<(), String> {
    let interval = CandleInterval::parse(&engine.config.dynamic.atr_interval)
        .ok_or_else(|| "bad atr interval".to_string())?;
    let need = (engine.config.dynamic.atr_period as usize) + 5;
    let candles = fetch_candles(mode, &engine.config.symbol, interval, need.max(40))
        .await
        .map_err(|e| e.to_string())?;
    let bars: Vec<OhlcBar> = candles
        .into_iter()
        .filter_map(|c| {
            Some(OhlcBar {
                time: c.time,
                open: c.open.parse().ok()?,
                high: c.high.parse().ok()?,
                low: c.low.parse().ok()?,
                close: c.close.parse().ok()?,
            })
        })
        .collect();
    if let Some(last) = bars.last() {
        let outside = last.close < engine.snapshot().active_lower.unwrap_or(Decimal::ZERO)
            || last.close > engine.snapshot().active_upper.unwrap_or(Decimal::MAX);
        engine.note_outside_confirm_bar(outside);
    }
    let metrics = compute_atr(&bars, engine.config.dynamic.atr_period).map_err(|e| e.to_string())?;
    engine.set_atr(&metrics);
    Ok(())
}

async fn sync_fills_and_position(
    app: &AppHandle,
    st: &mut AppState,
    symbol: &str,
    session_id: &str,
) -> Result<(), String> {
    let mut fills = if st.mode == RunMode::Simulation {
        st.sim
            .as_mut()
            .unwrap()
            .drain_fills()
            .await
            .unwrap_or_default()
    } else {
        let live = st
            .engine
            .as_ref()
            .map(|e| e.live_orders().to_vec())
            .unwrap_or_default();
        if let Some(hl) = st.hl.as_mut() {
            hl.restore_tracked_orders(&live);
        }
        st.hl
            .as_mut()
            .unwrap()
            .drain_fills()
            .await
            .unwrap_or_default()
    };
    sort_fills_chronologically(&mut fills);
    for fill in fills {
        if let Some(engine) = st.engine.as_mut() {
            match engine.on_fill(fill.clone()) {
                Ok((pnl, _)) => {
                    record_fill_ledger(&st.storage, session_id, &fill, pnl);
                    let _ = app.emit(
                        "bot-event",
                        &EngineEvent::Filled {
                            fill,
                            realized_pnl: pnl,
                        },
                    );
                }
                Err(e) => warn!("late fill apply: {e}"),
            }
        }
    }
    if st.mode != RunMode::Simulation {
        if let Some(hl) = st.hl.as_mut() {
            if let Ok((size, entry, upnl, liq)) = hl.get_perp_position(symbol).await {
                if let Some(engine) = st.engine.as_mut() {
                    engine.sync_position_from_exchange(size, entry, upnl, liq);
                }
            }
        }
    } else if let Some(sim) = st.sim.as_ref() {
        let size = sim.position_size().await;
        if let Some(engine) = st.engine.as_mut() {
            engine.sync_position_from_exchange(size, None, None, None);
        }
    }
    Ok(())
}

/// One loop iteration. Returns false when the runner should stop.
pub async fn tick_once(
    app: &AppHandle,
    st: &mut AppState,
    funding_poll_tick: &mut u32,
    atr_poll_tick: &mut u32,
    position_poll_tick: &mut u32,
    fail_streak: &mut u32,
) -> bool {
    if !st.running_task || st.engine.is_none() {
        return false;
    }
    let symbol = st.engine.as_ref().unwrap().config.symbol.clone();
    let session_id = st.engine.as_ref().unwrap().session_id().to_string();

    let mid = match fetch_mid(st, &symbol).await {
        Ok(m) => {
            *fail_streak = 0;
            m
        }
        Err(e) => {
            *fail_streak = fail_streak.saturating_add(1);
            error!("mid fetch failed ({fail_streak}): {e}");
            if *fail_streak >= 5 {
                if let Some(engine) = st.engine.as_mut() {
                    engine.mark_recovering(&format!("api failures: {e}"));
                    persist_checkpoint(&st.storage, engine, "recovering");
                    let _ = app.emit("bot-status", &engine.snapshot());
                }
            }
            return true;
        }
    };

    // Exit Recovering after a good mid + reconcile.
    if matches!(
        st.engine.as_ref().map(|e| e.snapshot().status),
        Some(BotStatus::Recovering)
    ) {
        if let Err(e) = recover_session(app, st).await {
            warn!("recover_session: {e}");
            return true;
        }
    }

    // ATR / confirm bars about once per minute when dynamic.
    let mode_for_atr = st.mode;
    let want_atr = st
        .engine
        .as_ref()
        .map(|e| e.config.is_dynamic())
        .unwrap_or(false)
        && *atr_poll_tick % SLOW_POLL_EVERY_TICKS == 0;
    if want_atr {
        if let Some(engine) = st.engine.as_mut() {
            if let Err(e) = refresh_atr(mode_for_atr, engine).await {
                warn!("atr refresh: {e}");
            }
        }
    }
    *atr_poll_tick = atr_poll_tick.wrapping_add(1);

    let breakout_events = st.engine.as_mut().unwrap().on_mid_price(mid);
    let mut want_recenter = false;
    let mut protective_exit: Option<(bool, bool)> = None;
    for ev in &breakout_events {
        let _ = app.emit("bot-event", ev);
        match ev {
            EngineEvent::RecenterRequested { .. } => want_recenter = true,
            EngineEvent::ProtectiveExitRequested {
                close_position,
                risk_triggered,
                ..
            } => protective_exit = Some((*close_position, *risk_triggered)),
            EngineEvent::Halted { reason } => {
                handle_hard_halt(app, st, &symbol, reason, true).await;
                return false;
            }
            _ => {}
        }
    }

    if let Some((close_position, risk_triggered)) = protective_exit {
        let reason = if risk_triggered {
            "strategy equity risk limit reached"
        } else if close_position {
            "breakout: cancel and close"
        } else {
            "breakout: cancel only"
        };
        handle_hard_halt(app, st, &symbol, reason, close_position).await;
        return false;
    }

    if want_recenter
        || matches!(
            st.engine.as_ref().map(|e| e.snapshot().status),
            Some(BotStatus::Recentering)
        )
    {
        match execute_recenter(app, st, mid).await {
            Ok(()) => {}
            Err(e) => {
                error!("execute_recenter: {e}");
                // Stay halted if integrity path already stopped us.
                if !st.running_task {
                    return false;
                }
            }
        }
        if let Some(engine) = st.engine.as_ref() {
            let _ = app.emit("bot-status", &engine.snapshot());
        }
        return st.running_task;
    }

    // Normal fills / replenish.
    if let Err(e) = process_fills_and_replenish(app, st, &symbol, &session_id).await {
        handle_hard_halt(app, st, &symbol, &e, true).await;
        return false;
    }

    *position_poll_tick = position_poll_tick.wrapping_add(1);
    if st.mode != RunMode::Simulation
        && st.running_task
        && *position_poll_tick % POSITION_SYNC_EVERY_TICKS == 0
    {
        if let Some(hl) = st.hl.as_mut() {
            match hl.get_perp_position(&symbol).await {
                Ok((size, entry, upnl, liq)) => {
                    if let Some(engine) = st.engine.as_mut() {
                        engine.sync_position_from_exchange(size, entry, upnl, liq);
                    }
                }
                Err(e) => warn!("position sync failed: {e}"),
            }
        }
    }
    if st.running_task && *position_poll_tick % ORDER_SYNC_EVERY_TICKS == 0 {
        match sync_open_orders_from_exchange(st, &symbol).await {
            Ok(intents) => {
                if let Err(e) = place_and_register_intents(app, st, &symbol, intents).await {
                    warn!("post-sync replenish/repair failed: {e}");
                }
            }
            Err(e) => warn!("open-order sync failed: {e}"),
        }
    }

    if st.mode != RunMode::Simulation
        && st.running_task
        && *funding_poll_tick % SLOW_POLL_EVERY_TICKS == 0
    {
        if let Some(hl) = st.hl.as_ref() {
            match hl.get_session_funding_pnl(&symbol).await {
                Ok(funding_pnl) => {
                    if let Some(engine) = st.engine.as_mut() {
                        let prev = engine.snapshot().funding_pnl;
                        engine.sync_funding_pnl(funding_pnl);
                        let delta = funding_pnl - prev;
                        if delta != Decimal::ZERO {
                            let key = format!(
                                "{}:{}:{}",
                                session_id,
                                symbol,
                                chrono::Utc::now().timestamp() / 3600
                            );
                            let _ = st.storage.record_funding(&FundingRow {
                                session_id: session_id.clone(),
                                strategy_id: "grid".into(),
                                symbol: symbol.clone(),
                                exchange_time_ms: chrono::Utc::now().timestamp_millis(),
                                usdc: delta,
                                position_size: Some(engine.snapshot().position_base),
                                funding_rate: None,
                                event_key: key,
                            });
                        }
                    }
                }
                Err(e) => warn!("funding pnl sync failed: {e}"),
            }
        }
    }
    *funding_poll_tick = funding_poll_tick.wrapping_add(1);

    if *funding_poll_tick % CHECKPOINT_EVERY_TICKS == 0 {
        if let Some(engine) = st.engine.as_ref() {
            record_equity(&st.storage, engine);
            persist_checkpoint(&st.storage, engine, "tick");
        }
    }

    if let Some(engine) = st.engine.as_ref() {
        let _ = app.emit("bot-status", &engine.snapshot());
    }
    true
}

async fn fetch_mid(st: &mut AppState, symbol: &str) -> Result<Decimal, String> {
    if st.mode == RunMode::Simulation {
        st.sim
            .as_mut()
            .ok_or("no sim")?
            .get_mid(symbol)
            .await
            .map_err(|e| e.to_string())
    } else {
        st.hl
            .as_mut()
            .ok_or("no hl")?
            .get_mid(symbol)
            .await
            .map_err(|e| e.to_string())
    }
}

async fn process_fills_and_replenish(
    app: &AppHandle,
    st: &mut AppState,
    symbol: &str,
    session_id: &str,
) -> Result<(), String> {
    let mut fills = if st.mode == RunMode::Simulation {
        st.sim
            .as_mut()
            .unwrap()
            .drain_fills()
            .await
            .unwrap_or_default()
    } else {
        let live = st
            .engine
            .as_ref()
            .map(|e| e.live_orders().to_vec())
            .unwrap_or_default();
        if let Some(hl) = st.hl.as_mut() {
            hl.restore_tracked_orders(&live);
        }
        st.hl
            .as_mut()
            .unwrap()
            .drain_fills()
            .await
            .unwrap_or_default()
    };

    let mut replenish_intents = Vec::new();
    let mut risk_exit: Option<String> = None;
    sort_fills_chronologically(&mut fills);
    for fill in fills {
        match st.engine.as_mut().unwrap().on_fill(fill.clone()) {
            Ok((pnl, replenish)) => {
                record_fill_ledger(&st.storage, session_id, &fill, pnl);
                let _ = app.emit(
                    "bot-event",
                    &EngineEvent::Filled {
                        fill: fill.clone(),
                        realized_pnl: pnl,
                    },
                );
                if let Some(intent) = replenish {
                    replenish_intents.push(intent);
                }
            }
            Err(e) => {
                risk_exit = Some(e.to_string());
                let _ = app.emit(
                    "bot-event",
                    &EngineEvent::Halted {
                        reason: e.to_string(),
                    },
                );
            }
        }
    }

    if let Some(reason) = risk_exit {
        return Err(reason);
    }

    if replenish_intents.is_empty() {
        return Ok(());
    }

    // SoftBreakout / Recentering: engine should already omit expanding intents.
    let mut intents = replenish_intents;
    let placed = place_replenish(st, intents.clone()).await;
    let placed = match placed {
        Err(e) if is_reduce_only_increase_error(&e) => {
            // Engine/exchange position skew or flip-through-flat: retry without RO
            // instead of hard-halting the whole strategy.
            warn!("replenish reduce_only rejected ({e}); retrying without reduce_only");
            for intent in &mut intents {
                intent.reduce_only = false;
            }
            let _ = st.storage.record_event(
                "replenish_ro_retry",
                &format!("{symbol}: {e}"),
            );
            place_replenish(st, intents).await
        }
        other => other,
    };
    match placed {
        Ok(orders) => {
            for order in orders {
                st.engine
                    .as_mut()
                    .unwrap()
                    .register_live_order(order.clone());
                let _ = app.emit("bot-event", &EngineEvent::OrderPlaced { order });
            }
            Ok(())
        }
        Err(e) => {
            if let Some(ev) = st.engine.as_mut().unwrap().note_order_failure(&e.to_string()) {
                let _ = app.emit("bot-event", &ev);
                if let EngineEvent::Halted { reason } = ev {
                    return Err(reason);
                }
            }
            Err(format!("{symbol} replenish failed: {e}"))
        }
    }
}

fn is_reduce_only_increase_error(err: &impl std::fmt::Display) -> bool {
    let lower = err.to_string().to_ascii_lowercase();
    lower.contains("reduce only order would increase position")
        || lower.contains("reduce_only order would increase position")
}

async fn place_replenish(
    st: &mut AppState,
    intents: Vec<grid_engine::OrderIntent>,
) -> Result<Vec<grid_engine::LiveOrder>, String> {
    if st.mode == RunMode::Simulation {
        st.sim
            .as_mut()
            .unwrap()
            .place_orders(intents)
            .await
            .map_err(|e| e.to_string())
    } else {
        st.hl
            .as_mut()
            .unwrap()
            .place_orders(intents)
            .await
            .map_err(|e| e.to_string())
    }
}

pub async fn recover_session(app: &AppHandle, st: &mut AppState) -> Result<(), String> {
    let symbol = st
        .engine
        .as_ref()
        .map(|e| e.config.symbol.clone())
        .ok_or("no engine")?;
    let session_id = st
        .engine
        .as_ref()
        .map(|e| e.session_id().to_string())
        .unwrap_or_default();

    // Complete any incomplete recenter first.
    if let Ok(Some((op_id, _gen, phase, _intent))) =
        st.storage.incomplete_recenter_op(&session_id)
    {
        info!("resuming incomplete recenter phase={phase}");
        let mid = fetch_mid(st, &symbol).await?;
        match execute_recenter(app, st, mid).await {
            Ok(()) => {
                let _ = st
                    .storage
                    .complete_recenter_op(op_id, "committed", r#"{"resumed":true}"#);
            }
            Err(e) => {
                let _ = st.storage.complete_recenter_op(
                    op_id,
                    "failed",
                    &format!(r#"{{"error":"{e}"}}"#),
                );
                return Err(e);
            }
        }
    }

    let _ = sync_fills_and_position(app, st, &symbol, &session_id).await;

    // Always take exchange open orders as source of truth (drops local phantoms).
    match sync_open_orders_from_exchange(st, &symbol).await {
        Ok(intents) => {
            if let Err(e) = place_and_register_intents(app, st, &symbol, intents).await {
                warn!("recover replenish/repair failed: {e}");
            }
        }
        Err(e) => warn!("recover open-order sync: {e}"),
    }
    if let Some(engine) = st.engine.as_mut() {
        engine.clear_recovering();
        persist_checkpoint(&st.storage, engine, "recovered");
        let _ = app.emit("bot-status", &engine.snapshot());
    }
    Ok(())
}

/// Persist state on window close.
///
/// Simulation orders exist only in-process memory, so sim sessions are ended
/// (not left active for cross-process resume). Live modes detach without
/// cancel/flatten so exchange orders/position can be resumed next launch.
/// Skips revival after manual Stop / Halt so the next launch does not auto-resume.
pub fn detach_on_exit(st: &mut AppState) {
    st.running_task = false;
    let Some(engine) = st.engine.as_mut() else {
        return;
    };
    let status = engine.snapshot().status;
    let sid = engine.session_id().to_string();
    let payload = engine.checkpoint_payload();

    // Simulation: end the session. Leaving active=1 would make the next process
    // resume against a fresh empty SimExchange and treat local open orders as
    // phantoms while holding the global lock (UI freeze).
    if st.mode == RunMode::Simulation {
        let _ = st.storage.save_checkpoint(&sid, "exit_sim", &payload);
        let _ = st.storage.deactivate_session(&sid, Some("stopped"));
        let _ = st.storage.record_event(
            "exit_sim",
            "simulation exit: session ended (orders were in-memory only)",
        );
        info!(
            "simulation exit; session {} deactivated (no cross-process resume)",
            sid
        );
        return;
    }

    // After Stop the engine is Idle; after risk/breakout it may be Halted / BreakoutStopped.
    // Do not upsert active=1 or the next launch will "resume" a session the user already stopped.
    if matches!(
        status,
        BotStatus::Idle
            | BotStatus::Halted
            | BotStatus::BreakoutStopped
            | BotStatus::ProtectiveExit
    ) {
        let final_status = match status {
            BotStatus::Idle => "stopped".to_string(),
            other => bot_status_key(other),
        };
        let _ = st.storage.save_checkpoint(&sid, "exit_after_stop", &payload);
        let _ = st.storage.deactivate_session(&sid, Some(&final_status));
        info!(
            "exit after terminal status {:?}; session {} kept inactive",
            status, sid
        );
        return;
    }
    engine.mark_detached(
        "app closed: exchange orders and position retained; software risk offline",
    );
    persist_checkpoint(&st.storage, engine, "detached");
    let _ = st.storage.record_event(
        "detach",
        "window closed; orders and position preserved on exchange",
    );
    info!(
        "detached session {} symbol={}",
        engine.session_id(),
        engine.config.symbol
    );
}

pub async fn try_resume_active_session(app: &AppHandle, st: &mut AppState) -> Result<bool, String> {
    let cfg = st.storage.load_config().map_err(|e| e.to_string())?;
    if !cfg.resume_on_restart {
        return Ok(false);
    }
    let Some(session) = st
        .storage
        .get_active_session()
        .map_err(|e| e.to_string())?
    else {
        return Ok(false);
    };
    let status_l = session.status.to_ascii_lowercase();
    // Do not resume sessions the user already stopped / that hard-halted.
    // Note: DB may store snake_case (`stopped`) or Debug (`Stopped`) — compare lowercase.
    if status_l.contains("halted")
        || status_l.contains("stopped")
        || status_l.contains("protective_exit")
        || status_l == "idle"
    {
        let _ = st
            .storage
            .deactivate_session(&session.session_id, Some(&status_l));
        return Ok(false);
    }
    let Some((phase, payload)) = st
        .storage
        .latest_checkpoint(&session.session_id)
        .map_err(|e| e.to_string())?
    else {
        return Ok(false);
    };
    let phase_l = phase.to_ascii_lowercase();
    if phase_l == "stopped"
        || phase_l == "exit_after_stop"
        || phase_l == "exit_sim"
        || phase_l == "halted"
        || phase_l == "halt_failed"
        || phase_l == "integrity_halt"
    {
        let _ = st
            .storage
            .deactivate_session(&session.session_id, Some("stopped"));
        info!(
            "skip resume: session {} checkpoint phase={phase}",
            session.session_id
        );
        return Ok(false);
    }

    // Residual simulation sessions are not recoverable: sim orders never leave
    // process memory. Deactivate and skip so we do not reconcile against an
    // empty SimExchange (phantom fills + long global-lock hold → UI freeze).
    let checkpoint_mode = payload
        .get("mode")
        .and_then(|v| serde_json::from_value::<RunMode>(v.clone()).ok());
    if matches!(checkpoint_mode, Some(RunMode::Simulation)) || st.mode == RunMode::Simulation
    {
        if matches!(checkpoint_mode, Some(RunMode::Simulation)) {
            let _ = st
                .storage
                .deactivate_session(&session.session_id, Some("stopped"));
            info!(
                "skip resume: residual simulation session {} deactivated",
                session.session_id
            );
        } else {
            info!(
                "skip resume: current mode is simulation (session {})",
                session.session_id
            );
        }
        return Ok(false);
    }

    info!("resuming session {} phase={phase}", session.session_id);

    let config: GridConfig = payload
        .get("config")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .or_else(|| serde_json::from_str(&session.config_json).ok())
        .ok_or("resume: missing config")?;

    let mode = st.mode;
    if st.private_key.trim().is_empty() {
        return Err(i18n("resumeRequiresKey"));
    }
    if st.hl.is_none() {
        let mut hl = HyperliquidExchange::new(mode);
        hl.set_private_key(&st.private_key)
            .map_err(|e| e.to_string())?;
        st.hl = Some(hl);
    }
    st.hl
        .as_mut()
        .unwrap()
        .connect()
        .await
        .map_err(|e| e.to_string())?;
    if let Err(e) = st.hl.as_mut().unwrap().prime_seen_fills().await {
        warn!("prime_seen_fills: {e}");
    }

    let mut engine = GridEngine::new(config.clone(), mode, config.total_budget)
        .map_err(|e| e.to_string())?;
    engine.set_session_id(&session.session_id);
    engine.restore_from_checkpoint(&payload)?;
    engine.mark_recovering("resuming after app restart");
    st.engine = Some(engine);
    st.running_task = true;
    if let Err(e) = recover_session(app, st).await {
        // Avoid fake-dead state: resume failed but running_task would block Start.
        st.running_task = false;
        warn!("resume recover_session failed; cleared running_task: {e}");
        return Err(e);
    }
    let _ = st.storage.record_event("resume", "session resumed from checkpoint");
    Ok(true)
}

pub async fn run_loop(app: AppHandle, state: std::sync::Arc<tokio::sync::Mutex<AppState>>) {
    let mut funding_poll_tick: u32 = 0;
    let mut atr_poll_tick: u32 = 0;
    let mut position_poll_tick: u32 = 0;
    let mut fail_streak: u32 = 0;
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(TICK_INTERVAL_MS)).await;
        let mut st = state.lock().await;
        if !tick_once(
            &app,
            &mut st,
            &mut funding_poll_tick,
            &mut atr_poll_tick,
            &mut position_poll_tick,
            &mut fail_streak,
        )
        .await
        {
            break;
        }
    }
    info!("bot loop exited");
}

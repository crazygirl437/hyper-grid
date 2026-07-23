use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use exchange::{
    fetch_candles, fetch_live_mid, list_live_markets, Candle, CandleInterval, Exchange,
    HyperliquidExchange, MarketInfo, SimExchange,
};
use grid_engine::{
    preview_grid, BotSnapshot, BreakoutAction, GridConfig, GridEngine, GridPreview, GridSpacing,
    MarketKind, RunMode,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use storage::{AppConfig, EventRow, FillRow, Storage};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

/// Skip duplicate flatten when window close already flattened.
static EXIT_FLATTEN_DONE: AtomicBool = AtomicBool::new(false);

struct AppState {
    storage: Storage,
    engine: Option<GridEngine>,
    sim: Option<SimExchange>,
    hl: Option<HyperliquidExchange>,
    mode: RunMode,
    private_key: String,
    address: Option<String>,
    running_task: bool,
}

impl AppState {
    fn new() -> anyhow::Result<Self> {
        let storage = Storage::open_default()?;
        let cfg = storage.load_config().unwrap_or_default();
        let mode = parse_mode(&cfg.mode);
        Ok(Self {
            storage,
            engine: None,
            sim: None,
            hl: None,
            mode,
            private_key: cfg.private_key,
            address: None,
            running_task: false,
        })
    }
}

fn parse_mode(s: &str) -> RunMode {
    match s {
        "testnet" => RunMode::Testnet,
        "mainnet" => RunMode::Mainnet,
        _ => RunMode::Simulation,
    }
}

fn mode_str(m: RunMode) -> &'static str {
    match m {
        RunMode::Simulation => "simulation",
        RunMode::Testnet => "testnet",
        RunMode::Mainnet => "mainnet",
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviewRequest {
    symbol: String,
    lower_price: String,
    upper_price: String,
    grid_count: u32,
    total_budget: String,
    spacing: String,
    mid_price: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartRequest {
    symbol: String,
    lower_price: String,
    upper_price: String,
    grid_count: u32,
    total_budget: String,
    spacing: String,
    breakout_action: String,
    max_drawdown_pct: String,
    max_daily_loss: String,
    max_order_failures: u32,
    #[serde(default = "default_leverage_req")]
    leverage: u32,
    #[serde(default = "default_cross_req")]
    is_cross: bool,
}

fn default_leverage_req() -> u32 {
    5
}
fn default_cross_req() -> bool {
    true
}

fn dec(s: &str) -> Result<Decimal, String> {
    s.parse::<Decimal>().map_err(|e| e.to_string())
}

fn spacing(s: &str) -> GridSpacing {
    if s == "geometric" {
        GridSpacing::Geometric
    } else {
        GridSpacing::Arithmetic
    }
}

fn breakout(s: &str) -> BreakoutAction {
    match s {
        "alert_only" => BreakoutAction::AlertOnly,
        "cancel_and_pause" => BreakoutAction::CancelAndPause,
        _ => BreakoutAction::Pause,
    }
}

#[tauri::command]
fn greet(name: String) -> String {
    format!("hello {name} from hyper-grid")
}

#[tauri::command]
async fn preview_grid_cmd(req: PreviewRequest) -> Result<GridPreview, String> {
    let config = GridConfig {
        symbol: req.symbol,
        lower_price: dec(&req.lower_price)?,
        upper_price: dec(&req.upper_price)?,
        grid_count: req.grid_count,
        total_budget: dec(&req.total_budget)?,
        spacing: spacing(&req.spacing),
        breakout_action: BreakoutAction::Pause,
        max_drawdown_pct: Decimal::ZERO,
        max_daily_loss: Decimal::ZERO,
        max_order_failures: 5,
        market: MarketKind::Perp,
        leverage: 5,
        is_cross: true,
    };
    let mid = dec(&req.mid_price)?;
    preview_grid(&config, mid).map_err(|e| e.to_string())
}

#[tauri::command]
async fn set_mode(state: State<'_, Arc<Mutex<AppState>>>, mode: String) -> Result<(), String> {
    let mut st = state.lock().await;
    let next = parse_mode(&mode);
    if st.running_task && next != st.mode {
        return Err("机器人运行中，请先停止再切换模式".into());
    }
    st.mode = next;
    // Drop exchange clients so the next connect uses the new API endpoint.
    // Never wipe while running — that drops open-order oid tracking and misses fills.
    if !st.running_task {
        st.hl = None;
        st.sim = None;
    }
    let mut cfg = st.storage.load_config().map_err(|e| e.to_string())?;
    cfg.mode = mode_str(st.mode).into();
    st.storage.save_config(&cfg).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn set_private_key(
    state: State<'_, Arc<Mutex<AppState>>>,
    private_key: String,
) -> Result<String, String> {
    let mut st = state.lock().await;
    let key_changed = private_key != st.private_key;
    if st.running_task && key_changed {
        return Err("机器人运行中，请先停止再更换私钥".into());
    }
    st.private_key = private_key.clone();
    let mut address = String::new();
    if !private_key.trim().is_empty() && st.mode != RunMode::Simulation {
        // Keep the live client when the key did not change (e.g. refresh balance).
        if let Some(hl) = st.hl.as_ref() {
            if !key_changed {
                address = hl.address().unwrap_or("").to_string();
                st.address = Some(address.clone());
                let mut cfg = st.storage.load_config().map_err(|e| e.to_string())?;
                cfg.private_key = private_key;
                st.storage.save_config(&cfg).map_err(|e| e.to_string())?;
                return Ok(address);
            }
        }
        let mut hl = HyperliquidExchange::new(st.mode);
        hl.set_private_key(&private_key).map_err(|e| e.to_string())?;
        address = hl.address().unwrap_or("").to_string();
        st.address = Some(address.clone());
        st.hl = Some(hl);
    } else if !private_key.trim().is_empty() {
        // Derive address even in simulation for display
        let mut hl = HyperliquidExchange::new(RunMode::Testnet);
        if hl.set_private_key(&private_key).is_ok() {
            address = hl.address().unwrap_or("").to_string();
            st.address = Some(address.clone());
        }
        if !st.running_task {
            st.hl = None;
        }
    } else if !st.running_task {
        st.hl = None;
        st.address = None;
    }
    let mut cfg = st.storage.load_config().map_err(|e| e.to_string())?;
    cfg.private_key = private_key;
    st.storage.save_config(&cfg).map_err(|e| e.to_string())?;
    Ok(address)
}

#[tauri::command]
async fn get_account(
    state: State<'_, Arc<Mutex<AppState>>>,
) -> Result<serde_json::Value, String> {
    let mut st = state.lock().await;
    let mode = mode_str(st.mode).to_string();

    // Recreate exchange client after mode switch / restart so balances keep working
    // without requiring the user to click Save again.
    if st.mode != RunMode::Simulation && !st.private_key.trim().is_empty() && st.hl.is_none() {
        let mut hl = HyperliquidExchange::new(st.mode);
        hl.set_private_key(&st.private_key)
            .map_err(|e| e.to_string())?;
        st.address = hl.address().map(|a| a.to_string());
        st.hl = Some(hl);
    }

    let address = st.address.clone().unwrap_or_default();
    let balances = if st.mode == RunMode::Simulation {
        if let Some(sim) = st.sim.as_mut() {
            sim.get_balances().await.unwrap_or_default()
        } else {
            vec![]
        }
    } else if let Some(hl) = st.hl.as_mut() {
        let _ = hl.connect().await;
        hl.get_balances().await.unwrap_or_default()
    } else {
        vec![]
    };
    Ok(serde_json::json!({
        "mode": mode,
        "address": address,
        "balances": balances,
        "hasKey": !st.private_key.is_empty(),
    }))
}

#[tauri::command]
async fn list_markets(state: State<'_, Arc<Mutex<AppState>>>) -> Result<Vec<MarketInfo>, String> {
    let st = state.lock().await;
    list_live_markets(st.mode)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_symbols(state: State<'_, Arc<Mutex<AppState>>>) -> Result<Vec<String>, String> {
    let markets = {
        let st = state.lock().await;
        list_live_markets(st.mode)
            .await
            .map_err(|e| e.to_string())?
    };
    Ok(markets.into_iter().map(|m| m.symbol).collect())
}

#[tauri::command]
async fn get_mid(state: State<'_, Arc<Mutex<AppState>>>, symbol: String) -> Result<String, String> {
    let mut st = state.lock().await;

    if st.mode == RunMode::Simulation {
        // While the bot is running, keep the profitable in-band oscillator —
        // do not snap mid back to the live exchange price.
        if st.running_task {
            if let Some(sim) = st.sim.as_ref() {
                return Ok(sim.peek_mid().await.normalize().to_string());
            }
        }
        let mid = fetch_live_mid(st.mode, &symbol)
            .await
            .map_err(|e| e.to_string())?;
        if st.sim.is_none() {
            st.sim = Some(SimExchange::new(
                symbol.clone(),
                mid,
                Decimal::new(10000, 0),
                Decimal::ZERO,
            ));
        } else {
            st.sim.as_mut().unwrap().set_mid_async(mid).await;
        }
        return Ok(mid.normalize().to_string());
    }

    // Live / testnet / mainnet: use Hyperliquid mid.
    let mid = fetch_live_mid(st.mode, &symbol)
        .await
        .map_err(|e| e.to_string())?;

    if st.hl.is_none() {
        let mut hl = HyperliquidExchange::new(st.mode);
        if !st.private_key.is_empty() {
            hl.set_private_key(&st.private_key)
                .map_err(|e| e.to_string())?;
        }
        hl.connect().await.map_err(|e| e.to_string())?;
        st.hl = Some(hl);
    }
    Ok(mid.normalize().to_string())
}

#[tauri::command]
async fn get_candles(
    state: State<'_, Arc<Mutex<AppState>>>,
    symbol: String,
    interval: String,
    limit: Option<usize>,
) -> Result<Vec<Candle>, String> {
    let mode = {
        let st = state.lock().await;
        st.mode
    };
    let iv = CandleInterval::parse(&interval)
        .ok_or_else(|| format!("unsupported candle interval: {interval}"))?;
    fetch_candles(mode, &symbol, iv, limit.unwrap_or(300))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn start_bot(
    app: AppHandle,
    state: State<'_, Arc<Mutex<AppState>>>,
    req: StartRequest,
) -> Result<BotSnapshot, String> {
    let state_arc = state.inner().clone();
    {
        let mut st = state_arc.lock().await;
        if st.running_task {
            // No-op when already running: return current snapshot without restarting.
            if let Some(engine) = st.engine.as_ref() {
                return Ok(engine.snapshot());
            }
            return Err("bot already running".into());
        }
        if st.mode != RunMode::Simulation && st.private_key.trim().is_empty() {
            return Err("private key required for testnet/mainnet".into());
        }
        let config = GridConfig {
            symbol: req.symbol.clone(),
            lower_price: dec(&req.lower_price)?,
            upper_price: dec(&req.upper_price)?,
            grid_count: req.grid_count,
            total_budget: dec(&req.total_budget)?,
            spacing: spacing(&req.spacing),
            breakout_action: breakout(&req.breakout_action),
            max_drawdown_pct: dec(&req.max_drawdown_pct).unwrap_or(Decimal::ZERO),
            max_daily_loss: dec(&req.max_daily_loss).unwrap_or(Decimal::ZERO),
            max_order_failures: req.max_order_failures,
            market: MarketKind::Perp,
            leverage: req.leverage.max(1).min(50),
            is_cross: req.is_cross,
        };
        let mid = if st.mode == RunMode::Simulation {
            let live = fetch_live_mid(st.mode, &config.symbol)
                .await
                .unwrap_or_else(|_| (config.lower_price + config.upper_price) / Decimal::from(2));
            let seed = live.clamp(config.lower_price, config.upper_price);
            // Oscillate inside the configured grid band → mean-reversion → stable grid profit.
            st.sim = Some(SimExchange::with_band(
                config.symbol.clone(),
                seed,
                config.total_budget * Decimal::from(2),
                Decimal::ZERO,
                config.lower_price,
                config.upper_price,
            ));
            st.sim
                .as_mut()
                .unwrap()
                .connect()
                .await
                .map_err(|e| e.to_string())?;
            // Clear leftovers first (overlay only covers this brief step).
            flatten_account_notify(&app, &mut st, "start")
                .await
                .map_err(|e| format!("启动前撤单/平仓失败: {e}"))?;
            // Flatten resets base; keep the oscillation band.
            if let Some(sim) = st.sim.as_ref() {
                sim.set_band(config.lower_price, config.upper_price).await;
            }
            seed
        } else {
            if st.hl.is_none() {
                let mut hl = HyperliquidExchange::new(st.mode);
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
            // Clear leftovers first (overlay only covers this brief step).
            flatten_account_notify(&app, &mut st, "start")
                .await
                .map_err(|e| format!("启动前撤单/平仓失败: {e}"))?;
            let hl = st.hl.as_mut().unwrap();
            hl.set_leverage(&config.symbol, config.leverage, config.is_cross)
                .await
                .map_err(|e| e.to_string())?;
            hl.get_mid(&config.symbol)
                .await
                .map_err(|e| e.to_string())?
        };

        // Snapshot existing exchange fills BEFORE we place, so history is not
        // mistaken for new bot fills. Do this after flatten, before place.
        if st.mode != RunMode::Simulation {
            if let Some(hl) = st.hl.as_mut() {
                if let Err(e) = hl.prime_seen_fills().await {
                    warn!("prime_seen_fills failed: {e}");
                }
            }
        }

        let mut engine =
            GridEngine::new(config.clone(), st.mode, config.total_budget).map_err(|e| e.to_string())?;
        let intents = engine
            .bootstrap_intents(mid)
            .map_err(|e| e.to_string())?;

        let placed = if st.mode == RunMode::Simulation {
            st.sim
                .as_mut()
                .unwrap()
                .place_orders(intents)
                .await
                .map_err(|e| e.to_string())?
        } else {
            let hl = st.hl.as_mut().unwrap();
            if let Err(e) = hl
                .preflight_grid_notional(&intents, config.leverage)
                .await
            {
                return Err(e.to_string());
            }
            match hl.place_orders(intents).await {
                Ok(o) => o,
                Err(e) => {
                    // Extra safety: clear any leftovers if rollback missed something.
                    let _ = hl.cancel_all(&config.symbol).await;
                    if let Some(ev) = engine.note_order_failure(&e.to_string()) {
                        let _ = app.emit("bot-event", &ev);
                    }
                    return Err(e.to_string());
                }
            }
        };
        for order in placed {
            engine.register_live_order(order);
        }
        let snap = engine.snapshot();
        st.engine = Some(engine);
        st.running_task = true;
        let _ = st.storage.record_event("start", "bot started");
        let _ = app.emit("bot-status", &snap);
    }

    let app2 = app.clone();
    let state2 = state_arc.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
            let mut st = state2.lock().await;
            if !st.running_task || st.engine.is_none() {
                break;
            }
            let symbol = st.engine.as_ref().unwrap().config.symbol.clone();
            let mid = if st.mode == RunMode::Simulation {
                match st.sim.as_mut().unwrap().get_mid(&symbol).await {
                    Ok(m) => m,
                    Err(e) => {
                        error!("sim mid: {e}");
                        continue;
                    }
                }
            } else {
                match st.hl.as_mut().unwrap().get_mid(&symbol).await {
                    Ok(m) => m,
                    Err(e) => {
                        error!("hl mid: {e}");
                        continue;
                    }
                }
            };

            let breakout_events = st.engine.as_mut().unwrap().on_mid_price(mid);
            for ev in breakout_events {
                let _ = app2.emit("bot-event", &ev);
            }

            let fills = if st.mode == RunMode::Simulation {
                st.sim.as_mut().unwrap().drain_fills().await.unwrap_or_default()
            } else {
                // Safety net: if the HL client was recreated, re-attach engine oids
                // so website fills can still be matched.
                let live = st
                    .engine
                    .as_ref()
                    .map(|e| e.live_orders().to_vec())
                    .unwrap_or_default();
                if let Some(hl) = st.hl.as_mut() {
                    hl.restore_tracked_orders(&live);
                }
                st.hl.as_mut().unwrap().drain_fills().await.unwrap_or_default()
            };

            let mut replenish_intents = Vec::new();
            for fill in fills {
                let side = format!("{:?}", fill.side);
                match st.engine.as_mut().unwrap().on_fill(fill.clone()) {
                    Ok((pnl, replenish)) => {
                        let _ = st.storage.record_fill(
                            &fill.symbol,
                            &side,
                            fill.price,
                            fill.size,
                            pnl,
                            &fill.client_id,
                        );
                        let _ = app2.emit(
                            "bot-event",
                            &grid_engine::EngineEvent::Filled {
                                fill: fill.clone(),
                                realized_pnl: pnl,
                            },
                        );
                        if let Some(intent) = replenish {
                            replenish_intents.push(intent);
                        }
                    }
                    Err(e) => {
                        let _ = app2.emit(
                            "bot-event",
                            &grid_engine::EngineEvent::Halted {
                                reason: e.to_string(),
                            },
                        );
                        st.running_task = false;
                    }
                }
            }

            if !replenish_intents.is_empty() && st.running_task {
                let placed = if st.mode == RunMode::Simulation {
                    st.sim.as_mut().unwrap().place_orders(replenish_intents).await
                } else {
                    st.hl.as_mut().unwrap().place_orders(replenish_intents).await
                };
                match placed {
                    Ok(orders) => {
                        for order in orders {
                            st.engine.as_mut().unwrap().register_live_order(order.clone());
                            let _ = app2.emit(
                                "bot-event",
                                &grid_engine::EngineEvent::OrderPlaced { order },
                            );
                        }
                    }
                    Err(e) => {
                        if let Some(ev) =
                            st.engine.as_mut().unwrap().note_order_failure(&e.to_string())
                        {
                            let _ = app2.emit("bot-event", &ev);
                        }
                    }
                }
            }

            // Dashboard position must match the exchange, not only locally inferred fills.
            if st.mode != RunMode::Simulation && st.running_task {
                if let Some(hl) = st.hl.as_mut() {
                    match hl.get_perp_position(&symbol).await {
                        Ok((size, entry, upnl)) => {
                            if let Some(engine) = st.engine.as_mut() {
                                engine.sync_position_from_exchange(size, entry, upnl);
                            }
                        }
                        Err(e) => {
                            warn!("position sync failed: {e}");
                        }
                    }
                }
            }

            if let Some(engine) = st.engine.as_ref() {
                let snap = engine.snapshot();
                let _ = app2.emit("bot-status", &snap);
            }
        }
        info!("bot loop exited");
    });

    let st = state_arc.lock().await;
    Ok(st
        .engine
        .as_ref()
        .map(|e| e.snapshot())
        .unwrap_or_else(|| BotSnapshot {
            status: grid_engine::BotStatus::Idle,
            mode: RunMode::Simulation,
            symbol: req.symbol,
            mid_price: None,
            open_orders: 0,
            resting_orders: vec![],
            position_base: Decimal::ZERO,
            avg_entry_price: None,
            realized_pnl: Decimal::ZERO,
            unrealized_pnl: Decimal::ZERO,
            events_tail: vec![],
        }))
}

#[tauri::command]
async fn pause_bot(state: State<'_, Arc<Mutex<AppState>>>) -> Result<BotSnapshot, String> {
    let mut st = state.lock().await;
    let engine = st.engine.as_mut().ok_or("no engine")?;
    engine.pause();
    Ok(engine.snapshot())
}

#[tauri::command]
async fn resume_bot(state: State<'_, Arc<Mutex<AppState>>>) -> Result<BotSnapshot, String> {
    let mut st = state.lock().await;
    let engine = st.engine.as_mut().ok_or("no engine")?;
    engine.resume().map_err(|e| e.to_string())?;
    Ok(engine.snapshot())
}

async fn ensure_exchange_ready(st: &mut AppState) -> Result<(), String> {
    if st.mode == RunMode::Simulation {
        return Ok(());
    }
    if st.private_key.trim().is_empty() {
        return Ok(());
    }
    if st.hl.is_none() {
        let mut hl = HyperliquidExchange::new(st.mode);
        hl.set_private_key(&st.private_key)
            .map_err(|e| e.to_string())?;
        st.address = hl.address().map(|a| a.to_string());
        st.hl = Some(hl);
    }
    st.hl
        .as_mut()
        .unwrap()
        .connect()
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Cancel all open orders and close all positions on the active exchange.
async fn flatten_account(st: &mut AppState) -> Result<(), String> {
    ensure_exchange_ready(st).await?;
    if st.mode == RunMode::Simulation {
        if let Some(sim) = st.sim.as_mut() {
            sim.flatten().await.map_err(|e| e.to_string())?;
        }
        return Ok(());
    }
    if let Some(hl) = st.hl.as_mut() {
        info!("flattening account: cancel orders + close positions");
        hl.flatten().await.map_err(|e| e.to_string())?;
        let _ = st.storage.record_event("flatten", "canceled orders and closed positions");
    }
    Ok(())
}

#[derive(Clone, Serialize)]
struct FlattenStartPayload {
    reason: String,
}

#[derive(Clone, Serialize)]
struct FlattenEndPayload {
    reason: String,
    ok: bool,
    error: Option<String>,
}

async fn flatten_account_notify(
    app: &AppHandle,
    st: &mut AppState,
    reason: &str,
) -> Result<(), String> {
    let _ = app.emit(
        "flatten-start",
        FlattenStartPayload {
            reason: reason.to_string(),
        },
    );
    let result = flatten_account(st).await;
    let _ = app.emit(
        "flatten-end",
        FlattenEndPayload {
            reason: reason.to_string(),
            ok: result.is_ok(),
            error: result.as_ref().err().cloned(),
        },
    );
    result
}

#[tauri::command]
async fn flatten_now(
    app: AppHandle,
    state: State<'_, Arc<Mutex<AppState>>>,
    reason: Option<String>,
) -> Result<(), String> {
    let reason = reason.unwrap_or_else(|| "manual".into());
    let mut st = state.lock().await;
    st.running_task = false;
    let res = flatten_account_notify(&app, &mut st, &reason).await;
    if reason == "exit" {
        EXIT_FLATTEN_DONE.store(true, Ordering::SeqCst);
    }
    res
}

#[tauri::command]
async fn stop_bot(
    app: AppHandle,
    state: State<'_, Arc<Mutex<AppState>>>,
) -> Result<BotSnapshot, String> {
    let mut st = state.lock().await;
    st.running_task = false;
    let snap = {
        if let Some(engine) = st.engine.as_mut() {
            let _ = engine.stop();
            engine.snapshot()
        } else {
            BotSnapshot {
                status: grid_engine::BotStatus::Idle,
                mode: st.mode,
                symbol: String::new(),
                mid_price: None,
                open_orders: 0,
                resting_orders: vec![],
                position_base: Decimal::ZERO,
                avg_entry_price: None,
                realized_pnl: Decimal::ZERO,
                unrealized_pnl: Decimal::ZERO,
                events_tail: vec![],
            }
        }
    };
    // Always flatten on stop — cancel resting orders and close positions.
    if let Err(e) = flatten_account_notify(&app, &mut st, "stop").await {
        error!("flatten on stop failed: {e}");
        return Err(format!("已停止策略，但平仓/撤单失败: {e}"));
    }
    if let Some(engine) = st.engine.as_mut() {
        engine.note("flattened: canceled orders & closed positions");
    }
    Ok(st.engine.as_ref().map(|e| e.snapshot()).unwrap_or(snap))
}

#[tauri::command]
async fn get_status(state: State<'_, Arc<Mutex<AppState>>>) -> Result<Option<BotSnapshot>, String> {
    let st = state.lock().await;
    Ok(st.engine.as_ref().map(|e| e.snapshot()))
}

#[tauri::command]
async fn clear_logs(state: State<'_, Arc<Mutex<AppState>>>) -> Result<Option<BotSnapshot>, String> {
    let mut st = state.lock().await;
    st.storage.clear_logs().map_err(|e| e.to_string())?;
    if let Some(engine) = st.engine.as_mut() {
        engine.clear_events();
        return Ok(Some(engine.snapshot()));
    }
    Ok(None)
}

#[tauri::command]
async fn list_fills(state: State<'_, Arc<Mutex<AppState>>>, limit: usize) -> Result<Vec<FillRow>, String> {
    let st = state.lock().await;
    st.storage.list_fills(limit).map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_events(
    state: State<'_, Arc<Mutex<AppState>>>,
    limit: usize,
) -> Result<Vec<EventRow>, String> {
    let st = state.lock().await;
    st.storage.list_events(limit).map_err(|e| e.to_string())
}

#[tauri::command]
async fn export_fills_csv(
    state: State<'_, Arc<Mutex<AppState>>>,
    path: String,
) -> Result<usize, String> {
    let st = state.lock().await;
    st.storage
        .export_fills_csv(&PathBuf::from(path))
        .map_err(|e| e.to_string())
}

#[derive(Debug, Serialize, Deserialize)]
struct ExportConfig {
    symbol: String,
    lower_price: String,
    upper_price: String,
    grid_count: u32,
    total_budget: String,
    spacing: String,
    breakout_action: String,
    #[serde(default = "default_drawdown")]
    max_drawdown_pct: String,
    #[serde(default = "default_daily_loss")]
    max_daily_loss: String,
    #[serde(default = "default_order_failures")]
    max_order_failures: u32,
    #[serde(default = "default_leverage_export")]
    leverage: u32,
    #[serde(default = "default_cross_export")]
    is_cross: bool,
}

fn default_drawdown() -> String {
    "20".into()
}
fn default_daily_loss() -> String {
    "100".into()
}
fn default_order_failures() -> u32 {
    5
}
fn default_leverage_export() -> u32 {
    5
}
fn default_cross_export() -> bool {
    true
}

#[tauri::command]
fn export_strategy_config(cfg: ExportConfig) -> Result<String, String> {
    serde_json::to_string_pretty(&cfg).map_err(|e| e.to_string())
}

#[tauri::command]
fn import_strategy_config(json: String) -> Result<ExportConfig, String> {
    serde_json::from_str(&json).map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_language(state: State<'_, Arc<Mutex<AppState>>>) -> Result<Option<String>, String> {
    let st = state.lock().await;
    let cfg = st.storage.load_config().map_err(|e| e.to_string())?;
    Ok(cfg.language)
}

#[tauri::command]
async fn set_language(state: State<'_, Arc<Mutex<AppState>>>, language: String) -> Result<(), String> {
    let st = state.lock().await;
    let mut cfg = st.storage.load_config().map_err(|e| e.to_string())?;
    cfg.language = Some(language);
    st.storage.save_config(&cfg).map_err(|e| e.to_string())
}

#[derive(Debug, Serialize, Deserialize)]
struct SettingsPayload {
    #[serde(flatten)]
    config: AppConfig,
    env_path: String,
}

#[tauri::command]
async fn get_settings(state: State<'_, Arc<Mutex<AppState>>>) -> Result<SettingsPayload, String> {
    let st = state.lock().await;
    let config = st.storage.load_config().map_err(|e| e.to_string())?;
    Ok(SettingsPayload {
        env_path: st.storage.dotenv_path().display().to_string(),
        config,
    })
}

#[tauri::command]
async fn save_settings(
    state: State<'_, Arc<Mutex<AppState>>>,
    settings: AppConfig,
) -> Result<SettingsPayload, String> {
    let mut st = state.lock().await;
    let new_mode = parse_mode(&settings.mode);
    let key_changed = settings.private_key != st.private_key;
    let mode_changed = new_mode != st.mode;

    if st.running_task && mode_changed {
        return Err("机器人运行中，请先停止再切换模式".into());
    }
    if st.running_task && key_changed {
        return Err("机器人运行中，请先停止再更换私钥".into());
    }

    st.mode = new_mode;
    st.private_key = settings.private_key.clone();

    // Critical: never replace the live Hyperliquid client while the bot is running.
    // Auto-saving .env used to recreate `st.hl`, wiping open-order oid maps so
    // website fills never matched and never appeared in the app.
    if !st.running_task {
        if !settings.private_key.trim().is_empty() {
            let need_new_client = st.hl.is_none() || key_changed || mode_changed;
            if need_new_client {
                let mut hl = HyperliquidExchange::new(if st.mode == RunMode::Simulation {
                    RunMode::Testnet
                } else {
                    st.mode
                });
                if hl.set_private_key(&settings.private_key).is_ok() {
                    st.address = hl.address().map(|a| a.to_string());
                    if st.mode != RunMode::Simulation {
                        st.hl = Some(hl);
                    } else {
                        st.hl = None;
                    }
                }
            } else if let Some(hl) = st.hl.as_ref() {
                st.address = hl.address().map(|a| a.to_string());
            }
        } else {
            st.hl = None;
            st.address = None;
        }
        if mode_changed {
            st.sim = None;
        }
    }

    st.storage
        .save_config(&settings)
        .map_err(|e| e.to_string())?;
    Ok(SettingsPayload {
        env_path: st.storage.dotenv_path().display().to_string(),
        config: settings,
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .init();

    let state = AppState::new().expect("storage");
    let state = Arc::new(Mutex::new(state));
    let state_for_exit = state.clone();
    let state_for_startup = state.clone();

    tauri::Builder::default()
        .manage(state)
        .setup(move |app| {
            let handle = app.handle().clone();
            // On app launch: if wallet is configured, flatten leftover orders/positions.
            tauri::async_runtime::spawn(async move {
                let mut st = state_for_startup.lock().await;
                if st.mode != RunMode::Simulation && !st.private_key.trim().is_empty() {
                    if let Err(e) = flatten_account_notify(&handle, &mut st, "startup").await {
                        error!("flatten on startup failed: {e}");
                    } else {
                        info!("startup flatten completed");
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            greet,
            preview_grid_cmd,
            set_mode,
            set_private_key,
            get_account,
            list_symbols,
            list_markets,
            get_mid,
            get_candles,
            start_bot,
            pause_bot,
            resume_bot,
            stop_bot,
            flatten_now,
            get_status,
            list_fills,
            list_events,
            clear_logs,
            export_fills_csv,
            export_strategy_config,
            import_strategy_config,
            get_language,
            set_language,
            get_settings,
            save_settings,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(move |_app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                if EXIT_FLATTEN_DONE.swap(true, Ordering::SeqCst) {
                    return;
                }
                // Fallback when close path did not flatten (e.g. forced kill after UI).
                let state = state_for_exit.clone();
                let _ = std::thread::spawn(move || {
                    if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        rt.block_on(async move {
                            let mut st = state.lock().await;
                            st.running_task = false;
                            if let Err(e) = flatten_account(&mut st).await {
                                error!("flatten on exit failed: {e}");
                            } else {
                                info!("exit flatten completed");
                            }
                        });
                    }
                })
                .join();
            }
        });
}

fn main() {
    run();
}

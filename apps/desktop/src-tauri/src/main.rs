mod runner;
mod i18n_err;

use std::path::PathBuf;
use std::sync::Arc;

use exchange::{
    fetch_candles, fetch_live_mid, list_live_markets, list_live_mids, Candle, CandleInterval,
    Exchange, HyperliquidExchange, MarketInfo, SimExchange,
};
use grid_engine::{
    preview_grid_with_options, BotSnapshot, BreakoutAction, DynamicGridConfig, GridConfig,
    GridEngine, GridMode, GridPreview, GridSpacing, MarketKind, RunMode,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use storage::{
    resolve_data_dir, AppConfig, DailyPnlRow, EquitySnapshotRow, EventRow, FillRow,
    SessionListItem, SessionPnlSummary, Storage,
};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use i18n_err::{i18n, i18n_kv};
use runner::{
    build_grid_config, detach_on_exit, idle_snapshot, protect_symbol, resolve_dynamic_bounds,
    run_loop, try_resume_active_session,
};

pub struct AppState {
    pub storage: Storage,
    pub engine: Option<GridEngine>,
    pub sim: Option<SimExchange>,
    pub hl: Option<HyperliquidExchange>,
    pub mode: RunMode,
    pub private_key: String,
    pub address: Option<String>,
    pub running_task: bool,
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
    #[serde(default = "default_leverage_req")]
    leverage: u32,
    #[serde(default = "default_cross_req")]
    is_cross: bool,
    /// Optional account equity (USDC) for a tighter cross-margin liquidation check.
    #[serde(default)]
    account_equity: Option<String>,
    /// Exchange max leverage for the market (used to derive maintenance margin rate).
    #[serde(default)]
    max_leverage: Option<u32>,
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
    #[serde(default)]
    grid_mode: Option<String>,
    #[serde(default)]
    atr_interval: Option<String>,
    #[serde(default)]
    atr_period: Option<u32>,
    #[serde(default)]
    atr_mult: Option<String>,
    #[serde(default)]
    confirm_bars: Option<u32>,
    #[serde(default)]
    recenter_cooldown_secs: Option<u64>,
    #[serde(default)]
    max_recenters_per_day: Option<u32>,
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
        leverage: req.leverage,
        is_cross: req.is_cross,
        grid_mode: GridMode::Fixed,
        dynamic: DynamicGridConfig::default(),
    };
    let mid = dec(&req.mid_price)?;
    let equity = match req.account_equity.as_deref() {
        Some(s) if !s.trim().is_empty() => Some(dec(s)?),
        _ => None,
    };
    preview_grid_with_options(&config, mid, equity, req.max_leverage).map_err(|e| e.to_string())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EstimateDynamicBoundsRequest {
    symbol: String,
    #[serde(default)]
    atr_interval: Option<String>,
    #[serde(default)]
    atr_period: Option<u32>,
    #[serde(default)]
    atr_mult: Option<String>,
    #[serde(default)]
    mid_price: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EstimateDynamicBoundsResponse {
    lower_price: String,
    upper_price: String,
    mid_price: String,
    atr: String,
    atr_pct: String,
    half_width_pct: String,
}

#[tauri::command]
async fn estimate_dynamic_bounds(
    state: State<'_, Arc<Mutex<AppState>>>,
    req: EstimateDynamicBoundsRequest,
) -> Result<EstimateDynamicBoundsResponse, String> {
    let st = state.lock().await;
    let mid = match req.mid_price.as_deref() {
        Some(s) if !s.trim().is_empty() => dec(s)?,
        _ => fetch_live_mid(st.mode, &req.symbol)
            .await
            .map_err(|e| e.to_string())?,
    };
    let atr_interval = req
        .atr_interval
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "1h".into());
    let atr_period = req.atr_period.unwrap_or(14).max(2);
    let atr_mult = req
        .atr_mult
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(dec)
        .transpose()?
        .unwrap_or_else(|| Decimal::from(5));
    let mut config = GridConfig {
        symbol: req.symbol.clone(),
        lower_price: Decimal::ZERO,
        upper_price: Decimal::ZERO,
        grid_count: 10,
        total_budget: Decimal::ONE,
        spacing: GridSpacing::Arithmetic,
        breakout_action: BreakoutAction::Recenter,
        max_drawdown_pct: Decimal::ZERO,
        max_daily_loss: Decimal::ZERO,
        max_order_failures: 5,
        market: MarketKind::Perp,
        leverage: 5,
        is_cross: true,
        grid_mode: GridMode::Dynamic,
        dynamic: DynamicGridConfig {
            atr_interval,
            atr_period,
            atr_mult,
            ..DynamicGridConfig::default()
        },
    };
    let metrics = resolve_dynamic_bounds(st.mode, &mut config, mid)
        .await?
        .ok_or_else(|| "dynamic bounds unavailable".to_string())?;
    let half = grid_engine::suggest_half_width_pct(metrics.atr_pct, config.dynamic.atr_mult)
        .max(config.dynamic.min_half_width_pct)
        .min(config.dynamic.max_half_width_pct);
    Ok(EstimateDynamicBoundsResponse {
        lower_price: config.lower_price.normalize().to_string(),
        upper_price: config.upper_price.normalize().to_string(),
        mid_price: mid.normalize().to_string(),
        atr: metrics.atr.normalize().to_string(),
        atr_pct: metrics.atr_pct.normalize().to_string(),
        half_width_pct: half.normalize().to_string(),
    })
}

#[tauri::command]
async fn set_mode(state: State<'_, Arc<Mutex<AppState>>>, mode: String) -> Result<(), String> {
    let mut st = state.lock().await;
    let next = parse_mode(&mode);
    if st.running_task && next != st.mode {
        return Err(i18n("botRunningMode"));
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
        return Err(i18n("botRunningKey"));
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
        hl.set_private_key(&private_key)
            .map_err(|e| e.to_string())?;
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
async fn get_account(state: State<'_, Arc<Mutex<AppState>>>) -> Result<serde_json::Value, String> {
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
        // Do not full-refresh meta on every balance poll — a partial HIP-3 failure
        // used to wipe xyz:* from asset_index and break live place/cancel.
        let _ = hl.ensure_connected().await;
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
    let mode = state.lock().await.mode;
    list_live_markets(mode).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_market_mids(
    state: State<'_, Arc<Mutex<AppState>>>,
) -> Result<std::collections::HashMap<String, String>, String> {
    let mode = state.lock().await.mode;
    let mids = list_live_mids(mode).await.map_err(|e| e.to_string())?;
    Ok(mids
        .into_iter()
        .map(|(k, v)| (k, v.normalize().to_string()))
        .collect())
}

#[tauri::command]
async fn list_symbols(state: State<'_, Arc<Mutex<AppState>>>) -> Result<Vec<String>, String> {
    let mode = state.lock().await.mode;
    let markets = list_live_markets(mode)
        .await
        .map_err(|e| e.to_string())?;
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
            return Err(i18n("botAlreadyRunning"));
        }
        if st.mode != RunMode::Simulation && st.private_key.trim().is_empty() {
            return Err(i18n("privateKeyRequired"));
        }
        let app_cfg = st.storage.load_config().map_err(|e| e.to_string())?;
        let runner_req = runner::StartRequest {
            symbol: req.symbol.clone(),
            lower_price: req.lower_price.clone(),
            upper_price: req.upper_price.clone(),
            grid_count: req.grid_count,
            total_budget: req.total_budget.clone(),
            spacing: req.spacing.clone(),
            breakout_action: req.breakout_action.clone(),
            max_drawdown_pct: req.max_drawdown_pct.clone(),
            max_daily_loss: req.max_daily_loss.clone(),
            max_order_failures: req.max_order_failures,
            leverage: req.leverage,
            is_cross: req.is_cross,
            grid_mode: req.grid_mode.clone(),
            atr_interval: req.atr_interval.clone(),
            atr_period: req.atr_period,
            atr_mult: req.atr_mult.clone(),
            confirm_bars: req.confirm_bars,
            recenter_cooldown_secs: req.recenter_cooldown_secs,
            max_recenters_per_day: req.max_recenters_per_day,
        };
        let mut config = build_grid_config(&runner_req, &app_cfg)?;
        let mid = if st.mode == RunMode::Simulation {
            let live = fetch_live_mid(st.mode, &config.symbol)
                .await
                .unwrap_or_else(|_| {
                    if config.lower_price > Decimal::ZERO && config.upper_price > config.lower_price {
                        (config.lower_price + config.upper_price) / Decimal::from(2)
                    } else {
                        Decimal::from(100_000)
                    }
                });
            if config.is_dynamic() {
                resolve_dynamic_bounds(RunMode::Testnet, &mut config, live).await?;
            }
            if live <= config.lower_price || live >= config.upper_price {
                return Err(i18n_kv(
                    "midOutOfRange",
                    &[
                        ("mid", live.to_string()),
                        ("lower", config.lower_price.to_string()),
                        ("upper", config.upper_price.to_string()),
                    ],
                ));
            }
            let seed = live;
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
            // Cancel sim leftovers for this symbol only (preserve other state).
            protect_symbol(&mut st, &config.symbol, false)
                .await
                .map_err(|e| {
                    if e.starts_with("i18n:") {
                        e
                    } else {
                        i18n_kv("cancelBeforeStartFailed", &[("detail", e)])
                    }
                })?;
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
            let live_mid = st
                .hl
                .as_mut()
                .unwrap()
                .get_mid(&config.symbol)
                .await
                .map_err(|e| e.to_string())?;
            if config.is_dynamic() {
                resolve_dynamic_bounds(st.mode, &mut config, live_mid).await?;
            }
            if live_mid <= config.lower_price || live_mid >= config.upper_price {
                return Err(i18n_kv(
                    "midOutOfRange",
                    &[
                        ("mid", live_mid.to_string()),
                        ("lower", config.lower_price.to_string()),
                        ("upper", config.upper_price.to_string()),
                    ],
                ));
            }
            // Cancel only this strategy symbol — never flatten the whole account.
            protect_symbol(&mut st, &config.symbol, false)
                .await
                .map_err(|e| {
                    if e.starts_with("i18n:") {
                        e
                    } else {
                        i18n_kv("cancelSymbolBeforeStartFailed", &[("detail", e)])
                    }
                })?;
            let hl = st.hl.as_mut().unwrap();
            hl.set_leverage(&config.symbol, config.leverage, config.is_cross)
                .await
                .map_err(|e| e.to_string())?;
            live_mid
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

        let mut engine = GridEngine::new(config.clone(), st.mode, config.total_budget)
            .map_err(|e| e.to_string())?;
        let intents = engine.bootstrap_intents(mid).map_err(|e| e.to_string())?;

        let placed = if st.mode == RunMode::Simulation {
            st.sim
                .as_mut()
                .unwrap()
                .place_orders(intents)
                .await
                .map_err(|e| e.to_string())?
        } else {
            let hl = st.hl.as_mut().unwrap();
            if let Err(e) = hl.preflight_grid_notional(&intents, config.leverage).await {
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
        let sid = engine.session_id().to_string();
        let cfg_json = serde_json::to_string(&config).unwrap_or_else(|_| "{}".into());
        let _ = st.storage.upsert_bot_session(
            &sid,
            "grid",
            &config.symbol,
            "running",
            &cfg_json,
            true,
        );
        runner::persist_checkpoint(&st.storage, &engine, "started");
        let snap = engine.snapshot();
        st.engine = Some(engine);
        st.running_task = true;
        let _ = st.storage.record_event("start", "bot started");
        let _ = app.emit("bot-status", &snap);
    }

    let app2 = app.clone();
    let state2 = state_arc.clone();
    tauri::async_runtime::spawn(async move {
        run_loop(app2, state2).await;
    });

    let st = state_arc.lock().await;
    return Ok(st
        .engine
        .as_ref()
        .map(|e| e.snapshot())
        .unwrap_or_else(|| idle_snapshot(RunMode::Simulation, req.symbol)));
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
        let _ = st
            .storage
            .record_event("flatten", "canceled orders and closed positions");
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
    flatten_account_notify(&app, &mut st, &reason).await
}

#[tauri::command]
async fn stop_bot(
    app: AppHandle,
    state: State<'_, Arc<Mutex<AppState>>>,
) -> Result<BotSnapshot, String> {
    let mut st = state.lock().await;
    st.running_task = false;
    let symbol = st
        .engine
        .as_ref()
        .map(|e| e.config.symbol.clone())
        .unwrap_or_default();
    let snap = {
        if let Some(engine) = st.engine.as_mut() {
            let _ = engine.stop();
            engine.snapshot()
        } else {
            idle_snapshot(st.mode, symbol.clone())
        }
    };
    if symbol.is_empty() {
        return Ok(snap);
    }
    // Stop = cancel strategy symbol orders + close that symbol position only.
    let _ = app.emit(
        "flatten-start",
        FlattenStartPayload {
            reason: "stop".into(),
        },
    );
    let res = protect_symbol(&mut st, &symbol, true).await;
    let _ = app.emit(
        "flatten-end",
        FlattenEndPayload {
            reason: "stop".into(),
            ok: res.is_ok(),
            error: res.as_ref().err().cloned(),
        },
    );
    if let Err(e) = res {
        error!("stop protect_symbol failed: {e}");
        if e.starts_with("i18n:") {
            return Err(e);
        }
        return Err(i18n_kv("stopFlattenFailed", &[("detail", e)]));
    }
    if let Some(engine) = st.engine.as_mut() {
        engine.note("stopped: canceled symbol orders & closed position");
        let sid = engine.session_id().to_string();
        let payload = engine.checkpoint_payload();
        let cfg_json = serde_json::to_string(&engine.config).unwrap_or_else(|_| "{}".into());
        // Session history uses `stopped` (clearer than engine Idle) for 中英文 UI.
        let status = "stopped";
        let _ = st.storage.save_checkpoint(&sid, "stopped", &payload);
        let _ = st
            .storage
            .upsert_bot_session(&sid, "grid", &symbol, status, &cfg_json, false);
        let _ = st.storage.deactivate_session(&sid, Some(status));
    }
    let _ = st
        .storage
        .record_event("stop", "symbol orders canceled and position closed");
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
async fn clear_analytics(
    state: State<'_, Arc<Mutex<AppState>>>,
    session_id: Option<String>,
    all: Option<bool>,
) -> Result<serde_json::Value, String> {
    let st = state.lock().await;
    let wipe_all = all.unwrap_or(false);
    let sid = if wipe_all {
        None
    } else {
        session_id
            .filter(|s| !s.is_empty())
            .or_else(|| st.engine.as_ref().map(|e| e.session_id().to_string()))
    };
    if !wipe_all && sid.is_none() {
        return Err(i18n("noSessionToClear"));
    }
    let cleared = st
        .storage
        .clear_analytics(sid.as_deref())
        .map_err(|e| e.to_string())?;
    let _ = st.storage.record_event(
        "analytics_cleared",
        &if wipe_all {
            format!("cleared all analytics ({cleared} rows)")
        } else {
            format!(
                "cleared analytics for session {} ({cleared} rows)",
                sid.as_deref().unwrap_or("?")
            )
        },
    );
    Ok(serde_json::json!({
        "cleared": cleared,
        "session_id": sid,
        "all": wipe_all,
    }))
}

#[tauri::command]
async fn list_fills(
    state: State<'_, Arc<Mutex<AppState>>>,
    limit: usize,
) -> Result<Vec<FillRow>, String> {
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

#[tauri::command]
async fn get_session_pnl(
    state: State<'_, Arc<Mutex<AppState>>>,
    session_id: Option<String>,
    all: Option<bool>,
) -> Result<Option<SessionPnlSummary>, String> {
    let st = state.lock().await;
    if all.unwrap_or(false) {
        return st
            .storage
            .all_sessions_pnl_summary()
            .map(Some)
            .map_err(|e| e.to_string());
    }
    let sid = session_id
        .filter(|s| !s.is_empty())
        .or_else(|| st.engine.as_ref().map(|e| e.session_id().to_string()));
    let Some(sid) = sid else {
        return Ok(None);
    };
    st.storage
        .session_pnl_summary(&sid)
        .map(Some)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_sessions(
    state: State<'_, Arc<Mutex<AppState>>>,
    limit: Option<usize>,
) -> Result<Vec<SessionListItem>, String> {
    let st = state.lock().await;
    st.storage
        .list_session_summaries(limit.unwrap_or(50).clamp(1, 200))
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_daily_pnl(
    state: State<'_, Arc<Mutex<AppState>>>,
    session_id: Option<String>,
    days: Option<u32>,
) -> Result<Vec<DailyPnlRow>, String> {
    let st = state.lock().await;
    let sid = session_id.filter(|s| !s.is_empty());
    st.storage
        .daily_pnl(sid.as_deref(), days.unwrap_or(30))
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_equity_curve(
    state: State<'_, Arc<Mutex<AppState>>>,
    session_id: Option<String>,
    days: Option<u32>,
    all: Option<bool>,
    limit: Option<usize>,
) -> Result<Vec<EquitySnapshotRow>, String> {
    let st = state.lock().await;
    let lim = limit.unwrap_or(800).clamp(10, 5000);
    if all.unwrap_or(false) {
        let since = days.map(|d| {
            let d = d.clamp(1, 365) as i64;
            chrono::Utc::now().timestamp_millis() - d * 86_400_000
        });
        return st
            .storage
            .list_equity_curve_all(since, lim)
            .map_err(|e| e.to_string());
    }
    let sid = session_id
        .filter(|s| !s.is_empty())
        .or_else(|| st.engine.as_ref().map(|e| e.session_id().to_string()));
    let Some(sid) = sid else {
        return Ok(vec![]);
    };
    let since = days.map(|d| {
        let d = d.clamp(1, 365) as i64;
        chrono::Utc::now().timestamp_millis() - d * 86_400_000
    });
    st.storage
        .list_equity_snapshots_range(&sid, since, lim, true)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn export_analytics_pack(
    state: State<'_, Arc<Mutex<AppState>>>,
    session_id: Option<String>,
    dir: Option<String>,
) -> Result<serde_json::Value, String> {
    let st = state.lock().await;
    let sid = session_id
        .filter(|s| !s.is_empty())
        .or_else(|| st.engine.as_ref().map(|e| e.session_id().to_string()));
    let out_dir = if let Some(d) = dir.filter(|s| !s.is_empty()) {
        PathBuf::from(d)
    } else {
        // `<program_dir>/data/analytics/...`
        let base = resolve_data_dir().join("analytics");
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let name = match &sid {
            Some(s) => format!("{}-{}", stamp, &s[..s.len().min(8)]),
            None => format!("{stamp}-all"),
        };
        base.join(name)
    };
    let pack = st
        .storage
        .export_analytics_pack(&out_dir, sid.as_deref())
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "path": out_dir.to_string_lossy(),
        "pack": pack,
    }))
}

#[derive(Debug, Serialize, Deserialize)]
struct ExportConfig {
    #[serde(default = "default_schema_version")]
    schema_version: u32,
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
    #[serde(default = "default_grid_mode_export")]
    grid_mode: String,
    #[serde(default)]
    atr_interval: Option<String>,
    #[serde(default)]
    atr_period: Option<u32>,
    #[serde(default)]
    atr_mult: Option<String>,
    #[serde(default)]
    confirm_bars: Option<u32>,
    #[serde(default)]
    recenter_cooldown_secs: Option<u64>,
    #[serde(default)]
    max_recenters_per_day: Option<u32>,
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
fn default_schema_version() -> u32 {
    2
}
fn default_grid_mode_export() -> String {
    "dynamic".into()
}

#[tauri::command]
fn export_strategy_config(mut cfg: ExportConfig) -> Result<String, String> {
    cfg.schema_version = 2;
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
async fn set_language(
    state: State<'_, Arc<Mutex<AppState>>>,
    language: String,
) -> Result<(), String> {
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
        return Err(i18n("botRunningMode"));
    }
    if st.running_task && key_changed {
        return Err(i18n("botRunningKey"));
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
    tracing_subscriber::fmt().with_env_filter("info").init();

    let state = AppState::new().expect("storage");
    let state = Arc::new(Mutex::new(state));
    let state_for_exit = state.clone();
    let state_for_startup = state.clone();

    tauri::Builder::default()
        .manage(state)
        .setup(move |app| {
            let handle = app.handle().clone();
            let state_for_loop = state_for_startup.clone();
            // Resume preserved session; never auto-flatten on launch.
            tauri::async_runtime::spawn(async move {
                let mut st = state_for_startup.lock().await;
                match try_resume_active_session(&handle, &mut st).await {
                    Ok(true) => {
                        info!("resumed active session from checkpoint");
                        let running = st.running_task;
                        drop(st);
                        if running {
                            let app2 = handle.clone();
                            tauri::async_runtime::spawn(async move {
                                run_loop(app2, state_for_loop).await;
                            });
                        }
                    }
                    Ok(false) => {
                        info!("no resumable session; waiting for user start");
                    }
                    Err(e) => {
                        error!("session resume failed: {e}");
                        // Belt-and-suspenders: never leave running_task stuck after
                        // a failed live resume (blocks Start → looks like a freeze).
                        st.running_task = false;
                        let _ = handle.emit(
                            "bot-alert",
                            serde_json::json!({
                                "kind": "resume_failed",
                                "reason": e
                            }),
                        );
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            greet,
            preview_grid_cmd,
            estimate_dynamic_bounds,
            set_mode,
            set_private_key,
            get_account,
            list_symbols,
            list_markets,
            list_market_mids,
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
            clear_analytics,
            export_fills_csv,
            get_session_pnl,
            list_sessions,
            get_daily_pnl,
            list_equity_curve,
            export_analytics_pack,
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
                // Preserve exchange orders & position; only persist checkpoint.
                let state = state_for_exit.clone();
                let _ = std::thread::spawn(move || {
                    if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        rt.block_on(async move {
                            let mut st = state.lock().await;
                            let was_sim = st.mode == RunMode::Simulation;
                            detach_on_exit(&mut st);
                            if was_sim {
                                info!("exit: simulation session ended");
                            } else {
                                info!("exit: session detached (orders/position preserved)");
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

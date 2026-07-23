use std::collections::HashMap;
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use grid_engine::{FillEvent, LiveOrder, OrderIntent, RunMode, Side};
use k256::ecdsa::{RecoveryId, Signature, SigningKey};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::{json, Value};
use sha3::{Digest, Keccak256};
use tracing::{info, warn};

use crate::traits::{Balance, Exchange, ExchangeError, ExchangeResult, MarketInfo};

#[derive(Clone)]
pub struct HyperliquidExchange {
    mode: RunMode,
    base_url: String,
    client: reqwest::Client,
    private_key: Option<String>,
    address: Option<String>,
    open_orders: HashMap<String, LiveOrder>,
    asset_index: HashMap<String, u32>,
    /// Perp/spot size decimals by symbol
    sz_decimals: HashMap<String, u32>,
    /// label/alias -> allMids key (e.g. HFUN/USDC -> @1)
    mid_aliases: HashMap<String, String>,
    last_seen_fills: Vec<String>,
    /// After priming, historical exchange fills are ignored.
    fills_primed: bool,
    /// Only emit fills at/after this exchange timestamp (ms).
    session_start_ms: u64,
}

impl HyperliquidExchange {
    pub fn new(mode: RunMode) -> Self {
        let base_url = match mode {
            RunMode::Mainnet => "https://api.hyperliquid.xyz".to_string(),
            RunMode::Testnet => "https://api.hyperliquid-testnet.xyz".to_string(),
            RunMode::Simulation => "https://api.hyperliquid-testnet.xyz".to_string(),
        };
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            mode,
            base_url,
            client,
            private_key: None,
            address: None,
            open_orders: HashMap::new(),
            asset_index: HashMap::new(),
            sz_decimals: HashMap::new(),
            mid_aliases: HashMap::new(),
            last_seen_fills: Vec::new(),
            fills_primed: false,
            session_start_ms: 0,
        }
    }

    pub fn set_private_key(&mut self, key: &str) -> ExchangeResult<()> {
        let key = key.trim().trim_start_matches("0x");
        let bytes = hex::decode(key).map_err(|e| ExchangeError::InvalidKey(e.to_string()))?;
        if bytes.len() != 32 {
            return Err(ExchangeError::InvalidKey(
                "private key must be 32 bytes hex".into(),
            ));
        }
        let signing_key = SigningKey::from_bytes((&bytes[..]).into())
            .map_err(|e| ExchangeError::InvalidKey(e.to_string()))?;
        let verifying = signing_key.verifying_key();
        let point = verifying.to_encoded_point(false);
        let hash = Keccak256::digest(&point.as_bytes()[1..]);
        let addr = format!("0x{}", hex::encode(&hash[12..]));
        self.private_key = Some(key.to_string());
        self.address = Some(addr);
        Ok(())
    }

    pub fn address(&self) -> Option<&str> {
        self.address.as_deref()
    }

    /// Re-attach bot orders after the exchange client was accidentally recreated.
    pub fn restore_tracked_orders(&mut self, orders: &[LiveOrder]) {
        for order in orders {
            if order.exchange_id.is_none() {
                continue;
            }
            self.open_orders
                .entry(order.client_id.clone())
                .or_insert_with(|| order.clone());
        }
    }

    /// Account equity available for sizing max position (USDC).
    pub async fn account_equity_usdc(&self) -> ExchangeResult<Decimal> {
        let mut equity = Decimal::ZERO;
        if let Some(addr) = &self.address {
            if let Ok(state) = self
                .post_info(json!({"type": "clearinghouseState", "user": addr}))
                .await
            {
                for key in ["crossMarginSummary", "marginSummary"] {
                    if let Some(v) = state
                        .get(key)
                        .and_then(|m| m.get("accountValue"))
                        .and_then(|v| v.as_str())
                        .and_then(|s| Decimal::from_str(s).ok())
                    {
                        equity = equity.max(v);
                    }
                }
                if let Some(w) = state
                    .get("withdrawable")
                    .and_then(|v| v.as_str())
                    .and_then(|s| Decimal::from_str(s).ok())
                {
                    equity = equity.max(w);
                }
            }
        }
        // Unified accounts often keep equity in spot USDC.
        if let Ok(bals) = self.get_balances().await {
            for b in bals {
                if b.asset.eq_ignore_ascii_case("USDC") {
                    equity = equity.max(b.available).max(b.total);
                }
            }
        }
        Ok(equity)
    }

    /// Rough max one-sided position notional at `leverage` (with a small safety buffer).
    pub async fn max_side_notional(&self, leverage: u32) -> ExchangeResult<Decimal> {
        let equity = self.account_equity_usdc().await?;
        let lev = Decimal::from(leverage.max(1));
        // 90% buffer for fees / mark movement / open-order haircut.
        Ok((equity * lev * dec!(0.90)).round_dp(2))
    }

    /// Reject grid intents early if either side would exceed leverage position cap.
    pub async fn preflight_grid_notional(
        &self,
        intents: &[OrderIntent],
        leverage: u32,
    ) -> ExchangeResult<()> {
        let mut buy_ntl = Decimal::ZERO;
        let mut sell_ntl = Decimal::ZERO;
        for i in intents {
            let n = i.price * i.size;
            match i.side {
                Side::Buy => buy_ntl += n,
                Side::Sell => sell_ntl += n,
            }
        }
        let max_side = self.max_side_notional(leverage).await?;
        let equity = self.account_equity_usdc().await?;
        let worst = buy_ntl.max(sell_ntl);
        if max_side <= Decimal::ZERO {
            return Err(ExchangeError::Other(format!(
                "账户可用保证金不足（约 {equity} USDC），无法按 {leverage}x 挂网格。请先充值。"
            )));
        }
        if worst > max_side {
            // total_budget ≈ buy+sell ≈ 2 * side for a balanced grid
            let suggest_total = (max_side * dec!(2) * dec!(0.95)).round_dp(0);
            return Err(ExchangeError::Other(format!(
                "网格单边名义约 {worst} USDC，超过当前 {leverage}x 杠杆允许的约 {max_side} USDC \
（保证金约 {equity} USDC）。请把「总名义投入」降到约 {suggest_total} 以下，或降低杠杆 / 增加保证金。"
            )));
        }
        Ok(())
    }

    /// Live perp position for `symbol` from clearinghouse (signed size, entry, uPnL).
    pub async fn get_perp_position(
        &self,
        symbol: &str,
    ) -> ExchangeResult<(Decimal, Option<Decimal>, Option<Decimal>)> {
        let addr = self
            .address
            .as_ref()
            .ok_or(ExchangeError::NotConnected)?;
        let state = self
            .post_info(json!({"type": "clearinghouseState", "user": addr}))
            .await
            .unwrap_or(json!({}));
        let mut size = Decimal::ZERO;
        let mut entry = None;
        let mut upnl = None;
        if let Some(positions) = state.get("assetPositions").and_then(|a| a.as_array()) {
            for p in positions {
                let pos = match p.get("position") {
                    Some(x) => x,
                    None => continue,
                };
                let coin = pos.get("coin").and_then(|c| c.as_str()).unwrap_or("");
                if !coin.eq_ignore_ascii_case(symbol) {
                    continue;
                }
                size = pos
                    .get("szi")
                    .and_then(|s| s.as_str())
                    .and_then(|s| Decimal::from_str(s).ok())
                    .unwrap_or(Decimal::ZERO);
                entry = pos
                    .get("entryPx")
                    .and_then(|s| s.as_str())
                    .and_then(|s| Decimal::from_str(s).ok())
                    .filter(|px| *px > Decimal::ZERO);
                upnl = pos
                    .get("unrealizedPnl")
                    .and_then(|s| s.as_str())
                    .and_then(|s| Decimal::from_str(s).ok());
                break;
            }
        }
        Ok((size, entry, upnl))
    }

    /// Seed fill dedupe from current exchange history so old trades are not
    /// treated as new fills when the bot starts.
    pub async fn prime_seen_fills(&mut self) -> ExchangeResult<()> {
        let addr = match &self.address {
            Some(a) => a.clone(),
            None => {
                self.fills_primed = true;
                self.session_start_ms = Self::now_ms();
                return Ok(());
            }
        };
        self.session_start_ms = Self::now_ms();
        let fills = self
            .post_info(json!({
                "type": "userFills",
                "user": addr
            }))
            .await
            .unwrap_or(json!([]));
        if let Some(arr) = fills.as_array() {
            for f in arr {
                let tid = f
                    .get("tid")
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| f.to_string());
                if !self.last_seen_fills.contains(&tid) {
                    self.last_seen_fills.push(tid);
                }
            }
            if self.last_seen_fills.len() > 500 {
                let excess = self.last_seen_fills.len() - 400;
                self.last_seen_fills.drain(0..excess);
            }
            info!(
                "primed {} historical fill id(s); ignoring them as new",
                arr.len()
            );
        }
        self.fills_primed = true;
        Ok(())
    }

    async fn post_info(&self, body: Value) -> ExchangeResult<Value> {
        let url = format!("{}/info", self.base_url);
        let res = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| ExchangeError::Api(e.to_string()))?;
        let status = res.status();
        let text = res
            .text()
            .await
            .map_err(|e| ExchangeError::Api(e.to_string()))?;
        if !status.is_success() {
            return Err(ExchangeError::Api(format!("{status}: {text}")));
        }
        serde_json::from_str(&text).map_err(|e| ExchangeError::Api(e.to_string()))
    }

    async fn refresh_spot_meta(&mut self) -> ExchangeResult<()> {
        let mut map = HashMap::new();
        let mut aliases = HashMap::new();
        let mut sz_decimals = HashMap::new();

        // Perp names first so bare symbols like BTC keep the perpetual asset / mid,
        // instead of being stolen by a spot token also named BTC (common on testnet).
        if let Ok(perp) = self.post_info(json!({"type": "meta"})).await {
            if let Some(universe) = perp.get("universe").and_then(|u| u.as_array()) {
                for (i, item) in universe.iter().enumerate() {
                    if let Some(name) = item.get("name").and_then(|n| n.as_str()) {
                        map.insert(name.to_string(), i as u32);
                        let decs = item
                            .get("szDecimals")
                            .and_then(|d| d.as_u64())
                            .unwrap_or(4) as u32;
                        sz_decimals.insert(name.to_string(), decs);
                    }
                }
            }
        }

        let meta = self.post_info(json!({"type": "spotMeta"})).await?;
        let tokens: HashMap<u64, String> = meta
            .get("tokens")
            .and_then(|t| t.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|tok| {
                        let idx = tok.get("index")?.as_u64()?;
                        let name = tok.get("name")?.as_str()?.to_string();
                        Some((idx, name))
                    })
                    .collect()
            })
            .unwrap_or_default();

        if let Some(universe) = meta.get("universe").and_then(|u| u.as_array()) {
            for (i, item) in universe.iter().enumerate() {
                if let Some(name) = item.get("name").and_then(|n| n.as_str()) {
                    let spot_id = 10_000u32 + i as u32;
                    map.insert(name.to_string(), spot_id);
                    if let Some(arr) = item.get("tokens").and_then(|t| t.as_array()) {
                        if let (Some(b), Some(q)) = (
                            arr.first().and_then(|x| x.as_u64()),
                            arr.get(1).and_then(|x| x.as_u64()),
                        ) {
                            let base = tokens.get(&b).cloned().unwrap_or_else(|| format!("T{b}"));
                            let quote = tokens.get(&q).cloned().unwrap_or_else(|| "USDC".into());
                            let label = format!("{base}/{quote}");
                            aliases.insert(label.clone(), name.to_string());
                            map.insert(label, spot_id);
                            // Only alias bare base (e.g. BTC -> @50) when it does not
                            // collide with an existing perp coin name.
                            if !map.contains_key(&base) {
                                aliases.insert(base.clone(), name.to_string());
                                map.insert(base, spot_id);
                            }
                        }
                    }
                }
            }
        }
        self.asset_index = map;
        self.mid_aliases = aliases;
        self.sz_decimals = sz_decimals;
        Ok(())
    }

    fn resolve_mid_key(&self, symbol: &str) -> String {
        self.mid_aliases
            .get(symbol)
            .cloned()
            .unwrap_or_else(|| symbol.to_string())
    }

    /// Coin id for candleSnapshot — same key as allMids (perp name or spot @n).
    fn resolve_candle_coin(&self, symbol: &str) -> String {
        self.resolve_mid_key(symbol)
    }

    fn resolve_asset(&self, symbol: &str) -> ExchangeResult<u32> {
        self.asset_index
            .get(symbol)
            .copied()
            .ok_or_else(|| ExchangeError::Other(format!("unknown symbol {symbol}")))
    }

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    fn sign_l1_action(&self, action: &Value, nonce: u64) -> ExchangeResult<(String, String, String)> {
        let key_hex = self
            .private_key
            .as_ref()
            .ok_or(ExchangeError::NotConnected)?;
        let connection_id = action_hash(action, nonce, None)?;
        let is_mainnet = matches!(self.mode, RunMode::Mainnet);
        let source = if is_mainnet { "a" } else { "b" };

        // EIP-712 Agent(source, connectionId)
        let domain_type_hash = keccak(
            b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
        );
        let name_hash = keccak(b"Exchange");
        let version_hash = keccak(b"1");
        let mut domain = Vec::new();
        domain.extend_from_slice(&domain_type_hash);
        domain.extend_from_slice(&name_hash);
        domain.extend_from_slice(&version_hash);
        domain.extend_from_slice(&u256_bytes(1337));
        domain.extend_from_slice(&[0u8; 32]); // verifyingContract = 0
        let domain_separator = keccak(&domain);

        let agent_type_hash = keccak(b"Agent(string source,bytes32 connectionId)");
        let source_hash = keccak(source.as_bytes());
        let mut msg = Vec::new();
        msg.extend_from_slice(&agent_type_hash);
        msg.extend_from_slice(&source_hash);
        msg.extend_from_slice(&connection_id);
        let struct_hash = keccak(&msg);

        let mut digest_input = Vec::with_capacity(66);
        digest_input.extend_from_slice(&[0x19, 0x01]);
        digest_input.extend_from_slice(&domain_separator);
        digest_input.extend_from_slice(&struct_hash);
        let digest = keccak(&digest_input);

        let signing_key = SigningKey::from_bytes((&hex::decode(key_hex).unwrap()[..]).into())
            .map_err(|e| ExchangeError::InvalidKey(e.to_string()))?;
        let recoverable = signing_key
            .sign_prehash_recoverable(digest.as_slice())
            .map_err(|e| ExchangeError::Other(e.to_string()))?;
        let (sig, recid): (Signature, RecoveryId) = recoverable;
        let sig_bytes = sig.to_bytes();
        let r = hex::encode(&sig_bytes[..32]);
        let s = hex::encode(&sig_bytes[32..64]);
        let v = format!("{}", 27 + recid.to_byte());
        Ok((r, s, v))
    }

    async fn post_exchange(&self, action: Value) -> ExchangeResult<Value> {
        let nonce = Self::now_ms();
        let (r, s, v) = self.sign_l1_action(&action, nonce)?;
        let body = json!({
            "action": action,
            "nonce": nonce,
            "signature": { "r": format!("0x{r}"), "s": format!("0x{s}"), "v": v.parse::<u64>().unwrap_or(28) },
        });
        let url = format!("{}/exchange", self.base_url);
        let res = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| ExchangeError::Api(e.to_string()))?;
        let status = res.status();
        let text = res
            .text()
            .await
            .map_err(|e| ExchangeError::Api(e.to_string()))?;
        if !status.is_success() {
            return Err(ExchangeError::Api(format!("{status}: {text}")));
        }
        serde_json::from_str(&text).map_err(|e| ExchangeError::Api(format!("{e}: {text}")))
    }

    /// Set leverage for a perpetual coin before trading.
    pub async fn set_leverage(
        &self,
        symbol: &str,
        leverage: u32,
        is_cross: bool,
    ) -> ExchangeResult<()> {
        let asset = self.resolve_asset(symbol)?;
        let action = json!({
            "type": "updateLeverage",
            "asset": asset,
            "isCross": is_cross,
            "leverage": leverage
        });
        let resp = self.post_exchange(action).await?;
        if resp.get("status").and_then(|s| s.as_str()) != Some("ok") {
            return Err(ExchangeError::Api(friendly_hl_error(
                &resp,
                self.address.as_deref(),
            )));
        }
        Ok(())
    }
}

fn keccak(data: &[u8]) -> [u8; 32] {
    let mut hasher = Keccak256::new();
    hasher.update(data);
    let out = hasher.finalize();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    arr
}

fn u256_bytes(v: u64) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[24..].copy_from_slice(&v.to_be_bytes());
    out
}

fn action_hash(action: &Value, nonce: u64, vault: Option<[u8; 20]>) -> ExchangeResult<[u8; 32]> {
    // Key order in msgpack MUST match Hyperliquid's wire schema (same as Python SDK).
    // rmp_serde on serde_json::Value sorts map keys alphabetically and breaks recovery.
    let packed = pack_action_msgpack(action)?;
    let mut buf = packed;
    buf.extend_from_slice(&nonce.to_be_bytes());
    if let Some(v) = vault {
        buf.push(1);
        buf.extend_from_slice(&v);
    } else {
        buf.push(0);
    }
    Ok(keccak(&buf))
}

fn pack_action_msgpack(action: &Value) -> ExchangeResult<Vec<u8>> {
    let mp = json_to_mp_ordered(action)?;
    let mut buf = Vec::new();
    rmpv::encode::write_value(&mut buf, &mp)
        .map_err(|e| ExchangeError::Other(format!("msgpack encode: {e}")))?;
    Ok(buf)
}

fn json_to_mp_ordered(v: &Value) -> ExchangeResult<rmpv::Value> {
    match v {
        Value::Null => Ok(rmpv::Value::Nil),
        Value::Bool(b) => Ok(rmpv::Value::Boolean(*b)),
        Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                Ok(rmpv::Value::Integer(u.into()))
            } else if let Some(i) = n.as_i64() {
                Ok(rmpv::Value::Integer(i.into()))
            } else if let Some(f) = n.as_f64() {
                Ok(rmpv::Value::F64(f))
            } else {
                Err(ExchangeError::Other("invalid number".into()))
            }
        }
        Value::String(s) => Ok(rmpv::Value::String(s.as_str().into())),
        Value::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for item in arr {
                if item.is_object() {
                    out.push(json_object_ordered(item, order_or_cancel_keys(item))?);
                } else {
                    out.push(json_to_mp_ordered(item)?);
                }
            }
            Ok(rmpv::Value::Array(out))
        }
        Value::Object(_) => {
            let keys = top_level_action_keys(v);
            json_object_ordered(v, keys)
        }
    }
}

fn top_level_action_keys(action: &Value) -> &'static [&'static str] {
    match action.get("type").and_then(|t| t.as_str()) {
        Some("order") => &["type", "orders", "grouping", "builder"],
        Some("cancel") => &["type", "cancels"],
        Some("cancelByCloid") => &["type", "cancels"],
        Some("batchModify") => &["type", "modifies"],
        Some("modify") => &["type", "oid", "order"],
        Some("updateLeverage") => &["type", "asset", "isCross", "leverage"],
        Some("scheduleCancel") => &["type", "time"],
        _ => &["type"],
    }
}

fn order_or_cancel_keys(item: &Value) -> &'static [&'static str] {
    if item.get("o").is_some() && item.get("a").is_some() && item.get("p").is_none() {
        // cancel wire {a, o}
        &["a", "o"]
    } else if item.get("p").is_some() {
        // order wire
        &["a", "b", "p", "s", "r", "t", "c"]
    } else if item.get("cloid").is_some() {
        &["asset", "cloid"]
    } else {
        &["a", "b", "p", "s", "r", "t", "c"]
    }
}

fn json_object_ordered(v: &Value, key_order: &[&str]) -> ExchangeResult<rmpv::Value> {
    let obj = v
        .as_object()
        .ok_or_else(|| ExchangeError::Other("expected object".into()))?;
    let mut pairs = Vec::new();
    let mut used = std::collections::HashSet::new();
    for key in key_order {
        if let Some(child) = obj.get(*key) {
            // Skip null optional fields (e.g. absent builder)
            if child.is_null() {
                continue;
            }
            used.insert(*key);
            let mp_child = if *key == "t" {
                // order type: {"limit":{"tif":"Gtc"}} or trigger
                pack_order_type(child)?
            } else if *key == "orders" || *key == "cancels" || *key == "modifies" {
                json_to_mp_ordered(child)?
            } else if *key == "order" {
                json_object_ordered(child, &["a", "b", "p", "s", "r", "t", "c"])?
            } else {
                json_to_mp_ordered(child)?
            };
            pairs.push((rmpv::Value::String((*key).into()), mp_child));
        }
    }
    // Append any unexpected keys last (stable for forward-compat)
    for (k, child) in obj {
        if used.contains(k.as_str()) || child.is_null() {
            continue;
        }
        pairs.push((
            rmpv::Value::String(k.as_str().into()),
            json_to_mp_ordered(child)?,
        ));
    }
    Ok(rmpv::Value::Map(pairs))
}

fn pack_order_type(t: &Value) -> ExchangeResult<rmpv::Value> {
    if let Some(limit) = t.get("limit") {
        let tif = limit
            .get("tif")
            .and_then(|x| x.as_str())
            .ok_or_else(|| ExchangeError::Other("missing tif".into()))?;
        return Ok(rmpv::Value::Map(vec![(
            rmpv::Value::String("limit".into()),
            rmpv::Value::Map(vec![(
                rmpv::Value::String("tif".into()),
                rmpv::Value::String(tif.into()),
            )]),
        )]));
    }
    json_to_mp_ordered(t)
}

fn float_to_wire(d: Decimal) -> String {
    // Match Python SDK float_to_wire: up to 8 decimals, normalized, no scientific notation.
    let rounded = d.round_dp(8);
    let s = format!("{rounded:.8}");
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "-" {
        "0".into()
    } else {
        trimmed.to_string()
    }
}

/// Perp price tick rules (Hyperliquid docs):
/// - ≤ 5 significant figures (integers always allowed)
/// - ≤ (6 - szDecimals) decimal places
fn round_perp_price(price: Decimal, sz_decimals: u32) -> Decimal {
    if price <= Decimal::ZERO {
        return price;
    }
    let max_decimals = 6u32.saturating_sub(sz_decimals);
    // Prices above 100_000: use integers (always valid regardless of sig figs).
    if price >= Decimal::from(100_000) {
        return price.round_dp(0);
    }
    let sig = round_to_sig_figs(price, 5);
    sig.round_dp(max_decimals)
}

fn round_to_sig_figs(price: Decimal, sig_figs: u32) -> Decimal {
    if price == Decimal::ZERO || sig_figs == 0 {
        return price;
    }
    let abs = price.abs();
    // order of magnitude: floor(log10(abs))
    let f = abs.to_string().parse::<f64>().unwrap_or(0.0);
    if f <= 0.0 {
        return price;
    }
    let exp = f.log10().floor() as i32;
    let scale = (sig_figs as i32 - 1) - exp;
    if scale >= 0 {
        let factor = Decimal::from(10u64.pow(scale as u32));
        (price * factor).round() / factor
    } else {
        let factor = Decimal::from(10u64.pow((-scale) as u32));
        (price / factor).round() * factor
    }
}

fn summarize_batch_place_errors(placed: usize, errors: &[String]) -> String {
    let joined = errors.join("; ");
    if joined.contains("maximum position size") || joined.contains("PerpMaxPosition") {
        return format!(
            "挂单超过当前杠杆允许的最大持仓（已撤销本次成功的 {placed} 笔）。\
原因：买单或卖单一侧的合计名义过大。请降低「总名义投入」、降低杠杆，或先充值增加保证金后重试。"
        );
    }
    // Deduplicate noisy repeated API lines.
    let mut uniq = Vec::new();
    for e in errors {
        if !uniq.iter().any(|u: &String| u == e) {
            uniq.push(e.clone());
        }
        if uniq.len() >= 3 {
            break;
        }
    }
    format!(
        "批量挂单部分失败（已撤销本次成功的 {placed} 笔）: {}",
        uniq.join("；")
    )
}

fn parse_order_status_item(status: &Value) -> ExchangeResult<u64> {
    if let Some(err) = status.get("error").and_then(|e| e.as_str()) {
        return Err(ExchangeError::Api(err.to_string()));
    }
    if let Some(oid) = status
        .pointer("/resting/oid")
        .and_then(|o| o.as_u64())
        .or_else(|| status.pointer("/filled/oid").and_then(|o| o.as_u64()))
    {
        return Ok(oid);
    }
    Err(ExchangeError::Api(format!(
        "order not resting on book: {status}"
    )))
}

/// Hyperliquid may return status=ok while individual order statuses contain errors.
#[allow(dead_code)] // used by unit tests / single-order helpers
fn parse_order_oid(resp: &Value) -> ExchangeResult<u64> {
    let statuses = resp
        .pointer("/response/data/statuses")
        .and_then(|s| s.as_array())
        .ok_or_else(|| ExchangeError::Api(format!("unexpected order response: {resp}")))?;
    let status = statuses
        .first()
        .ok_or_else(|| ExchangeError::Api("empty order statuses".into()))?;
    parse_order_status_item(status)
}

fn parse_batch_order_oids(resp: &Value, expected: usize) -> ExchangeResult<Vec<ExchangeResult<u64>>> {
    let statuses = resp
        .pointer("/response/data/statuses")
        .and_then(|s| s.as_array())
        .ok_or_else(|| ExchangeError::Api(format!("unexpected order response: {resp}")))?;
    if statuses.len() != expected {
        return Err(ExchangeError::Api(format!(
            "expected {expected} order statuses, got {}",
            statuses.len()
        )));
    }
    Ok(statuses.iter().map(parse_order_status_item).collect())
}

fn friendly_hl_error(resp: &Value, expected_addr: Option<&str>) -> String {
    let text = resp.to_string();
    if text.contains("does not exist") {
        let recovered = text
            .split("Wallet ")
            .nth(1)
            .and_then(|s| s.split(' ').next())
            .unwrap_or("");
        let mismatch = expected_addr
            .map(|a| !a.eq_ignore_ascii_case(recovered))
            .unwrap_or(false);
        if mismatch {
            return format!(
                "下单签名校验失败：交易所从签名恢复出的地址 {recovered} 与当前私钥地址 {} 不一致（通常是历史签名 bug，请更新后重试）。原始错误: {text}",
                expected_addr.unwrap_or("")
            );
        }
        return format!(
            "Hyperliquid 账户不存在或未激活（该地址尚未在当前网络入金开户）。\
请先到对应官网用同一钱包充值/领水龙头后再交易：\
主网 https://app.hyperliquid.xyz · 测试网 https://app.hyperliquid-testnet.xyz/drip 。\
并确认本软件模式与官网一致、私钥对应地址与报错地址相同。原始错误: {text}"
        );
    }
    text
}

#[async_trait]
impl Exchange for HyperliquidExchange {
    fn mode(&self) -> RunMode {
        self.mode
    }

    async fn connect(&mut self) -> ExchangeResult<()> {
        self.refresh_spot_meta().await?;
        Ok(())
    }

    async fn get_mid(&self, symbol: &str) -> ExchangeResult<Decimal> {
        let mids = self.post_info(json!({"type": "allMids"})).await?;
        let obj = mids
            .as_object()
            .ok_or_else(|| ExchangeError::Api("allMids not object".into()))?;
        // Exact key first (perp BTC, spot @50, PURR/USDC) so aliases cannot steal it.
        if let Some(v) = obj.get(symbol).and_then(|x| x.as_str()) {
            return Decimal::from_str(v).map_err(|e| ExchangeError::Api(e.to_string()));
        }
        let key = self.resolve_mid_key(symbol);
        if key != symbol {
            if let Some(v) = obj.get(&key).and_then(|x| x.as_str()) {
                return Decimal::from_str(v).map_err(|e| ExchangeError::Api(e.to_string()));
            }
        }
        for (k, v) in obj {
            if k.eq_ignore_ascii_case(symbol) || k.eq_ignore_ascii_case(&format!("{symbol}/USDC"))
            {
                if let Some(s) = v.as_str() {
                    return Decimal::from_str(s).map_err(|e| ExchangeError::Api(e.to_string()));
                }
            }
        }
        Err(ExchangeError::Other(format!("mid not found for {symbol}")))
    }

    async fn get_balances(&self) -> ExchangeResult<Vec<Balance>> {
        let addr = self
            .address
            .as_ref()
            .ok_or(ExchangeError::NotConnected)?;
        let mut out = Vec::new();

        let abstraction = self
            .post_info(json!({"type": "userAbstraction", "user": addr}))
            .await
            .ok()
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "unknown".into());

        let unified = matches!(
            abstraction.as_str(),
            "unifiedAccount" | "portfolioMargin"
        );

        // Official docs: unified / portfolio margin balances live in spot clearinghouse.
        // Perp clearinghouse accountValue is often 0 and not meaningful.
        if let Ok(spot) = self
            .post_info(json!({"type": "spotClearinghouseState", "user": addr}))
            .await
        {
            if let Some(balances) = spot.get("balances").and_then(|b| b.as_array()) {
                for b in balances {
                    let coin = b.get("coin").and_then(|c| c.as_str()).unwrap_or("");
                    let total = b
                        .get("total")
                        .and_then(|t| t.as_str())
                        .and_then(|s| Decimal::from_str(s).ok())
                        .unwrap_or(Decimal::ZERO);
                    let hold = b
                        .get("hold")
                        .and_then(|t| t.as_str())
                        .and_then(|s| Decimal::from_str(s).ok())
                        .unwrap_or(Decimal::ZERO);
                    if total == Decimal::ZERO && hold == Decimal::ZERO {
                        continue;
                    }
                    out.push(Balance {
                        asset: coin.to_string(),
                        total,
                        available: (total - hold).max(Decimal::ZERO),
                        kind: if unified {
                            "unified".into()
                        } else {
                            "spot".into()
                        },
                    });
                }
            }
        }

        // Still show open perp positions / (manual-mode) perp equity when present.
        if let Ok(state) = self
            .post_info(json!({"type": "clearinghouseState", "user": addr}))
            .await
        {
            if !unified {
                let account_value = state
                    .get("marginSummary")
                    .and_then(|m| m.get("accountValue"))
                    .and_then(|v| v.as_str())
                    .and_then(|s| Decimal::from_str(s).ok())
                    .unwrap_or(Decimal::ZERO);
                if account_value != Decimal::ZERO {
                    out.push(Balance {
                        asset: "USDC".into(),
                        total: account_value,
                        available: account_value,
                        kind: "perp".into(),
                    });
                }
            }
            if let Some(positions) = state.get("assetPositions").and_then(|a| a.as_array()) {
                for p in positions {
                    let pos = p.get("position");
                    let coin = pos
                        .and_then(|x| x.get("coin"))
                        .and_then(|c| c.as_str())
                        .unwrap_or("");
                    let szi = pos
                        .and_then(|x| x.get("szi"))
                        .and_then(|s| s.as_str())
                        .and_then(|s| Decimal::from_str(s).ok())
                        .unwrap_or(Decimal::ZERO);
                    if szi != Decimal::ZERO {
                        out.push(Balance {
                            asset: coin.to_string(),
                            total: szi,
                            available: szi,
                            kind: "position".into(),
                        });
                    }
                }
            }
        }

        let mode_row = Balance {
            asset: abstraction.clone(),
            total: Decimal::ZERO,
            available: Decimal::ZERO,
            kind: "mode".into(),
        };
        if out.is_empty() {
            out.push(mode_row);
        } else {
            out.insert(0, mode_row);
        }

        Ok(out)
    }

    async fn place_order(&mut self, intent: OrderIntent) -> ExchangeResult<LiveOrder> {
        let mut orders = self.place_orders(vec![intent]).await?;
        orders
            .pop()
            .ok_or_else(|| ExchangeError::Other("empty place_orders result".into()))
    }

    async fn place_orders(&mut self, intents: Vec<OrderIntent>) -> ExchangeResult<Vec<LiveOrder>> {
        if intents.is_empty() {
            return Ok(vec![]);
        }
        // Chunk conservatively; HL accepts large batches but weight grows with size.
        const CHUNK: usize = 40;
        let mut placed = Vec::with_capacity(intents.len());
        for chunk in intents.chunks(CHUNK) {
            let mut wires = Vec::with_capacity(chunk.len());
            let mut prepared = Vec::with_capacity(chunk.len());
            for intent in chunk {
                let asset = self.resolve_asset(&intent.symbol)?;
                let is_buy = matches!(intent.side, Side::Buy);
                let sz_dec = *self.sz_decimals.get(&intent.symbol).unwrap_or(&4);
                let px = round_perp_price(intent.price, sz_dec);
                let sz = intent.size.round_dp(sz_dec);
                let notional = px * sz;
                if notional < Decimal::from(10) {
                    return Err(ExchangeError::Other(format!(
                        "订单名义约 {notional} USDC，低于 Hyperliquid 最低 $10。请提高总投入或减少网格数量。"
                    )));
                }
                wires.push(json!({
                    "a": asset,
                    "b": is_buy,
                    "p": float_to_wire(px),
                    "s": float_to_wire(sz),
                    "r": false,
                    "t": {"limit": {"tif": "Gtc"}}
                }));
                prepared.push((intent, px, sz));
            }
            let action = json!({
                "type": "order",
                "orders": wires,
                "grouping": "na"
            });
            let resp = self.post_exchange(action).await?;
            if resp.get("status").and_then(|s| s.as_str()) != Some("ok") {
                return Err(ExchangeError::Api(friendly_hl_error(
                    &resp,
                    self.address.as_deref(),
                )));
            }
            let results = parse_batch_order_oids(&resp, prepared.len())?;
            let mut errors = Vec::new();
            for ((intent, px, sz), result) in prepared.into_iter().zip(results) {
                match result {
                    Ok(oid) => {
                        let order = LiveOrder::new(
                            intent.client_id.clone(),
                            Some(oid.to_string()),
                            intent.symbol.clone(),
                            intent.side,
                            px,
                            sz,
                            intent.level_index,
                        );
                        self.open_orders
                            .insert(order.client_id.clone(), order.clone());
                        placed.push(order);
                    }
                    Err(e) => errors.push(e.to_string()),
                }
            }
            if !errors.is_empty() {
                let n = placed.len();
                // Roll back successful orders from this attempt so we don't leave a half grid.
                let ids: Vec<String> = placed.iter().map(|o| o.client_id.clone()).collect();
                for id in ids {
                    let _ = self.cancel_order(&id).await;
                }
                placed.clear();
                return Err(ExchangeError::Api(summarize_batch_place_errors(n, &errors)));
            }
        }
        Ok(placed)
    }

    async fn cancel_order(&mut self, client_id: &str) -> ExchangeResult<()> {
        if let Some(order) = self.open_orders.get(client_id).cloned() {
            if let Some(oid_str) = &order.exchange_id {
                if let Ok(oid) = oid_str.parse::<u64>() {
                    if let Ok(asset) = self.resolve_asset(&order.symbol) {
                        let action = json!({
                            "type": "cancel",
                            "cancels": [{"a": asset, "o": oid}]
                        });
                        let _ = self.post_exchange(action).await;
                    }
                }
            }
        }
        self.open_orders.remove(client_id);
        Ok(())
    }

    async fn cancel_all(&mut self, symbol: &str) -> ExchangeResult<()> {
        // Prefer live open orders from the exchange, not only locally tracked ones.
        let addr = match &self.address {
            Some(a) => a.clone(),
            None => {
                self.open_orders.clear();
                return Ok(());
            }
        };
        let open = self
            .post_info(json!({"type": "openOrders", "user": addr}))
            .await
            .unwrap_or(json!([]));
        let mut cancels = Vec::new();
        if let Some(arr) = open.as_array() {
            for o in arr {
                let coin = o.get("coin").and_then(|c| c.as_str()).unwrap_or("");
                if !symbol.is_empty() && coin != symbol {
                    continue;
                }
                let oid = match o.get("oid").and_then(|x| x.as_u64()) {
                    Some(v) => v,
                    None => continue,
                };
                let asset = match self.resolve_asset(coin) {
                    Ok(a) => a,
                    Err(_) => continue,
                };
                cancels.push(json!({"a": asset, "o": oid}));
            }
        }
        // Also include any locally tracked oids not returned yet.
        for order in self.open_orders.values() {
            if !symbol.is_empty() && order.symbol != symbol {
                continue;
            }
            if let Some(oid_str) = &order.exchange_id {
                if let Ok(oid) = oid_str.parse::<u64>() {
                    if let Ok(asset) = self.resolve_asset(&order.symbol) {
                        let item = json!({"a": asset, "o": oid});
                        if !cancels.iter().any(|c| c == &item) {
                            cancels.push(item);
                        }
                    }
                }
            }
        }
        for chunk in cancels.chunks(40) {
            if chunk.is_empty() {
                continue;
            }
            let action = json!({
                "type": "cancel",
                "cancels": chunk
            });
            let resp = self.post_exchange(action).await?;
            if resp.get("status").and_then(|s| s.as_str()) != Some("ok") {
                warn!("cancel_all response: {resp}");
            }
        }
        if symbol.is_empty() {
            self.open_orders.clear();
        } else {
            self.open_orders.retain(|_, o| o.symbol != symbol);
        }
        Ok(())
    }

    async fn flatten(&mut self) -> ExchangeResult<()> {
        let addr = match &self.address {
            Some(a) => a.clone(),
            None => {
                self.open_orders.clear();
                return Ok(());
            }
        };

        // Fetch open orders + positions together (empty account = 2 quick GETs).
        let open = self
            .post_info(json!({"type": "openOrders", "user": addr}))
            .await
            .unwrap_or(json!([]));
        let state = self
            .post_info(json!({"type": "clearinghouseState", "user": addr}))
            .await
            .unwrap_or(json!({}));

        let mut cancels = Vec::new();
        if let Some(arr) = open.as_array() {
            for o in arr {
                let coin = o.get("coin").and_then(|c| c.as_str()).unwrap_or("");
                let oid = match o.get("oid").and_then(|x| x.as_u64()) {
                    Some(v) => v,
                    None => continue,
                };
                let asset = match self.resolve_asset(coin) {
                    Ok(a) => a,
                    Err(_) => continue,
                };
                cancels.push(json!({"a": asset, "o": oid}));
            }
        }
        for order in self.open_orders.values() {
            if let Some(oid_str) = &order.exchange_id {
                if let Ok(oid) = oid_str.parse::<u64>() {
                    if let Ok(asset) = self.resolve_asset(&order.symbol) {
                        let item = json!({"a": asset, "o": oid});
                        if !cancels.iter().any(|c| c == &item) {
                            cancels.push(item);
                        }
                    }
                }
            }
        }

        let mut close_coins: Vec<(String, Decimal)> = Vec::new();
        if let Some(positions) = state.get("assetPositions").and_then(|a| a.as_array()) {
            for p in positions {
                let pos = match p.get("position") {
                    Some(x) => x,
                    None => continue,
                };
                let coin = pos.get("coin").and_then(|c| c.as_str()).unwrap_or("");
                let szi = pos
                    .get("szi")
                    .and_then(|s| s.as_str())
                    .and_then(|s| Decimal::from_str(s).ok())
                    .unwrap_or(Decimal::ZERO);
                if szi == Decimal::ZERO || coin.is_empty() {
                    continue;
                }
                close_coins.push((coin.to_string(), szi));
            }
        }

        if cancels.is_empty() && close_coins.is_empty() {
            info!("flatten: nothing to cancel or close");
            self.open_orders.clear();
            return Ok(());
        }

        for chunk in cancels.chunks(40) {
            if chunk.is_empty() {
                continue;
            }
            let action = json!({
                "type": "cancel",
                "cancels": chunk
            });
            let resp = self.post_exchange(action).await?;
            if resp.get("status").and_then(|s| s.as_str()) != Some("ok") {
                warn!("flatten cancel response: {resp}");
            }
        }

        let mut close_intents = Vec::new();
        for (coin, szi) in close_coins {
            let mid = self.get_mid(&coin).await.unwrap_or(Decimal::ONE);
            let sz_dec = *self.sz_decimals.get(&coin).unwrap_or(&4);
            let abs_sz = szi.abs().round_dp(sz_dec);
            if abs_sz <= Decimal::ZERO {
                continue;
            }
            // Close long by selling below mid; close short by buying above mid.
            let (is_buy, raw_px) = if szi > Decimal::ZERO {
                (false, mid * dec!(0.95))
            } else {
                (true, mid * dec!(1.05))
            };
            let px = round_perp_price(raw_px, sz_dec);
            let asset = self.resolve_asset(&coin)?;
            close_intents.push(json!({
                "a": asset,
                "b": is_buy,
                "p": float_to_wire(px),
                "s": float_to_wire(abs_sz),
                "r": true,
                "t": {"limit": {"tif": "Ioc"}}
            }));
        }
        for chunk in close_intents.chunks(40) {
            if chunk.is_empty() {
                continue;
            }
            let action = json!({
                "type": "order",
                "orders": chunk,
                "grouping": "na"
            });
            let resp = self.post_exchange(action).await?;
            if resp.get("status").and_then(|s| s.as_str()) != Some("ok") {
                return Err(ExchangeError::Api(friendly_hl_error(
                    &resp,
                    self.address.as_deref(),
                )));
            }
            if let Ok(results) = parse_batch_order_oids(&resp, chunk.len()) {
                let errs: Vec<_> = results
                    .into_iter()
                    .filter_map(|r| r.err().map(|e| e.to_string()))
                    .collect();
                if !errs.is_empty() {
                    warn!("flatten close errors: {}", errs.join("; "));
                }
            }
        }
        self.open_orders.clear();
        Ok(())
    }

    async fn drain_fills(&mut self) -> ExchangeResult<Vec<FillEvent>> {
        let addr = match &self.address {
            Some(a) => a.clone(),
            None => return Ok(vec![]),
        };
        // Never treat historical fills as new — prime once if forgotten.
        if !self.fills_primed {
            self.prime_seen_fills().await?;
        }
        let fills = self
            .post_info(json!({
                "type": "userFills",
                "user": addr
            }))
            .await
            .unwrap_or(json!([]));
        let mut out = Vec::new();
        // Allow small clock skew vs exchange timestamps.
        let min_fill_ms = self.session_start_ms.saturating_sub(3_000);
        // Newest-first; scan enough to cover a busy grid session.
        if let Some(arr) = fills.as_array() {
            for f in arr.iter().take(200) {
                let tid = f
                    .get("tid")
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| f.to_string());
                if self.last_seen_fills.contains(&tid) {
                    continue;
                }

                let fill_ms = f
                    .get("time")
                    .and_then(|t| t.as_u64().or_else(|| t.as_i64().map(|i| i as u64)))
                    .unwrap_or(0);
                if fill_ms > 0 && fill_ms < min_fill_ms {
                    // Old fill that somehow wasn't in the prime snapshot.
                    self.last_seen_fills.push(tid);
                    if self.last_seen_fills.len() > 500 {
                        self.last_seen_fills.drain(0..100);
                    }
                    continue;
                }

                let coin = f.get("coin").and_then(|c| c.as_str()).unwrap_or("");
                let side = match f.get("side").and_then(|s| s.as_str()) {
                    Some("B") | Some("Buy") | Some("buy") => Side::Buy,
                    _ => Side::Sell,
                };
                let px = f
                    .get("px")
                    .and_then(|p| p.as_str())
                    .and_then(|s| Decimal::from_str(s).ok())
                    .unwrap_or(Decimal::ZERO);
                let sz = f
                    .get("sz")
                    .and_then(|p| p.as_str())
                    .and_then(|s| Decimal::from_str(s).ok())
                    .unwrap_or(Decimal::ZERO);
                let oid = f
                    .get("oid")
                    .and_then(|o| o.as_u64().or_else(|| o.as_i64().map(|i| i as u64)))
                    .map(|o| o.to_string());

                // Only attribute fills to our bot orders (match exchange oid).
                let matched = self.open_orders.values().find(|o| {
                    match (&oid, &o.exchange_id) {
                        (Some(fill_oid), Some(ex_id)) => fill_oid == ex_id,
                        _ => false,
                    }
                });
                let Some(order) = matched.cloned() else {
                    // If we still have tracked orders, this fill is external — ignore.
                    // If the map is empty (client was wiped), leave tid unseen so a
                    // later restore can still claim the fill.
                    if !self.open_orders.is_empty() {
                        self.last_seen_fills.push(tid);
                        if self.last_seen_fills.len() > 500 {
                            self.last_seen_fills.drain(0..100);
                        }
                    }
                    continue;
                };

                self.last_seen_fills.push(tid);
                if self.last_seen_fills.len() > 500 {
                    self.last_seen_fills.drain(0..100);
                }

                let client_id = order.client_id.clone();
                let level_index = order.level_index;
                // Keep tracking through partial fills until size is exhausted.
                if let Some(tracked) = self.open_orders.get_mut(&client_id) {
                    let remaining = tracked.size - sz;
                    if remaining.abs() <= Decimal::new(1, 8) || remaining <= Decimal::ZERO {
                        self.open_orders.remove(&client_id);
                    } else {
                        tracked.size = remaining;
                    }
                }
                out.push(FillEvent {
                    client_id,
                    symbol: if coin.is_empty() {
                        order.symbol.clone()
                    } else {
                        coin.to_string()
                    },
                    side,
                    price: px,
                    size: sz,
                    level_index,
                    fee: Decimal::ZERO,
                });
            }
        }
        Ok(out)
    }

    async fn list_open_orders(&self, symbol: &str) -> ExchangeResult<Vec<LiveOrder>> {
        Ok(self
            .open_orders
            .values()
            .filter(|o| o.symbol == symbol)
            .cloned()
            .collect())
    }

    async fn list_spot_symbols(&self) -> ExchangeResult<Vec<String>> {
        let markets = self.list_markets().await?;
        Ok(markets.into_iter().map(|m| m.symbol).collect())
    }

    async fn list_markets(&self) -> ExchangeResult<Vec<MarketInfo>> {
        list_live_markets(self.mode).await
    }
}

/// Fetch mid from Hyperliquid public info API (no private key).
pub async fn fetch_live_mid(mode: RunMode, symbol: &str) -> ExchangeResult<Decimal> {
    let api_mode = match mode {
        RunMode::Testnet => RunMode::Testnet,
        _ => RunMode::Mainnet,
    };
    let mut hl = HyperliquidExchange::new(api_mode);
    hl.connect().await?;
    hl.get_mid(symbol).await
}

/// Candlestick interval accepted by Hyperliquid `candleSnapshot`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CandleInterval {
    #[serde(rename = "1m")]
    M1,
    #[serde(rename = "3m")]
    M3,
    #[serde(rename = "5m")]
    M5,
    #[serde(rename = "15m")]
    M15,
    #[serde(rename = "30m")]
    M30,
    #[serde(rename = "1h")]
    H1,
    #[serde(rename = "2h")]
    H2,
    #[serde(rename = "4h")]
    H4,
    #[serde(rename = "8h")]
    H8,
    #[serde(rename = "12h")]
    H12,
    #[serde(rename = "1d")]
    D1,
    #[serde(rename = "3d")]
    D3,
    #[serde(rename = "1w")]
    W1,
    #[serde(rename = "1M")]
    Mo1,
}

impl CandleInterval {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::M1 => "1m",
            Self::M3 => "3m",
            Self::M5 => "5m",
            Self::M15 => "15m",
            Self::M30 => "30m",
            Self::H1 => "1h",
            Self::H2 => "2h",
            Self::H4 => "4h",
            Self::H8 => "8h",
            Self::H12 => "12h",
            Self::D1 => "1d",
            Self::D3 => "3d",
            Self::W1 => "1w",
            Self::Mo1 => "1M",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "1m" => Some(Self::M1),
            "3m" => Some(Self::M3),
            "5m" => Some(Self::M5),
            "15m" => Some(Self::M15),
            "30m" => Some(Self::M30),
            "1h" => Some(Self::H1),
            "2h" => Some(Self::H2),
            "4h" => Some(Self::H4),
            "8h" => Some(Self::H8),
            "12h" => Some(Self::H12),
            "1d" => Some(Self::D1),
            "3d" => Some(Self::D3),
            "1w" => Some(Self::W1),
            "1M" => Some(Self::Mo1),
            _ => None,
        }
    }

    /// Approximate duration of one bar in milliseconds.
    pub fn duration_ms(self) -> i64 {
        match self {
            Self::M1 => 60_000,
            Self::M3 => 180_000,
            Self::M5 => 300_000,
            Self::M15 => 900_000,
            Self::M30 => 1_800_000,
            Self::H1 => 3_600_000,
            Self::H2 => 7_200_000,
            Self::H4 => 14_400_000,
            Self::H8 => 28_800_000,
            Self::H12 => 43_200_000,
            Self::D1 => 86_400_000,
            Self::D3 => 259_200_000,
            Self::W1 => 604_800_000,
            Self::Mo1 => 2_592_000_000, // ~30d
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Candle {
    /// Candle open time (unix seconds).
    pub time: i64,
    pub open: String,
    pub high: String,
    pub low: String,
    pub close: String,
    pub volume: String,
}

/// Fetch OHLCV candles from Hyperliquid public `candleSnapshot` (no private key).
pub async fn fetch_candles(
    mode: RunMode,
    symbol: &str,
    interval: CandleInterval,
    limit: usize,
) -> ExchangeResult<Vec<Candle>> {
    let api_mode = match mode {
        RunMode::Testnet => RunMode::Testnet,
        _ => RunMode::Mainnet,
    };
    let mut hl = HyperliquidExchange::new(api_mode);
    hl.connect().await?;

    let coin = hl.resolve_candle_coin(symbol);
    let bars = limit.clamp(1, 5000);
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let start_ms = now_ms.saturating_sub(interval.duration_ms().saturating_mul(bars as i64));

    let body = json!({
        "type": "candleSnapshot",
        "req": {
            "coin": coin,
            "interval": interval.as_str(),
            "startTime": start_ms,
            "endTime": now_ms,
        }
    });
    let raw = hl.post_info(body).await?;
    let arr = raw
        .as_array()
        .ok_or_else(|| ExchangeError::Api("candleSnapshot not array".into()))?;

    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let t_ms = item
            .get("t")
            .and_then(|v| v.as_i64().or_else(|| v.as_u64().map(|u| u as i64)))
            .unwrap_or(0);
        let open = item.get("o").and_then(|v| v.as_str()).unwrap_or("0");
        let high = item.get("h").and_then(|v| v.as_str()).unwrap_or("0");
        let low = item.get("l").and_then(|v| v.as_str()).unwrap_or("0");
        let close = item.get("c").and_then(|v| v.as_str()).unwrap_or("0");
        let volume = item.get("v").and_then(|v| v.as_str()).unwrap_or("0");
        if t_ms <= 0 {
            continue;
        }
        out.push(Candle {
            time: t_ms / 1000,
            open: open.to_string(),
            high: high.to_string(),
            low: low.to_string(),
            close: close.to_string(),
            volume: volume.to_string(),
        });
    }
    out.sort_by_key(|c| c.time);
    out.dedup_by_key(|c| c.time);
    if out.len() > bars {
        let skip = out.len() - bars;
        out = out.split_off(skip);
    }
    Ok(out)
}

/// Build selectable market list with live mids.
pub async fn list_live_markets(mode: RunMode) -> ExchangeResult<Vec<MarketInfo>> {
    let mode = match mode {
        RunMode::Testnet => RunMode::Testnet,
        _ => RunMode::Mainnet, // simulation + mainnet use mainnet catalog
    };
    let mut hl = HyperliquidExchange::new(mode);
    hl.connect().await?;
    let mids = hl.post_info(json!({"type": "allMids"})).await?;
    let obj = mids
        .as_object()
        .ok_or_else(|| ExchangeError::Api("allMids not object".into()))?;

    // Per-coin leverage limits from perpetual meta.
    let mut lev_map: HashMap<String, (u32, bool)> = HashMap::new();
    if let Ok(meta) = hl.post_info(json!({"type": "meta"})).await {
        if let Some(universe) = meta.get("universe").and_then(|u| u.as_array()) {
            for item in universe {
                let Some(name) = item.get("name").and_then(|n| n.as_str()) else {
                    continue;
                };
                if item.get("isDelisted").and_then(|d| d.as_bool()) == Some(true) {
                    continue;
                }
                let max_lev = item
                    .get("maxLeverage")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(50) as u32;
                let only_isolated = item
                    .get("onlyIsolated")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                    || item
                        .get("marginMode")
                        .and_then(|v| v.as_str())
                        .map(|m| m == "strictIsolated" || m == "noCross")
                        .unwrap_or(false);
                lev_map.insert(name.to_string(), (max_lev.max(1), only_isolated));
            }
        }
    }

    let preferred = [
        "BTC", "ETH", "HYPE", "SOL", "BNB", "ARB", "OP", "DOGE", "AVAX", "LINK",
    ];
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for coin in preferred {
        if let Some(v) = obj.get(coin).and_then(|x| x.as_str()) {
            if let Ok(mid) = Decimal::from_str(v) {
                seen.insert(coin.to_string());
                let (max_leverage, only_isolated) =
                    lev_map.get(coin).copied().unwrap_or((50, false));
                out.push(MarketInfo {
                    symbol: coin.to_string(),
                    label: format!("{coin} (perp)"),
                    kind: "perp".into(),
                    mid,
                    min_leverage: 1,
                    max_leverage,
                    only_isolated,
                });
            }
        }
    }

    // Other perp-like coins (no @/#)
    let mut others: Vec<_> = obj
        .keys()
        .filter(|k| {
            let key = k.as_str();
            !key.starts_with('@')
                && !key.starts_with('#')
                && !key.contains('/')
                && !seen.contains(key)
                && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
        .cloned()
        .collect();
    others.sort();
    for coin in others.into_iter().take(80) {
        if let Some(v) = obj.get(&coin).and_then(|x| x.as_str()) {
            if let Ok(mid) = Decimal::from_str(v) {
                let (max_leverage, only_isolated) =
                    lev_map.get(&coin).copied().unwrap_or((50, false));
                out.push(MarketInfo {
                    symbol: coin.clone(),
                    label: format!("{coin} (perp)"),
                    kind: "perp".into(),
                    mid,
                    min_leverage: 1,
                    max_leverage,
                    only_isolated,
                });
            }
        }
    }

    // Spot pairs omitted — product is perpetual grid.
    let _ = &hl.mid_aliases;

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::VerifyingKey;
    use rust_decimal_macros::dec;

    #[test]
    fn order_msgpack_uses_sdk_field_order() {
        let action = json!({
            "type": "order",
            "orders": [{
                "a": 3u64,
                "b": true,
                "p": "65000.0",
                "s": "0.001",
                "r": false,
                "t": {"limit": {"tif": "Gtc"}}
            }],
            "grouping": "na"
        });
        let packed = pack_action_msgpack(&action).unwrap();
        // type, orders, grouping — not alphabetical grouping/orders/type
        assert_eq!(
            hex::encode(&packed),
            "83a474797065a56f72646572a66f72646572739186a16103a162c3a170a736353030302e30a173a5302e303031a172c2a17481a56c696d697481a3746966a3477463a867726f7570696e67a26e61"
        );
    }

    #[test]
    fn l1_signature_recovers_signer_address() {
        let mut hl = HyperliquidExchange::new(RunMode::Testnet);
        // deterministic test key
        let sk = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        hl.set_private_key(sk).unwrap();
        let expected = hl.address().unwrap().to_lowercase();

        let action = json!({
            "type": "order",
            "orders": [{
                "a": 3u64,
                "b": true,
                "p": float_to_wire(dec!(65000)),
                "s": float_to_wire(dec!(0.001)),
                "r": false,
                "t": {"limit": {"tif": "Gtc"}}
            }],
            "grouping": "na"
        });
        let nonce = 1_700_000_000_000u64;
        let (r, s, v) = hl.sign_l1_action(&action, nonce).unwrap();
        let connection_id = action_hash(&action, nonce, None).unwrap();
        let source = "b";
        let domain_type_hash = keccak(
            b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
        );
        let name_hash = keccak(b"Exchange");
        let version_hash = keccak(b"1");
        let mut domain = Vec::new();
        domain.extend_from_slice(&domain_type_hash);
        domain.extend_from_slice(&name_hash);
        domain.extend_from_slice(&version_hash);
        domain.extend_from_slice(&u256_bytes(1337));
        domain.extend_from_slice(&[0u8; 32]);
        let domain_separator = keccak(&domain);
        let agent_type_hash = keccak(b"Agent(string source,bytes32 connectionId)");
        let source_hash = keccak(source.as_bytes());
        let mut msg = Vec::new();
        msg.extend_from_slice(&agent_type_hash);
        msg.extend_from_slice(&source_hash);
        msg.extend_from_slice(&connection_id);
        let struct_hash = keccak(&msg);
        let mut digest_input = Vec::with_capacity(66);
        digest_input.extend_from_slice(&[0x19, 0x01]);
        digest_input.extend_from_slice(&domain_separator);
        digest_input.extend_from_slice(&struct_hash);
        let digest = keccak(&digest_input);

        let mut sig = [0u8; 64];
        sig[..32].copy_from_slice(&hex::decode(&r).unwrap());
        sig[32..].copy_from_slice(&hex::decode(&s).unwrap());
        let recid = (v.parse::<u8>().unwrap() - 27) % 2;
        let vk = VerifyingKey::recover_from_prehash(
            &digest,
            &Signature::from_slice(&sig).unwrap(),
            RecoveryId::try_from(recid).unwrap(),
        )
        .unwrap();
        let point = vk.to_encoded_point(false);
        let hash = Keccak256::digest(&point.as_bytes()[1..]);
        let recovered = format!("0x{}", hex::encode(&hash[12..]));
        assert_eq!(recovered, expected);
    }

    #[test]
    fn parse_resting_oid() {
        let resp = json!({
            "status": "ok",
            "response": {
                "type": "order",
                "data": { "statuses": [ {"resting": {"oid": 12345}} ] }
            }
        });
        assert_eq!(parse_order_oid(&resp).unwrap(), 12345);
    }

    #[test]
    fn parse_order_error_status() {
        let resp = json!({
            "status": "ok",
            "response": {
                "type": "order",
                "data": { "statuses": [ {"error": "Order must have minimum value of $10."} ] }
            }
        });
        let err = parse_order_oid(&resp).unwrap_err().to_string();
        assert!(err.contains("minimum value"));
    }

    #[test]
    fn eth_price_tick_five_sig_figs() {
        // asset=4 ETH on testnet: szDecimals=4 → max 2 decimal places, ≤5 sig figs
        // 1927.15 has 6 sig figs → must become 1927.2 or 1927.1
        let px = round_perp_price(dec!(1927.15), 4);
        assert_eq!(px, dec!(1927.2));
        // 1234.56 → 1234.6 (5 sig) then 2 dp still 1234.6
        assert_eq!(round_perp_price(dec!(1234.56), 4), dec!(1234.6));
        // already valid
        assert_eq!(round_perp_price(dec!(1927.2), 4), dec!(1927.2));
    }

    #[test]
    fn btc_price_tick() {
        // BTC szDecimals=5 → max 1 decimal place
        assert_eq!(round_perp_price(dec!(65561.55), 5), dec!(65562));
        assert_eq!(round_perp_price(dec!(97000.55), 5), dec!(97001));
    }
}


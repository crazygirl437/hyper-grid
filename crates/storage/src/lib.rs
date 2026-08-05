use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Local;
use directories::ProjectDirs;
use rusqlite::{params, Connection};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tracing::info;

mod env_file;
mod ledger;
mod session;

pub use env_file::{env_path, resolve_data_dir, resolve_program_dir};
pub use ledger::{
    DailyPnlRow, EquitySnapshotRow, FillLedgerRow, FundingRow, SessionListItem, SessionPnlSummary,
};
pub use session::BotSessionRow;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub private_key: String,
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default)]
    pub language: Option<String>,
    /// @deprecated kept for old config.json; use `symbol`.
    #[serde(default)]
    pub last_symbol: Option<String>,
    #[serde(default = "default_symbol")]
    pub symbol: String,
    #[serde(default)]
    pub lower_price: String,
    #[serde(default)]
    pub upper_price: String,
    #[serde(default = "default_grid_count")]
    pub grid_count: u32,
    #[serde(default = "default_budget")]
    pub total_budget: String,
    #[serde(default = "default_spacing")]
    pub spacing: String,
    #[serde(default = "default_breakout")]
    pub breakout_action: String,
    #[serde(default = "default_drawdown")]
    pub max_drawdown_pct: String,
    #[serde(default = "default_daily_loss")]
    pub max_daily_loss: String,
    #[serde(default = "default_order_failures")]
    pub max_order_failures: u32,
    #[serde(default = "default_leverage")]
    pub leverage: u32,
    #[serde(default = "default_cross")]
    pub is_cross: bool,
    #[serde(default = "default_chart_mode")]
    pub chart_mode: String,
    #[serde(default = "default_chart_interval")]
    pub chart_interval: String,
    /// Percent used by "fill range from mid" (±N%).
    #[serde(default = "default_range_pct")]
    pub range_pct: String,
    #[serde(default = "default_grid_mode")]
    pub grid_mode: String,
    #[serde(default = "default_atr_interval")]
    pub atr_interval: String,
    #[serde(default = "default_atr_period")]
    pub atr_period: u32,
    #[serde(default = "default_atr_mult")]
    pub atr_mult: String,
    #[serde(default = "default_confirm_bars")]
    pub confirm_bars: u32,
    #[serde(default = "default_recenter_cooldown")]
    pub recenter_cooldown_secs: u64,
    #[serde(default = "default_max_recenters")]
    pub max_recenters_per_day: u32,
    #[serde(default)]
    pub auto_start: bool,
    #[serde(default = "default_true")]
    pub resume_on_restart: bool,
    /// preserve | flatten — close window policy.
    #[serde(default = "default_exit_policy")]
    pub exit_policy: String,
}

fn default_mode() -> String {
    "simulation".into()
}
fn default_symbol() -> String {
    "BTC".into()
}
fn default_grid_count() -> u32 {
    30
}
fn default_budget() -> String {
    "3000".into()
}
fn default_spacing() -> String {
    "arithmetic".into()
}
fn default_breakout() -> String {
    "cancel_close_and_stop".into()
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
fn default_leverage() -> u32 {
    5
}
fn default_cross() -> bool {
    true
}
fn default_chart_mode() -> String {
    "line".into()
}
fn default_chart_interval() -> String {
    "15m".into()
}
fn default_range_pct() -> String {
    "5".into()
}
fn default_grid_mode() -> String {
    "dynamic".into()
}
fn default_atr_interval() -> String {
    "1h".into()
}
fn default_atr_period() -> u32 {
    14
}
fn default_atr_mult() -> String {
    "5".into()
}
fn default_confirm_bars() -> u32 {
    2
}
fn default_recenter_cooldown() -> u64 {
    3600
}
fn default_max_recenters() -> u32 {
    4
}
fn default_true() -> bool {
    true
}
fn default_exit_policy() -> String {
    "preserve".into()
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            private_key: String::new(),
            mode: default_mode(),
            language: None,
            last_symbol: None,
            symbol: default_symbol(),
            lower_price: String::new(),
            upper_price: String::new(),
            grid_count: default_grid_count(),
            total_budget: default_budget(),
            spacing: default_spacing(),
            breakout_action: default_breakout(),
            max_drawdown_pct: default_drawdown(),
            max_daily_loss: default_daily_loss(),
            max_order_failures: default_order_failures(),
            leverage: default_leverage(),
            is_cross: default_cross(),
            chart_mode: default_chart_mode(),
            chart_interval: default_chart_interval(),
            range_pct: default_range_pct(),
            grid_mode: default_grid_mode(),
            atr_interval: default_atr_interval(),
            atr_period: default_atr_period(),
            atr_mult: default_atr_mult(),
            confirm_bars: default_confirm_bars(),
            recenter_cooldown_secs: default_recenter_cooldown(),
            max_recenters_per_day: default_max_recenters(),
            auto_start: false,
            resume_on_restart: true,
            exit_policy: default_exit_policy(),
        }
    }
}

impl AppConfig {
    pub fn to_env_pairs(&self) -> Vec<(String, String)> {
        vec![
            ("MODE".into(), self.mode.clone()),
            ("PRIVATE_KEY".into(), self.private_key.clone()),
            ("LANGUAGE".into(), self.language.clone().unwrap_or_default()),
            ("SYMBOL".into(), self.symbol.clone()),
            ("LOWER_PRICE".into(), self.lower_price.clone()),
            ("UPPER_PRICE".into(), self.upper_price.clone()),
            ("GRID_COUNT".into(), self.grid_count.to_string()),
            ("TOTAL_BUDGET".into(), self.total_budget.clone()),
            ("SPACING".into(), self.spacing.clone()),
            ("BREAKOUT_ACTION".into(), self.breakout_action.clone()),
            ("MAX_DRAWDOWN_PCT".into(), self.max_drawdown_pct.clone()),
            ("MAX_DAILY_LOSS".into(), self.max_daily_loss.clone()),
            (
                "MAX_ORDER_FAILURES".into(),
                self.max_order_failures.to_string(),
            ),
            ("LEVERAGE".into(), self.leverage.to_string()),
            (
                "IS_CROSS".into(),
                if self.is_cross { "true" } else { "false" }.into(),
            ),
            ("CHART_MODE".into(), self.chart_mode.clone()),
            ("CHART_INTERVAL".into(), self.chart_interval.clone()),
            ("RANGE_PCT".into(), self.range_pct.clone()),
            ("GRID_MODE".into(), self.grid_mode.clone()),
            ("ATR_INTERVAL".into(), self.atr_interval.clone()),
            ("ATR_PERIOD".into(), self.atr_period.to_string()),
            ("ATR_MULT".into(), self.atr_mult.clone()),
            ("CONFIRM_BARS".into(), self.confirm_bars.to_string()),
            (
                "RECENTER_COOLDOWN_SECS".into(),
                self.recenter_cooldown_secs.to_string(),
            ),
            (
                "MAX_RECENTERS_PER_DAY".into(),
                self.max_recenters_per_day.to_string(),
            ),
            (
                "AUTO_START".into(),
                if self.auto_start { "true" } else { "false" }.into(),
            ),
            (
                "RESUME_ON_RESTART".into(),
                if self.resume_on_restart {
                    "true"
                } else {
                    "false"
                }
                .into(),
            ),
            ("EXIT_POLICY".into(), self.exit_policy.clone()),
        ]
    }

    fn migrate_legacy(&mut self) {
        if self.symbol.is_empty() {
            if let Some(s) = self.last_symbol.clone() {
                self.symbol = s;
            } else {
                self.symbol = default_symbol();
            }
        }
        if self.mode.is_empty() {
            self.mode = default_mode();
        }
        if self.grid_count == 0 {
            self.grid_count = default_grid_count();
        }
        if self.total_budget.is_empty() {
            self.total_budget = default_budget();
        }
        if self.spacing.is_empty() {
            self.spacing = default_spacing();
        }
        if self.breakout_action.is_empty() {
            self.breakout_action = default_breakout();
        }
        if self.max_drawdown_pct.is_empty() {
            self.max_drawdown_pct = default_drawdown();
        }
        if self.max_daily_loss.is_empty() {
            self.max_daily_loss = default_daily_loss();
        }
        if self.max_order_failures == 0 {
            self.max_order_failures = default_order_failures();
        }
        if self.leverage == 0 {
            self.leverage = default_leverage();
        }
        if self.chart_mode.is_empty() {
            self.chart_mode = default_chart_mode();
        }
        if self.chart_interval.is_empty() {
            self.chart_interval = default_chart_interval();
        }
        if self.range_pct.is_empty() {
            self.range_pct = default_range_pct();
        }
        if self.grid_mode.is_empty() {
            self.grid_mode = default_grid_mode();
        }
        if self.atr_interval.is_empty() {
            self.atr_interval = default_atr_interval();
        }
        if self.atr_period == 0 {
            self.atr_period = default_atr_period();
        }
        if self.atr_mult.is_empty() {
            self.atr_mult = default_atr_mult();
        }
        if self.confirm_bars == 0 {
            self.confirm_bars = default_confirm_bars();
        }
        if self.max_recenters_per_day == 0 {
            self.max_recenters_per_day = default_max_recenters();
        }
        if self.exit_policy.is_empty() {
            self.exit_policy = default_exit_policy();
        }
    }
}

pub struct Storage {
    root: PathBuf,
    db: Connection,
    env_file: PathBuf,
}

impl Storage {
    pub fn open_default() -> Result<Self> {
        let program_dir = env_file::resolve_program_dir();
        let root = env_file::resolve_data_dir();
        fs::create_dir_all(&root)
            .with_context(|| format!("create data dir {}", root.display()))?;
        let db_path = root.join("hyper-grid.db");

        // One-time migrate into `<program>/data/` from older layouts.
        if !db_path.exists() {
            let mut migrated_from: Option<PathBuf> = None;
            // 1) Previous layout: DB next to the binary.
            let beside = program_dir.join("hyper-grid.db");
            if beside.exists() {
                fs::copy(&beside, &db_path).with_context(|| {
                    format!("migrate db from {} to {}", beside.display(), db_path.display())
                })?;
                for suffix in ["-wal", "-shm"] {
                    let src = program_dir.join(format!("hyper-grid.db{suffix}"));
                    if src.exists() {
                        let _ = fs::copy(&src, root.join(format!("hyper-grid.db{suffix}")));
                    }
                }
                migrated_from = Some(beside);
            } else if let Some(dirs) = ProjectDirs::from("xyz", "hyper-grid", "hyper-grid") {
                // 2) Legacy XDG app-data location.
                let legacy_dir = dirs.data_dir();
                let legacy_db = legacy_dir.join("hyper-grid.db");
                if legacy_db.exists() {
                    fs::copy(&legacy_db, &db_path).with_context(|| {
                        format!(
                            "migrate db from {} to {}",
                            legacy_db.display(),
                            db_path.display()
                        )
                    })?;
                    for suffix in ["-wal", "-shm"] {
                        let src = legacy_dir.join(format!("hyper-grid.db{suffix}"));
                        if src.exists() {
                            let _ = fs::copy(&src, root.join(format!("hyper-grid.db{suffix}")));
                        }
                    }
                    migrated_from = Some(legacy_db);
                }
            }
            if let Some(src) = migrated_from {
                info!("migrated SQLite from {} → {}", src.display(), db_path.display());
            }
        }

        // Move non-secret config mirror / analytics folder if they still sit beside the binary.
        let old_config = program_dir.join("config.json");
        let new_config = root.join("config.json");
        if old_config.exists() && !new_config.exists() {
            let _ = fs::copy(&old_config, &new_config);
        }
        let old_analytics = program_dir.join("analytics");
        let new_analytics = root.join("analytics");
        if old_analytics.is_dir() && !new_analytics.exists() {
            if let Err(e) = fs::rename(&old_analytics, &new_analytics) {
                info!("could not move analytics/ into data/: {e}");
            }
        }

        let mut storage = Self::open(&root)?;
        // User-facing `.env` stays next to the runnable program (not under data/).
        storage.env_file = env_file::env_path();
        Ok(storage)
    }

    pub fn open(root: &Path) -> Result<Self> {
        fs::create_dir_all(root)?;
        let db_path = root.join("hyper-grid.db");
        let db = Connection::open(&db_path)?;
        let storage = Self {
            root: root.to_path_buf(),
            db,
            // Tests / custom roots keep `.env` beside the data dir.
            env_file: root.join(".env"),
        };
        storage.migrate()?;
        Ok(storage)
    }

    /// Path of the synced `.env` (next to the program binary / AppImage / .app).
    pub fn dotenv_path(&self) -> &Path {
        &self.env_file
    }

    pub fn data_dir(&self) -> &Path {
        &self.root
    }

    fn migrate(&self) -> Result<()> {
        self.db.execute_batch(
            r#"
            PRAGMA foreign_keys = ON;
            PRAGMA busy_timeout = 5000;
            PRAGMA journal_mode = WAL;

            CREATE TABLE IF NOT EXISTS fills (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts TEXT NOT NULL,
                symbol TEXT NOT NULL,
                side TEXT NOT NULL,
                price TEXT NOT NULL,
                size TEXT NOT NULL,
                pnl TEXT NOT NULL,
                client_id TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts TEXT NOT NULL,
                kind TEXT NOT NULL,
                message TEXT NOT NULL,
                payload TEXT
            );
            CREATE TABLE IF NOT EXISTS order_snapshots (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts TEXT NOT NULL,
                symbol TEXT NOT NULL,
                payload TEXT NOT NULL
            );
            "#,
        )?;

        let version: i32 = self
            .db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);

        if version < 2 {
            // Expand fills ledger columns (nullable for legacy rows).
            let alters = [
                "ALTER TABLE fills ADD COLUMN session_id TEXT",
                "ALTER TABLE fills ADD COLUMN strategy_id TEXT",
                "ALTER TABLE fills ADD COLUMN exchange_tid TEXT",
                "ALTER TABLE fills ADD COLUMN exchange_oid TEXT",
                "ALTER TABLE fills ADD COLUMN cloid TEXT",
                "ALTER TABLE fills ADD COLUMN exchange_time_ms INTEGER",
                "ALTER TABLE fills ADD COLUMN direction TEXT",
                "ALTER TABLE fills ADD COLUMN notional TEXT",
                "ALTER TABLE fills ADD COLUMN crossed INTEGER DEFAULT 0",
                "ALTER TABLE fills ADD COLUMN fee TEXT DEFAULT '0'",
                "ALTER TABLE fills ADD COLUMN fee_token TEXT",
                "ALTER TABLE fills ADD COLUMN gross_closed_pnl TEXT DEFAULT '0'",
                "ALTER TABLE fills ADD COLUMN position_before TEXT",
                "ALTER TABLE fills ADD COLUMN position_after TEXT",
                "ALTER TABLE fills ADD COLUMN source TEXT DEFAULT 'legacy'",
                "ALTER TABLE events ADD COLUMN payload TEXT",
            ];
            for sql in alters {
                let _ = self.db.execute(sql, []);
            }
            let _ = self.db.execute(
                "UPDATE fills SET source = 'legacy' WHERE source IS NULL OR source = ''",
                [],
            );
            self.db.execute_batch(
                r#"
                CREATE UNIQUE INDEX IF NOT EXISTS idx_fills_exchange_tid
                  ON fills(exchange_tid) WHERE exchange_tid IS NOT NULL AND exchange_tid != '';

                CREATE TABLE IF NOT EXISTS bot_sessions (
                  session_id TEXT PRIMARY KEY,
                  strategy_id TEXT NOT NULL,
                  symbol TEXT NOT NULL,
                  status TEXT NOT NULL,
                  config_json TEXT NOT NULL,
                  created_at_ms INTEGER NOT NULL,
                  updated_at_ms INTEGER NOT NULL,
                  active INTEGER NOT NULL DEFAULT 0
                );

                CREATE TABLE IF NOT EXISTS session_checkpoints (
                  id INTEGER PRIMARY KEY AUTOINCREMENT,
                  session_id TEXT NOT NULL,
                  phase TEXT NOT NULL,
                  payload TEXT NOT NULL,
                  created_at_ms INTEGER NOT NULL,
                  is_active INTEGER NOT NULL DEFAULT 1
                );
                CREATE INDEX IF NOT EXISTS idx_checkpoints_session
                  ON session_checkpoints(session_id, is_active, id DESC);

                CREATE TABLE IF NOT EXISTS recenter_operations (
                  id INTEGER PRIMARY KEY AUTOINCREMENT,
                  session_id TEXT NOT NULL,
                  generation INTEGER NOT NULL,
                  phase TEXT NOT NULL,
                  intent_json TEXT NOT NULL,
                  result_json TEXT,
                  created_at_ms INTEGER NOT NULL,
                  updated_at_ms INTEGER NOT NULL
                );

                CREATE TABLE IF NOT EXISTS funding_payments (
                  id INTEGER PRIMARY KEY AUTOINCREMENT,
                  session_id TEXT NOT NULL,
                  strategy_id TEXT NOT NULL,
                  symbol TEXT NOT NULL,
                  exchange_time_ms INTEGER NOT NULL,
                  usdc TEXT NOT NULL,
                  position_size TEXT,
                  funding_rate TEXT,
                  event_key TEXT NOT NULL UNIQUE
                );

                CREATE TABLE IF NOT EXISTS equity_snapshots (
                  id INTEGER PRIMARY KEY AUTOINCREMENT,
                  session_id TEXT NOT NULL,
                  strategy_id TEXT NOT NULL,
                  ts_ms INTEGER NOT NULL,
                  realized_pnl TEXT NOT NULL,
                  unrealized_pnl TEXT NOT NULL,
                  fees_cum TEXT NOT NULL,
                  funding_cum TEXT NOT NULL,
                  net_pnl TEXT NOT NULL,
                  position_base TEXT NOT NULL,
                  avg_entry TEXT,
                  mark TEXT,
                  liquidation_price TEXT,
                  account_equity TEXT,
                  margin_used TEXT
                );
                CREATE INDEX IF NOT EXISTS idx_equity_session_ts
                  ON equity_snapshots(session_id, ts_ms);

                PRAGMA user_version = 2;
                "#,
            )?;
        }
        Ok(())
    }

    pub fn config_path(&self) -> PathBuf {
        self.root.join("config.json")
    }

    pub fn load_config(&self) -> Result<AppConfig> {
        // `.env` is the only user-facing source of truth.
        // If the user deletes it, do NOT resurrect secrets from config.json.
        if self.env_file.exists() {
            let mut cfg = AppConfig::default();
            let map = env_file::load_env_file(&self.env_file)?;
            env_file::apply_env_map(&mut cfg, &map);
            cfg.migrate_legacy();
            return Ok(cfg);
        }

        Ok(AppConfig::default())
    }

    pub fn save_config(&self, cfg: &AppConfig) -> Result<()> {
        let mut cfg = cfg.clone();
        cfg.migrate_legacy();
        cfg.last_symbol = Some(cfg.symbol.clone());

        // Write `.env` first (authoritative).
        env_file::write_env_file(&self.env_file, &cfg)?;

        // Keep a non-secret local cache for diagnostics — never store the private key here.
        let mut for_json = cfg.clone();
        for_json.private_key.clear();
        let path = self.config_path();
        let text = serde_json::to_string_pretty(&for_json)?;
        fs::write(path, text)?;

        info!("config saved (.env at {})", self.env_file.display());
        Ok(())
    }

    pub fn record_fill(
        &self,
        symbol: &str,
        side: &str,
        price: Decimal,
        size: Decimal,
        pnl: Decimal,
        client_id: &str,
    ) -> Result<()> {
        let row = FillLedgerRow {
            session_id: String::new(),
            strategy_id: String::new(),
            exchange_tid: None,
            exchange_oid: None,
            cloid: Some(client_id.to_string()),
            client_id: client_id.to_string(),
            exchange_time_ms: None,
            symbol: symbol.to_string(),
            side: side.to_string(),
            direction: None,
            price,
            size,
            notional: price * size,
            crossed: false,
            fee: Decimal::ZERO,
            fee_token: None,
            gross_closed_pnl: pnl,
            position_before: None,
            position_after: None,
            source: "runtime".into(),
        };
        self.record_fill_ledger(&row)?;
        Ok(())
    }

    pub fn record_event(&self, kind: &str, message: &str) -> Result<()> {
        self.record_event_payload(kind, message, None)
    }

    pub fn record_event_payload(
        &self,
        kind: &str,
        message: &str,
        payload: Option<&str>,
    ) -> Result<()> {
        self.db.execute(
            "INSERT INTO events (ts, kind, message, payload) VALUES (?1,?2,?3,?4)",
            params![Local::now().to_rfc3339(), kind, message, payload],
        )?;
        Ok(())
    }

    pub fn list_fills(&self, limit: usize) -> Result<Vec<FillRow>> {
        let mut stmt = self.db.prepare(
            "SELECT ts, symbol, side, price, size, pnl, client_id FROM fills ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(FillRow {
                ts: row.get(0)?,
                symbol: row.get(1)?,
                side: row.get(2)?,
                price: row.get(3)?,
                size: row.get(4)?,
                pnl: row.get(5)?,
                client_id: row.get(6)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn list_events(&self, limit: usize) -> Result<Vec<EventRow>> {
        let mut stmt = self
            .db
            .prepare("SELECT ts, kind, message FROM events ORDER BY id DESC LIMIT ?1")?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(EventRow {
                ts: row.get(0)?,
                kind: row.get(1)?,
                message: row.get(2)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn export_fills_csv(&self, path: &Path) -> Result<usize> {
        let fills = self.list_fills(10_000)?;
        let mut csv = String::from("ts,symbol,side,price,size,pnl,client_id\n");
        for f in &fills {
            csv.push_str(&format!(
                "{},{},{},{},{},{},{}\n",
                f.ts, f.symbol, f.side, f.price, f.size, f.pnl, f.client_id
            ));
        }
        fs::write(path, csv)?;
        Ok(fills.len())
    }

    pub fn save_order_snapshot(&self, symbol: &str, payload: &str) -> Result<()> {
        self.db.execute(
            "INSERT INTO order_snapshots (ts, symbol, payload) VALUES (?1,?2,?3)",
            params![Local::now().to_rfc3339(), symbol, payload],
        )?;
        Ok(())
    }

    pub fn clear_logs(&self) -> Result<()> {
        self.db.execute_batch(
            r#"
            DELETE FROM fills;
            DELETE FROM events;
            DELETE FROM order_snapshots;
            "#,
        )?;
        Ok(())
    }

    /// Clear analytics ledger data.
    /// - `Some(session_id)`: fills / funding / equity for that session only
    /// - `None`: all analytics rows + inactive bot_sessions (keeps active session row)
    pub fn clear_analytics(&self, session_id: Option<&str>) -> Result<u64> {
        let mut cleared = 0u64;
        if let Some(sid) = session_id {
            cleared += self
                .db
                .execute("DELETE FROM fills WHERE session_id = ?1", params![sid])?
                as u64;
            cleared += self.db.execute(
                "DELETE FROM funding_payments WHERE session_id = ?1",
                params![sid],
            )? as u64;
            cleared += self.db.execute(
                "DELETE FROM equity_snapshots WHERE session_id = ?1",
                params![sid],
            )? as u64;
        } else {
            cleared += self.db.execute("DELETE FROM funding_payments", [])? as u64;
            cleared += self.db.execute("DELETE FROM equity_snapshots", [])? as u64;
            // Keep legacy rows without session_id out of analytics wipe? Wipe all fills
            // that belong to sessions; also wipe orphan session fills.
            cleared += self.db.execute(
                "DELETE FROM fills WHERE session_id IS NOT NULL AND session_id != ''",
                [],
            )? as u64;
            cleared += self
                .db
                .execute("DELETE FROM bot_sessions WHERE active = 0", [])?
                as u64;
            cleared += self.db.execute(
                "DELETE FROM session_checkpoints WHERE session_id NOT IN (SELECT session_id FROM bot_sessions WHERE active = 1)",
                [],
            )? as u64;
            cleared += self.db.execute(
                "DELETE FROM recenter_operations WHERE session_id NOT IN (SELECT session_id FROM bot_sessions WHERE active = 1)",
                [],
            )? as u64;
        }
        Ok(cleared)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FillRow {
    pub ts: String,
    pub symbol: String,
    pub side: String,
    pub price: String,
    pub size: String,
    pub pnl: String,
    pub client_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRow {
    pub ts: String,
    pub kind: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use tempfile::tempdir;

    #[test]
    fn config_and_fills_roundtrip() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        let mut cfg = AppConfig::default();
        cfg.mode = "simulation".into();
        cfg.private_key = "0xabc".into();
        cfg.symbol = "ETH".into();
        cfg.grid_mode = "dynamic".into();
        cfg.auto_start = true;
        storage.save_config(&cfg).unwrap();
        let loaded = storage.load_config().unwrap();
        assert_eq!(loaded.private_key, "0xabc");
        assert_eq!(loaded.symbol, "ETH");
        assert_eq!(loaded.grid_mode, "dynamic");
        assert!(loaded.auto_start);
        assert_eq!(loaded.exit_policy, "preserve");
        storage
            .record_fill("BTC", "buy", dec!(1), dec!(2), dec!(0), "cid")
            .unwrap();
        storage.record_event("test", "hello").unwrap();
        assert_eq!(storage.list_fills(10).unwrap().len(), 1);
        assert_eq!(storage.list_events(10).unwrap().len(), 1);
        let csv = dir.path().join("out.csv");
        assert_eq!(storage.export_fills_csv(&csv).unwrap(), 1);
        storage.clear_logs().unwrap();
        assert_eq!(storage.list_fills(10).unwrap().len(), 0);
        assert_eq!(storage.list_events(10).unwrap().len(), 0);
    }

    #[test]
    fn checkpoint_and_ledger_idempotent() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        let version: i32 = storage
            .db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 2);

        storage
            .upsert_bot_session("s1", "strat", "BTC", "Running", "{}", true)
            .unwrap();
        let active = storage.get_active_session().unwrap().unwrap();
        assert_eq!(active.session_id, "s1");

        let payload = serde_json::json!({"phase":"pre_cancel","mid":"100"});
        storage.save_checkpoint("s1", "pre_cancel", &payload).unwrap();
        let (phase, _) = storage.latest_checkpoint("s1").unwrap().unwrap();
        assert_eq!(phase, "pre_cancel");

        let op = storage
            .begin_recenter_op("s1", 1, &serde_json::to_string(&payload).unwrap())
            .unwrap();
        storage
            .complete_recenter_op(op, "committed", r#"{"ok":true}"#)
            .unwrap();

        let fill = FillLedgerRow {
            session_id: "s1".into(),
            strategy_id: "strat".into(),
            exchange_tid: Some("tid-1".into()),
            exchange_oid: Some("oid-1".into()),
            cloid: Some("cloid-1".into()),
            client_id: "cloid-1".into(),
            exchange_time_ms: Some(1),
            symbol: "BTC".into(),
            side: "buy".into(),
            direction: Some("Open Long".into()),
            price: dec!(100),
            size: dec!(0.1),
            notional: dec!(10),
            crossed: false,
            fee: dec!(0.01),
            fee_token: Some("USDC".into()),
            gross_closed_pnl: dec!(0),
            position_before: Some(dec!(0)),
            position_after: Some(dec!(0.1)),
            source: "exchange".into(),
        };
        assert!(storage.record_fill_ledger(&fill).unwrap());
        assert!(!storage.record_fill_ledger(&fill).unwrap());

        let funding = FundingRow {
            session_id: "s1".into(),
            strategy_id: "strat".into(),
            symbol: "BTC".into(),
            exchange_time_ms: 2,
            usdc: dec!(0.05),
            position_size: Some(dec!(0.1)),
            funding_rate: Some(dec!(0.0001)),
            event_key: "fund-1".into(),
        };
        assert!(storage.record_funding(&funding).unwrap());
        assert!(!storage.record_funding(&funding).unwrap());

        storage
            .record_equity_snapshot(&EquitySnapshotRow {
                session_id: "s1".into(),
                strategy_id: "strat".into(),
                ts_ms: 3,
                realized_pnl: dec!(1),
                unrealized_pnl: dec!(2),
                fees_cum: dec!(0.01),
                funding_cum: dec!(0.05),
                net_pnl: dec!(1.04),
                position_base: dec!(0.1),
                avg_entry: Some(dec!(100)),
                mark: Some(dec!(101)),
                liquidation_price: None,
                account_equity: Some(dec!(1000)),
                margin_used: Some(dec!(50)),
            })
            .unwrap();

        let summary = storage.session_pnl_summary("s1").unwrap();
        assert_eq!(summary.fill_count, 1);
        assert_eq!(summary.fees, dec!(0.01));
        assert_eq!(summary.funding, dec!(0.05));
        assert_eq!(summary.net_pnl, dec!(0) - dec!(0.01) + dec!(0.05));

        assert_eq!(storage.session_fees_cum("s1").unwrap(), dec!(0.01));
        let daily = storage.daily_pnl(Some("s1"), 30).unwrap();
        assert!(!daily.is_empty());
        let pack_dir = dir.path().join("pack");
        let pack = storage
            .export_analytics_pack(&pack_dir, Some("s1"))
            .unwrap();
        assert_eq!(pack["fill_rows"], 1);
        assert!(pack_dir.join("fills.csv").exists());
        assert!(pack_dir.join("summary.json").exists());
        let sessions = storage.list_session_summaries(10).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "s1");

        let n = storage.clear_analytics(Some("s1")).unwrap();
        assert!(n >= 1);
        let summary2 = storage.session_pnl_summary("s1").unwrap();
        assert_eq!(summary2.fill_count, 0);
        assert_eq!(summary2.fees, dec!(0));
        assert_eq!(summary2.funding, dec!(0));
    }
}

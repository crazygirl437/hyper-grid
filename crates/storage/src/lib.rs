use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use directories::ProjectDirs;
use rusqlite::{params, Connection};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tracing::info;

mod env_file;

pub use env_file::{env_path, resolve_program_dir};

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
}

fn default_mode() -> String {
    "simulation".into()
}
fn default_symbol() -> String {
    "BTC".into()
}
fn default_grid_count() -> u32 {
    10
}
fn default_budget() -> String {
    "1000".into()
}
fn default_spacing() -> String {
    "arithmetic".into()
}
fn default_breakout() -> String {
    "pause".into()
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
        }
    }
}

impl AppConfig {
    pub fn to_env_pairs(&self) -> Vec<(String, String)> {
        vec![
            ("MODE".into(), self.mode.clone()),
            ("PRIVATE_KEY".into(), self.private_key.clone()),
            (
                "LANGUAGE".into(),
                self.language.clone().unwrap_or_default(),
            ),
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
    }
}

pub struct Storage {
    root: PathBuf,
    db: Connection,
    env_file: PathBuf,
}

impl Storage {
    pub fn open_default() -> Result<Self> {
        let dirs = ProjectDirs::from("xyz", "hyper-grid", "hyper-grid")
            .context("cannot resolve app data dir")?;
        let root = dirs.data_dir().to_path_buf();
        fs::create_dir_all(&root)?;
        let mut storage = Self::open(&root)?;
        // User-facing `.env` lives under the program/workspace directory.
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

    /// Path of the synced `.env` (under program/workspace dir).
    pub fn dotenv_path(&self) -> &Path {
        &self.env_file
    }

    fn migrate(&self) -> Result<()> {
        self.db.execute_batch(
            r#"
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
                message TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS order_snapshots (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts TEXT NOT NULL,
                symbol TEXT NOT NULL,
                payload TEXT NOT NULL
            );
            "#,
        )?;
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
        self.db.execute(
            "INSERT INTO fills (ts, symbol, side, price, size, pnl, client_id) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                Utc::now().to_rfc3339(),
                symbol,
                side,
                price.to_string(),
                size.to_string(),
                pnl.to_string(),
                client_id
            ],
        )?;
        Ok(())
    }

    pub fn record_event(&self, kind: &str, message: &str) -> Result<()> {
        self.db.execute(
            "INSERT INTO events (ts, kind, message) VALUES (?1,?2,?3)",
            params![Utc::now().to_rfc3339(), kind, message],
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
            params![Utc::now().to_rfc3339(), symbol, payload],
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
        storage.save_config(&cfg).unwrap();
        let loaded = storage.load_config().unwrap();
        assert_eq!(loaded.private_key, "0xabc");
        assert_eq!(loaded.symbol, "ETH");
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
}

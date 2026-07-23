//! `.env` config sync for hyper-grid.
//!
//! File lives under the program/workspace directory (not OS app-data), so users
//! can open and edit it next to the project. Format is classic KEY=VALUE lines.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::info;

use crate::AppConfig;

const ENV_HEADER: &str = "# hyper-grid settings — edited by the app; you can also edit manually\n";

/// Resolve the directory that owns `.env` ("程序下").
///
/// Priority:
/// 1. `HYPER_GRID_HOME`
/// 2. Walk up from cwd / exe looking for the hyper-grid workspace root
/// 3. Executable parent directory
/// 4. Current working directory
pub fn resolve_program_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HYPER_GRID_HOME") {
        let p = PathBuf::from(home);
        if !p.as_os_str().is_empty() {
            return p;
        }
    }

    let mut starts: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        starts.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            starts.push(parent.to_path_buf());
        }
    }

    for start in starts {
        let mut cur = start;
        for _ in 0..10 {
            if is_program_root(&cur) {
                return cur;
            }
            if !cur.pop() {
                break;
            }
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            return parent.to_path_buf();
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn is_program_root(dir: &Path) -> bool {
    let cargo = dir.join("Cargo.toml");
    if !cargo.is_file() {
        return false;
    }
    let Ok(text) = fs::read_to_string(&cargo) else {
        return false;
    };
    text.contains("[workspace]")
        || text.contains("name = \"hyper-grid\"")
        || text.contains("name = 'hyper-grid'")
}

pub fn env_path() -> PathBuf {
    resolve_program_dir().join(".env")
}

pub fn load_env_file(path: &Path) -> Result<BTreeMap<String, String>> {
    let mut map = BTreeMap::new();
    if !path.exists() {
        return Ok(map);
    }
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let body = line.strip_prefix("export ").unwrap_or(line);
        let Some((k, v)) = body.split_once('=') else {
            continue;
        };
        let key = k.trim();
        if key.is_empty() {
            continue;
        }
        let mut val = v.trim().to_string();
        if (val.starts_with('"') && val.ends_with('"'))
            || (val.starts_with('\'') && val.ends_with('\''))
        {
            val = val[1..val.len() - 1].to_string();
        }
        map.insert(key.to_string(), val);
    }
    Ok(map)
}

pub fn write_env_file(path: &Path, cfg: &AppConfig) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut out = String::from(ENV_HEADER);
    for (k, v) in cfg.to_env_pairs() {
        out.push_str(&format!("{k}={}\n", escape_env_value(&v)));
    }
    fs::write(path, out).with_context(|| format!("write {}", path.display()))?;
    info!("env saved to {}", path.display());
    Ok(())
}

fn escape_env_value(v: &str) -> String {
    if v.is_empty() {
        return String::new();
    }
    if v.chars()
        .any(|c| c.is_whitespace() || matches!(c, '#' | '"' | '\'' | '=' | '\\'))
    {
        let escaped = v.replace('\\', "\\\\").replace('"', "\\\"");
        return format!("\"{escaped}\"");
    }
    v.to_string()
}

pub fn apply_env_map(cfg: &mut AppConfig, map: &BTreeMap<String, String>) {
    macro_rules! take {
        ($field:ident, $key:expr) => {
            if let Some(v) = map.get($key) {
                cfg.$field = v.clone();
            }
        };
        ($field:ident, $key:expr, opt) => {
            if let Some(v) = map.get($key) {
                if v.is_empty() {
                    cfg.$field = None;
                } else {
                    cfg.$field = Some(v.clone());
                }
            }
        };
        ($field:ident, $key:expr, u32) => {
            if let Some(v) = map.get($key) {
                if let Ok(n) = v.parse::<u32>() {
                    cfg.$field = n;
                }
            }
        };
        ($field:ident, $key:expr, bool) => {
            if let Some(v) = map.get($key) {
                cfg.$field = matches!(
                    v.to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                );
            }
        };
    }

    take!(private_key, "PRIVATE_KEY");
    take!(mode, "MODE");
    take!(language, "LANGUAGE", opt);
    take!(symbol, "SYMBOL");
    take!(lower_price, "LOWER_PRICE");
    take!(upper_price, "UPPER_PRICE");
    take!(grid_count, "GRID_COUNT", u32);
    take!(total_budget, "TOTAL_BUDGET");
    take!(spacing, "SPACING");
    take!(breakout_action, "BREAKOUT_ACTION");
    take!(max_drawdown_pct, "MAX_DRAWDOWN_PCT");
    take!(max_daily_loss, "MAX_DAILY_LOSS");
    take!(max_order_failures, "MAX_ORDER_FAILURES", u32);
    take!(leverage, "LEVERAGE", u32);
    take!(is_cross, "IS_CROSS", bool);
    take!(chart_mode, "CHART_MODE");
    take!(chart_interval, "CHART_INTERVAL");
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn env_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(".env");
        let mut cfg = AppConfig::default();
        cfg.mode = "testnet".into();
        cfg.private_key = "0xabc def".into();
        cfg.symbol = "ETH".into();
        cfg.grid_count = 12;
        cfg.is_cross = false;
        cfg.language = Some("zh-CN".into());
        write_env_file(&path, &cfg).unwrap();
        let map = load_env_file(&path).unwrap();
        let mut loaded = AppConfig::default();
        apply_env_map(&mut loaded, &map);
        assert_eq!(loaded.mode, "testnet");
        assert_eq!(loaded.private_key, "0xabc def");
        assert_eq!(loaded.symbol, "ETH");
        assert_eq!(loaded.grid_count, 12);
        assert!(!loaded.is_cross);
        assert_eq!(loaded.language.as_deref(), Some("zh-CN"));
    }
}

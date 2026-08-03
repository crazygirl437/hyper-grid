use chrono::Local;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskConfig {
    pub max_drawdown_pct: Decimal,
    pub max_daily_loss: Decimal,
    pub max_order_failures: u32,
    pub starting_equity: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskState {
    pub realized_pnl: Decimal,
    pub peak_equity: Decimal,
    pub daily_pnl: Decimal,
    pub current_equity: Decimal,
    pub daily_start_equity: Decimal,
    /// Calendar day in the host machine's local timezone (YYYY-MM-DD).
    pub daily_local_date: String,
    pub order_failures: u32,
    pub halted: bool,
    pub halt_reason: Option<String>,
}

impl RiskState {
    pub fn new(starting_equity: Decimal) -> Self {
        Self {
            realized_pnl: Decimal::ZERO,
            peak_equity: starting_equity,
            daily_pnl: Decimal::ZERO,
            current_equity: starting_equity,
            daily_start_equity: starting_equity,
            daily_local_date: Local::now().format("%Y-%m-%d").to_string(),
            order_failures: 0,
            halted: false,
            halt_reason: None,
        }
    }

    pub fn on_fill_pnl(&mut self, pnl: Decimal, _cfg: &RiskConfig) {
        self.realized_pnl += pnl;
    }

    /// Revalue strategy equity from allocated budget plus net realized and unrealized PnL.
    pub fn on_strategy_equity(&mut self, unrealized_pnl: Decimal, cfg: &RiskConfig) {
        let date = Local::now().format("%Y-%m-%d").to_string();
        self.on_strategy_equity_at(unrealized_pnl, cfg, &date);
    }

    fn on_strategy_equity_at(
        &mut self,
        unrealized_pnl: Decimal,
        cfg: &RiskConfig,
        local_date: &str,
    ) {
        let equity = cfg.starting_equity + self.realized_pnl + unrealized_pnl;
        if self.daily_local_date != local_date {
            self.daily_local_date = local_date.to_string();
            self.daily_start_equity = equity;
        }
        self.current_equity = equity;
        self.daily_pnl = equity - self.daily_start_equity;
        if equity > self.peak_equity {
            self.peak_equity = equity;
        }
        if cfg.max_daily_loss > Decimal::ZERO && self.daily_pnl <= -cfg.max_daily_loss {
            self.halt("daily loss limit reached");
        }
        if cfg.max_drawdown_pct > Decimal::ZERO && self.peak_equity > Decimal::ZERO {
            let dd = (self.peak_equity - equity) / self.peak_equity * Decimal::from(100);
            if dd >= cfg.max_drawdown_pct {
                self.halt(format!("max drawdown {dd}% reached"));
            }
        }
    }

    pub fn on_order_failure(&mut self, cfg: &RiskConfig) {
        self.order_failures += 1;
        if self.order_failures >= cfg.max_order_failures.max(1) {
            self.halt("too many consecutive order failures");
        }
    }

    pub fn on_order_success(&mut self) {
        self.order_failures = 0;
    }

    pub fn force_halt(&mut self, reason: impl Into<String>) {
        self.halt(reason);
    }

    fn halt(&mut self, reason: impl Into<String>) {
        self.halted = true;
        self.halt_reason = Some(reason.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn config() -> RiskConfig {
        RiskConfig {
            max_drawdown_pct: dec!(10),
            max_daily_loss: dec!(50),
            max_order_failures: 3,
            starting_equity: dec!(1000),
        }
    }

    #[test]
    fn unrealized_loss_triggers_strategy_drawdown() {
        let cfg = config();
        let mut risk = RiskState::new(cfg.starting_equity);
        risk.on_strategy_equity_at(dec!(-101), &cfg, "2026-07-29");

        assert!(risk.halted);
        assert_eq!(risk.current_equity, dec!(899));
    }

    #[test]
    fn daily_loss_uses_equity_and_resets_on_local_date() {
        let mut cfg = config();
        cfg.max_drawdown_pct = Decimal::ZERO;
        let mut risk = RiskState::new(cfg.starting_equity);
        risk.daily_local_date = "2026-07-29".into();

        risk.on_strategy_equity_at(dec!(-40), &cfg, "2026-07-29");
        assert!(!risk.halted);
        assert_eq!(risk.daily_pnl, dec!(-40));

        risk.on_strategy_equity_at(dec!(-40), &cfg, "2026-07-30");
        assert_eq!(risk.daily_pnl, Decimal::ZERO);
        risk.on_strategy_equity_at(dec!(-91), &cfg, "2026-07-30");
        assert!(risk.halted);
        assert_eq!(risk.daily_pnl, dec!(-51));
    }
}

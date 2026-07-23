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
            order_failures: 0,
            halted: false,
            halt_reason: None,
        }
    }

    pub fn on_fill_pnl(&mut self, pnl: Decimal, cfg: &RiskConfig) {
        self.realized_pnl += pnl;
        self.daily_pnl += pnl;
        let equity = cfg.starting_equity + self.realized_pnl;
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

    fn halt(&mut self, reason: impl Into<String>) {
        self.halted = true;
        self.halt_reason = Some(reason.into());
    }
}

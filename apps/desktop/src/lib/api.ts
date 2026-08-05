import { invoke } from "@tauri-apps/api/core";

/** True only inside the Tauri webview (not a normal browser tab). */
export function isTauriRuntime(): boolean {
  if (typeof window === "undefined") return false;
  const w = window as Window & {
    __TAURI_INTERNALS__?: unknown;
    __TAURI__?: unknown;
  };
  return w.__TAURI_INTERNALS__ != null || w.__TAURI__ != null;
}

export class NotTauriError extends Error {
  constructor() {
    super(
      "请使用桌面窗口运行（bunx tauri dev），不要在浏览器打开。 / Use the desktop window from bunx tauri dev — not the browser.",
    );
    this.name = "NotTauriError";
  }
}

export type GridLevel = {
  index: number;
  price: string;
  side: "buy" | "sell";
  size: string;
};

export type GridPreview = {
  levels: GridLevel[];
  buy_count: number;
  sell_count: number;
  size_per_level: string;
  estimated_quote_needed: string;
  estimated_base_needed: string;
  max_loss_in_range: string;
  max_loss_at: "lower" | "upper" | string;
  estimated_margin: string;
  worst_equity_isolated: string;
  worst_margin_ratio_pct: string;
  isolated_liquidation_risk: boolean;
  cross_liq_risk_on_strategy_margin: boolean;
  cross_liquidation_risk: boolean | null;
  estimated_long_liq_price?: string | null;
  estimated_short_liq_price?: string | null;
  leverage: number;
  is_cross: boolean;
  assumed_mmr: string;
  max_leverage?: number;
};

export type RestingOrder = {
  side: "buy" | "sell";
  price: string;
  size: string;
};

export type BotSnapshot = {
  status: string;
  status_note?: string | null;
  mode: string;
  symbol: string;
  mid_price: string | null;
  open_orders: number;
  fill_count?: number;
  resting_orders?: RestingOrder[];
  position_base: string;
  avg_entry_price: string | null;
  liquidation_price?: string | null;
  realized_pnl: string;
  unrealized_pnl: string;
  funding_pnl: string;
  events_tail: string[];
  active_lower?: string | null;
  active_upper?: string | null;
  atr?: string | null;
  atr_pct?: string | null;
  recenter_generation?: number;
  recenters_today?: number;
  last_recenter_ms?: number | null;
  session_id?: string | null;
  last_tick_ms?: number | null;
  health_note?: string | null;
  grid_mode?: string;
};

export type ChartInterval = "1m" | "5m" | "15m" | "1h" | "4h" | "1d";

export type ChartMode = "line" | "candle";

export const CHART_INTERVALS: ChartInterval[] = ["1m", "5m", "15m", "1h", "4h", "1d"];

export type AppSettings = {
  private_key: string;
  mode: string;
  language?: string | null;
  symbol: string;
  lower_price: string;
  upper_price: string;
  grid_count: number;
  total_budget: string;
  spacing: string;
  breakout_action: string;
  max_drawdown_pct: string;
  max_daily_loss: string;
  max_order_failures: number;
  leverage: number;
  is_cross: boolean;
  chart_mode: string;
  chart_interval: string;
  range_pct?: string;
  grid_mode?: string;
  atr_interval?: string;
  atr_period?: number;
  atr_mult?: string;
  confirm_bars?: number;
  recenter_cooldown_secs?: number;
  max_recenters_per_day?: number;
  auto_start?: boolean;
  resume_on_restart?: boolean;
  exit_policy?: string;
  env_path?: string;
};

export type Candle = {
  time: number;
  open: string;
  high: string;
  low: string;
  close: string;
  volume: string;
};

export async function api<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauriRuntime()) {
    throw new NotTauriError();
  }
  return invoke<T>(cmd, args);
}

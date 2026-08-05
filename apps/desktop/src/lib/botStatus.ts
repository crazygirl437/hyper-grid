/** Normalize DB Debug (`Running`) or serde (`running` / `soft_breakout`) to snake_case. */
export function normalizeBotStatusKey(raw?: string | null): string {
  const s = String(raw || "idle").trim();
  if (!s) return "idle";
  if (s.includes("_") || s === s.toLowerCase()) return s.toLowerCase();
  return s
    .replace(/([a-z0-9])([A-Z])/g, "$1_$2")
    .replace(/([A-Z]+)([A-Z][a-z])/g, "$1_$2")
    .toLowerCase();
}

const STATUS_I18N: Record<string, string> = {
  idle: "app.statusIdle",
  running: "app.statusRunning",
  paused: "app.statusPaused",
  soft_breakout: "app.statusSoftBreakout",
  recentering: "app.statusRecentering",
  recovering: "app.statusRecovering",
  detached: "app.statusDetached",
  protective_exit: "app.statusProtectiveExit",
  breakout_stopped: "app.statusBreakoutStopped",
  halted: "app.statusHalted",
  stopped: "app.statusStopped",
};

export function botStatusI18nKey(raw?: string | null): string {
  const key = normalizeBotStatusKey(raw);
  return STATUS_I18N[key] || "app.statusIdle";
}

export function botStatusCssClass(raw?: string | null): string {
  const key = normalizeBotStatusKey(raw);
  if (key === "running" || key === "recentering") return "status-running";
  if (
    key === "paused" ||
    key === "soft_breakout" ||
    key === "recovering" ||
    key === "detached"
  ) {
    return "status-paused";
  }
  if (
    key === "halted" ||
    key === "protective_exit" ||
    key === "breakout_stopped" ||
    key === "stopped"
  ) {
    return "status-halted";
  }
  return "status-idle";
}

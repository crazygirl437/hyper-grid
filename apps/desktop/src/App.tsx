import { useCallback, useEffect, useMemo, useRef, useState, type CSSProperties } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import {
  api,
  AppSettings,
  BotSnapshot,
  Candle,
  ChartInterval,
  ChartMode,
  GridLevel,
  GridPreview,
} from "./lib/api";
import { GridChart, ChartTrade, PricePoint } from "./components/GridChart";
import { FlattenOverlay } from "./components/FlattenOverlay";
import i18n from "./i18n";

type Tab = "account" | "configure" | "dashboard";

type MarketInfo = {
  symbol: string;
  label: string;
  kind: string;
  mid: string;
  min_leverage?: number;
  max_leverage?: number;
  only_isolated?: boolean;
};

const defaultForm = {
  symbol: "BTC",
  lowerPrice: "",
  upperPrice: "",
  gridCount: 10,
  totalBudget: "1000",
  spacing: "arithmetic",
  breakoutAction: "pause",
  maxDrawdownPct: "20",
  maxDailyLoss: "100",
  maxOrderFailures: 5,
  leverage: 5,
  isCross: true,
};

function suggestRange(mid: number) {
  if (!Number.isFinite(mid) || mid <= 0) {
    return { lower: "", upper: "" };
  }
  const lower = mid * 0.95;
  const upper = mid * 1.05;
  const digits = mid >= 1000 ? 2 : mid >= 1 ? 4 : 6;
  return {
    lower: lower.toFixed(digits),
    upper: upper.toFixed(digits),
  };
}

function marketLeverageBounds(m?: MarketInfo | null) {
  const min = Math.max(1, Number(m?.min_leverage) || 1);
  const max = Math.max(min, Number(m?.max_leverage) || 50);
  return { min, max, onlyIsolated: !!m?.only_isolated };
}

function clampLeverage(value: number, min: number, max: number) {
  if (!Number.isFinite(value)) return min;
  return Math.min(max, Math.max(min, Math.round(value)));
}

export default function App() {
  const { t } = useTranslation();
  const [tab, setTab] = useState<Tab>("account");
  const [mode, setMode] = useState("simulation");
  const [privateKey, setPrivateKey] = useState("");
  const [privateKeyDirty, setPrivateKeyDirty] = useState(false);
  const [showPrivateKey, setShowPrivateKey] = useState(false);
  const storedPrivateKeyRef = useRef("");
  const PRIVATE_KEY_MASK = "••••••••••••••••••••";

  function isPrivateKeyMask(value: string) {
    const v = value.trim();
    return !v || v === PRIVATE_KEY_MASK || /^[•\u2022\u25CF*]+$/.test(v);
  }

  /** Never persist mask characters — keep previous real key if unchanged/masked. */
  function resolvePrivateKeyForSave() {
    if (!privateKeyDirty) return storedPrivateKeyRef.current;
    const v = privateKey.trim();
    if (isPrivateKeyMask(v)) return storedPrivateKeyRef.current;
    return v;
  }

  function setPrivateKeyFromStorage(raw: string) {
    const key = (raw || "").trim();
    storedPrivateKeyRef.current = key;
    setPrivateKeyDirty(false);
    setShowPrivateKey(false);
    setPrivateKey(key ? PRIVATE_KEY_MASK : "");
  }
  const [address, setAddress] = useState("");
  const [balances, setBalances] = useState<
    { asset: string; total: string; available?: string; kind?: string }[]
  >([]);
  const [form, setForm] = useState(defaultForm);
  const [markets, setMarkets] = useState<MarketInfo[]>([]);
  const [marketsLoading, setMarketsLoading] = useState(false);
  const [mid, setMid] = useState(0);
  const [midLoading, setMidLoading] = useState(false);
  const [levels, setLevels] = useState<GridLevel[]>([]);
  const [preview, setPreview] = useState<GridPreview | null>(null);
  const [status, setStatus] = useState<BotSnapshot | null>(null);
  const [fills, setFills] = useState<any[]>([]);
  const [events, setEvents] = useState<any[]>([]);
  const [error, setError] = useState("");
  const [tip, setTip] = useState("");
  const [configJson, setConfigJson] = useState("");
  const [priceHistory, setPriceHistory] = useState<PricePoint[]>([]);
  const [candles, setCandles] = useState<Candle[]>([]);
  const [chartMode, setChartMode] = useState<ChartMode>("line");
  const [chartInterval, setChartInterval] = useState<ChartInterval>("15m");
  const [candlesLoading, setCandlesLoading] = useState(false);
  const [chartTrades, setChartTrades] = useState<ChartTrade[]>([]);
  const [envPath, setEnvPath] = useState("");
  const settingsReady = useRef(false);
  const skipNextPersist = useRef(false);

  function buildSettingsPayload(): AppSettings {
    return {
      private_key: resolvePrivateKeyForSave(),
      mode,
      language: i18n.language,
      symbol: form.symbol,
      lower_price: form.lowerPrice,
      upper_price: form.upperPrice,
      grid_count: form.gridCount,
      total_budget: form.totalBudget,
      spacing: form.spacing,
      breakout_action: form.breakoutAction,
      max_drawdown_pct: form.maxDrawdownPct,
      max_daily_loss: form.maxDailyLoss,
      max_order_failures: form.maxOrderFailures,
      leverage: form.leverage,
      is_cross: form.isCross,
      chart_mode: chartMode,
      chart_interval: chartInterval,
    };
  }

  const persistSettings = useCallback(async () => {
    if (!settingsReady.current || skipNextPersist.current) return;
    try {
      const res = await api<AppSettings>("save_settings", {
        settings: buildSettingsPayload(),
      });
      if (res?.env_path) setEnvPath(res.env_path);
    } catch (e) {
      console.warn("save_settings failed", e);
    }
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [privateKey, mode, form, chartMode, chartInterval]);

  function pushPrice(value: number) {
    if (!Number.isFinite(value) || value <= 0) return;
    const time = Math.floor(Date.now() / 1000);
    setPriceHistory((prev) => {
      const next = [...prev, { time, value }];
      return next.length > 300 ? next.slice(-300) : next;
    });
    // Keep last candle close in sync with live mid between refreshes.
    setCandles((prev) => {
      if (prev.length === 0) return prev;
      const last = { ...prev[prev.length - 1] };
      const close = Number(last.close);
      const high = Number(last.high);
      const low = Number(last.low);
      last.close = String(value);
      if (Number.isFinite(high)) last.high = String(Math.max(high, value));
      if (Number.isFinite(low)) last.low = String(Math.min(low, value));
      if (!Number.isFinite(close) || close <= 0) last.open = String(value);
      return [...prev.slice(0, -1), last];
    });
  }

  const loadCandles = useCallback(
    async (symbol: string, interval: ChartInterval, silent = false) => {
      if (!symbol) return;
      if (!silent) setCandlesLoading(true);
      try {
        const rows = await api<Candle[]>("get_candles", {
          symbol,
          interval,
          limit: 300,
        });
        setCandles(rows);
      } catch (e: any) {
        if (!silent) setError(String(e));
      } finally {
        if (!silent) setCandlesLoading(false);
      }
    },
    []
  );

  function pushTrade(side: "buy" | "sell", price: number, size?: string, id?: string) {
    if (!Number.isFinite(price) || price <= 0) return;
    const time = Math.floor(Date.now() / 1000);
    setPriceHistory((prev) => {
      const next = [...prev, { time, value: price }];
      return next.length > 300 ? next.slice(-300) : next;
    });
    setChartTrades((prev) => {
      const trade: ChartTrade = {
        id: id || `${side}-${time}-${price}-${Math.random().toString(36).slice(2, 7)}`,
        time,
        price,
        side,
        size,
      };
      const next = [...prev, trade];
      return next.length > 100 ? next.slice(-100) : next;
    });
  }

  const midNumber = useMemo(() => mid, [mid]);

  const leverageBounds = useMemo(() => {
    const m = markets.find((x) => x.symbol === form.symbol);
    return marketLeverageBounds(m);
  }, [markets, form.symbol]);

  useEffect(() => {
    const { min, max, onlyIsolated } = leverageBounds;
    setForm((f) => {
      const nextLev = clampLeverage(f.leverage, min, max);
      const nextCross = onlyIsolated ? false : f.isCross;
      if (nextLev === f.leverage && nextCross === f.isCross) return f;
      return { ...f, leverage: nextLev, isCross: nextCross };
    });
  }, [leverageBounds]);

  async function loadMarkets(preferredSymbol?: string) {
    setMarketsLoading(true);
    const wantSymbol = preferredSymbol || form.symbol;
    try {
      const list = await api<MarketInfo[]>("list_markets");
      setMarkets(list);
      if (list.length && !list.find((m) => m.symbol === wantSymbol)) {
        await applySymbol(list[0].symbol, Number(list[0].mid));
      } else if (list.length) {
        const cur = list.find((m) => m.symbol === wantSymbol);
        if (cur) {
          const midVal = Number(cur.mid);
          setMid(midVal);
          setForm((f) => {
            if (f.lowerPrice && f.upperPrice) return { ...f, symbol: wantSymbol };
            if (!f.lowerPrice || !f.upperPrice) {
              const range = suggestRange(midVal);
              return {
                ...f,
                symbol: wantSymbol,
                lowerPrice: f.lowerPrice || range.lower,
                upperPrice: f.upperPrice || range.upper,
              };
            }
            return { ...f, symbol: wantSymbol };
          });
        }
      }
    } catch (e: any) {
      setError(String(e));
    } finally {
      setMarketsLoading(false);
    }
  }

  async function applySymbol(symbol: string, knownMid?: number) {
    const mkt = markets.find((x) => x.symbol === symbol);
    const bounds = marketLeverageBounds(mkt);
    setForm((f) => ({
      ...f,
      symbol,
      leverage: clampLeverage(f.leverage, bounds.min, bounds.max),
      isCross: bounds.onlyIsolated ? false : f.isCross,
    }));
    setMidLoading(true);
    try {
      const m = knownMid && knownMid > 0
        ? String(knownMid)
        : await api<string>("get_mid", { symbol });
      const midVal = Number(m);
      setMid(midVal);
      pushPrice(midVal);
      const range = suggestRange(midVal);
      setForm((f) => ({
        ...f,
        symbol,
        lowerPrice: range.lower,
        upperPrice: range.upper,
        leverage: clampLeverage(f.leverage, bounds.min, bounds.max),
        isCross: bounds.onlyIsolated ? false : f.isCross,
      }));
      setLevels([]);
      setPreview(null);
      setChartTrades([]);
      setCandles([]);
      setPriceHistory([{ time: Math.floor(Date.now() / 1000), value: midVal }]);
      void loadCandles(symbol, chartInterval);
    } catch (e: any) {
      setError(String(e));
    } finally {
      setMidLoading(false);
    }
  }

  useEffect(() => {
    void (async () => {
      try {
        const settings = await api<AppSettings>("get_settings");
        skipNextPersist.current = true;
        if (settings.env_path) setEnvPath(settings.env_path);
        if (settings.language) await i18n.changeLanguage(settings.language);
        setMode(settings.mode || "simulation");
        setPrivateKeyFromStorage(settings.private_key || "");
        setForm({
          symbol: settings.symbol || "BTC",
          lowerPrice: settings.lower_price || "",
          upperPrice: settings.upper_price || "",
          gridCount: settings.grid_count || 10,
          totalBudget: settings.total_budget || "1000",
          spacing: settings.spacing || "arithmetic",
          breakoutAction: settings.breakout_action || "pause",
          maxDrawdownPct: settings.max_drawdown_pct || "20",
          maxDailyLoss: settings.max_daily_loss || "100",
          maxOrderFailures: settings.max_order_failures || 5,
          leverage: settings.leverage || 5,
          isCross: settings.is_cross !== false,
        });
        if (settings.chart_mode === "candle" || settings.chart_mode === "line") {
          setChartMode(settings.chart_mode);
        }
        const iv = settings.chart_interval as ChartInterval;
        if (["1m", "5m", "15m", "1h", "4h", "1d"].includes(iv)) {
          setChartInterval(iv);
        }
        const account = await api<any>("get_account");
        setMode(account.mode || settings.mode || "simulation");
        setAddress(account.address || "");
        setBalances(account.balances || []);
        settingsReady.current = true;
        // Allow persist after state has settled.
        window.setTimeout(() => {
          skipNextPersist.current = false;
        }, 800);
        // Markets/mid after form hydrated from .env
        window.setTimeout(() => {
          void loadMarkets(settings.symbol || "BTC");
        }, 0);
      } catch (e) {
        console.warn(e);
        settingsReady.current = true;
        skipNextPersist.current = false;
      }
    })();
  }, []);

  useEffect(() => {
    if (!settingsReady.current || skipNextPersist.current) return;
    const id = window.setTimeout(() => {
      void persistSettings();
    }, 500);
    return () => window.clearTimeout(id);
  }, [persistSettings]);

  useEffect(() => {
    void loadMarkets();
  }, [mode]);

  useEffect(() => {
    if (!form.symbol) return;
    void loadCandles(form.symbol, chartInterval);
  }, [form.symbol, chartInterval, loadCandles]);

  useEffect(() => {
    if (!form.symbol) return;
    const pollMs =
      chartInterval === "1m"
        ? 15_000
        : chartInterval === "5m"
          ? 30_000
          : chartInterval === "15m"
            ? 60_000
            : 120_000;
    const id = window.setInterval(() => {
      void loadCandles(form.symbol, chartInterval, true);
    }, pollMs);
    return () => window.clearInterval(id);
  }, [form.symbol, chartInterval, loadCandles]);

  useEffect(() => {
    if (tab !== "configure" || !form.symbol) return;
    const id = window.setInterval(() => {
      void (async () => {
        try {
          const m = await api<string>("get_mid", { symbol: form.symbol });
          const v = Number(m);
          setMid(v);
          pushPrice(v);
        } catch {
          /* ignore poll errors */
        }
      })();
    }, 5000);
    return () => window.clearInterval(id);
  }, [tab, form.symbol]);

  useEffect(() => {
    let unlistenStatus: (() => void) | undefined;
    let unlistenEvent: (() => void) | undefined;
    void (async () => {
      unlistenStatus = await listen<BotSnapshot>("bot-status", (e) => {
        setStatus(e.payload);
        const m = e.payload.mid_price != null ? Number(e.payload.mid_price) : NaN;
        if (Number.isFinite(m) && m > 0) {
          setMid(m);
          pushPrice(m);
        }
      });
      unlistenEvent = await listen<any>("bot-event", async (e) => {
        const payload = e.payload;
        if (payload?.type === "filled" && payload.fill) {
          const fill = payload.fill;
          const side = String(fill.side || "").toLowerCase();
          const price = Number(fill.price);
          const size = String(fill.size ?? "");
          if (side === "buy" || side === "sell") {
            pushTrade(side, price, size, fill.client_id);
          }
        }
        setFills(await api("list_fills", { limit: 50 }));
        setEvents(await api("list_events", { limit: 50 }));
      });
    })();
    return () => {
      unlistenStatus?.();
      unlistenEvent?.();
    };
  }, []);

  async function refreshBalances() {
    setError("");
    try {
      const keyWasDirty = privateKeyDirty;
      const keyToSave = resolvePrivateKeyForSave();
      await api("set_mode", { mode });
      const addr = await api<string>("set_private_key", { privateKey: keyToSave });
      storedPrivateKeyRef.current = keyToSave;
      setPrivateKeyDirty(false);
      setPrivateKey(keyToSave ? PRIVATE_KEY_MASK : "");
      setShowPrivateKey(false);
      const account = await api<any>("get_account");
      setAddress(account.address || addr || "");
      setBalances(account.balances || []);
      if (keyWasDirty) {
        await persistSettings();
      }
    } catch (e: any) {
      setError(String(e));
    }
  }

  async function refreshMid() {
    setMidLoading(true);
    try {
      const m = await api<string>("get_mid", { symbol: form.symbol });
      const midVal = Number(m);
      setMid(midVal);
      pushPrice(midVal);
    } catch (e: any) {
      setError(String(e));
    } finally {
      setMidLoading(false);
    }
  }

  async function doPreview() {
    setError("");
    try {
      await refreshMid();
      const m = await api<string>("get_mid", { symbol: form.symbol });
      const midVal = Number(m);
      setMid(midVal);
      const p = await api<GridPreview>("preview_grid_cmd", {
        req: {
          symbol: form.symbol,
          lowerPrice: form.lowerPrice,
          upperPrice: form.upperPrice,
          gridCount: form.gridCount,
          totalBudget: form.totalBudget,
          spacing: form.spacing,
          midPrice: String(midVal),
        },
      });
      setPreview(p);
      setLevels(
        p.levels.map((l: any) => ({
          ...l,
          price: String(l.price),
          size: String(l.size),
          side: String(l.side).toLowerCase() as "buy" | "sell",
        })),
      );
    } catch (e: any) {
      setError(String(e));
    }
  }

  async function start() {
    setError("");
    setTip("");
    const live = status ?? (await api<BotSnapshot | null>("get_status").catch(() => null));
    const st = String(live?.status || "").toLowerCase();
    if (st === "running" || st === "paused") {
      setTip(t("app.alreadyRunningTip"));
      setTab("dashboard");
      return;
    }
    try {
      // Flatten overlay is driven by Rust flatten-start/end events only —
      // do not keep it up for the whole start_bot (place orders etc.).
      setChartTrades([]);
      setCandles([]);
      setPriceHistory([]);
      void loadCandles(form.symbol, chartInterval);
      const snap = await api<BotSnapshot>("start_bot", {
        req: {
          symbol: form.symbol,
          lowerPrice: form.lowerPrice,
          upperPrice: form.upperPrice,
          gridCount: form.gridCount,
          totalBudget: form.totalBudget,
          spacing: form.spacing,
          breakoutAction: form.breakoutAction,
          maxDrawdownPct: form.maxDrawdownPct,
          maxDailyLoss: form.maxDailyLoss,
          maxOrderFailures: form.maxOrderFailures,
          leverage: form.leverage,
          isCross: form.isCross,
        },
      });
      setStatus(snap);
      setTab("dashboard");
      if (snap.mid_price) {
        const m = Number(snap.mid_price);
        if (Number.isFinite(m)) {
          setMid(m);
          pushPrice(m);
        }
      }
    } catch (e: any) {
      const msg = String(e);
      if (/already running/i.test(msg)) {
        setTip(t("app.alreadyRunningTip"));
        setTab("dashboard");
        return;
      }
      setError(msg);
    }
  }

  async function changeLanguage(lng: string) {
    await i18n.changeLanguage(lng);
    await api("set_language", { language: lng });
    // Also refresh full .env so LANGUAGE stays aligned with other fields.
    if (settingsReady.current) {
      void persistSettings();
    }
  }

  function formatBalanceLabel(b: {
    asset: string;
    total: string;
    kind?: string;
  }) {
    const kind = b.kind || "spot";
    if (kind === "mode") {
      const modeKey: Record<string, string> = {
        unifiedAccount: "app.abstractionUnifiedAccount",
        portfolioMargin: "app.abstractionPortfolioMargin",
        disabled: "app.abstractionDisabled",
      };
      const modeLabel = t(modeKey[b.asset] || "app.abstractionUnknown");
      return `${t("app.balAccountMode")}: ${modeLabel}`;
    }
    const kindKey: Record<string, string> = {
      unified: "app.balKindUnified",
      spot: "app.balKindSpot",
      perp: "app.balKindPerp",
      position: "app.balKindPosition",
      sim: "app.balKindSim",
    };
    const kindLabel = t(kindKey[kind] || "app.balKindSpot");
    return `${b.asset} (${kindLabel}): ${String(b.total)}`;
  }

  function botStatusLabel(raw?: string | null) {
    const key = String(raw || "idle").toLowerCase();
    const map: Record<string, string> = {
      idle: "app.statusIdle",
      running: "app.statusRunning",
      paused: "app.statusPaused",
      halted: "app.statusHalted",
    };
    return t(map[key] || "app.statusIdle");
  }

  function fmtNum(v?: string | number | null, digits = 6) {
    if (v === undefined || v === null || v === "") return "—";
    const n = typeof v === "number" ? v : Number(v);
    if (!Number.isFinite(n)) return String(v);
    if (Math.abs(n) >= 1000) return n.toLocaleString(undefined, { maximumFractionDigits: 2 });
    return n.toLocaleString(undefined, { maximumFractionDigits: digits });
  }

  function pnlClass(v?: string | null) {
    const n = Number(v ?? 0);
    if (!Number.isFinite(n) || n === 0) return "pnl-flat";
    return n > 0 ? "pnl-pos" : "pnl-neg";
  }

  const totalPnl = (() => {
    const r = Number(status?.realized_pnl ?? 0);
    const u = Number(status?.unrealized_pnl ?? 0);
    if (!Number.isFinite(r) && !Number.isFinite(u)) return "0";
    return String((Number.isFinite(r) ? r : 0) + (Number.isFinite(u) ? u : 0));
  })();

  return (
    <div className="app">
      <FlattenOverlay />
      <header className="top">
        <div className="top-left">
          <div className="brand">{t("app.title")}</div>
          <nav className="tabs">
            {(["account", "configure", "dashboard"] as Tab[]).map((id) => (
              <button
                key={id}
                type="button"
                className={tab === id ? "tab active" : "tab"}
                onClick={() => setTab(id)}
              >
                {t(`app.${id}`)}
              </button>
            ))}
          </nav>
        </div>
        <div className="top-right">
          <span className="mode-pill">{t(`app.${mode}`)}</span>
          <div className="lang-switch" role="group" aria-label={t("app.language")}>
            <button
              type="button"
              className={i18n.language.startsWith("zh") ? "lang active" : "lang"}
              onClick={() => void changeLanguage("zh-CN")}
            >
              中文
            </button>
            <button
              type="button"
              className={i18n.language.startsWith("en") ? "lang active" : "lang"}
              onClick={() => void changeLanguage("en")}
            >
              EN
            </button>
          </div>
        </div>
      </header>

      {error && <div className="error">{error}</div>}
      {tip && (
        <div className="tip">
          {tip}
          <button type="button" className="tip-close" onClick={() => setTip("")}>
            ×
          </button>
        </div>
      )}

      {tab === "account" && (
        <section className="panel">
          <p className="hint">{t("app.depositHint")}</p>
          {envPath ? (
            <p className="hint env-path-hint">
              {t("app.envConfigHint")}: <code>{envPath}</code>
            </p>
          ) : null}
            <label>
            {t("app.mode")}
            <select
              value={mode}
              onChange={(e) => {
                const next = e.target.value;
                setMode(next);
                void (async () => {
                  try {
                    await api("set_mode", { mode: next });
                    const account = await api<any>("get_account");
                    setAddress(account.address || "");
                    setBalances(account.balances || []);
                    await loadMarkets();
                    if (form.symbol) {
                      const m = await api<string>("get_mid", { symbol: form.symbol });
                      const midVal = Number(m);
                      if (Number.isFinite(midVal) && midVal > 0) {
                        setMid(midVal);
                        // Keep saved band if already configured in .env.
                        setForm((f) => {
                          if (f.lowerPrice && f.upperPrice) return f;
                          const range = suggestRange(midVal);
                          return {
                            ...f,
                            lowerPrice: range.lower,
                            upperPrice: range.upper,
                          };
                        });
                      }
                    }
                  } catch (err: any) {
                    setError(String(err));
                  }
                })();
              }}
            >
              <option value="simulation">{t("app.simulation")}</option>
              <option value="testnet">{t("app.testnet")}</option>
              <option value="mainnet">{t("app.mainnet")}</option>
            </select>
          </label>
          <label className="private-key-field">
            {t("app.privateKey")}
            <div className="private-key-row">
              <input
                type={!showPrivateKey && privateKeyDirty ? "password" : "text"}
                autoComplete="off"
                spellCheck={false}
                value={
                  privateKeyDirty
                    ? privateKey
                    : showPrivateKey
                      ? storedPrivateKeyRef.current
                      : storedPrivateKeyRef.current
                        ? PRIVATE_KEY_MASK
                        : ""
                }
                placeholder={
                  storedPrivateKeyRef.current ? t("app.privateKeyKept") : "0x..."
                }
                onFocus={() => {
                  if (privateKeyDirty) return;
                  if (showPrivateKey && storedPrivateKeyRef.current) {
                    // Start editing from the revealed key.
                    setPrivateKeyDirty(true);
                    setPrivateKey(storedPrivateKeyRef.current);
                    return;
                  }
                  if (isPrivateKeyMask(privateKey)) {
                    setPrivateKey("");
                    setPrivateKeyDirty(true);
                  }
                }}
                onChange={(e) => {
                  setPrivateKeyDirty(true);
                  setPrivateKey(e.target.value);
                }}
                onBlur={() => {
                  if (privateKeyDirty && !privateKey.trim() && storedPrivateKeyRef.current) {
                    setPrivateKeyDirty(false);
                    setPrivateKey(PRIVATE_KEY_MASK);
                    setShowPrivateKey(false);
                  }
                }}
              />
              <button
                type="button"
                className="ghost"
                disabled={
                  !storedPrivateKeyRef.current &&
                  !(privateKeyDirty && privateKey.trim() && !isPrivateKeyMask(privateKey))
                }
                onClick={() => setShowPrivateKey((v) => !v)}
              >
                {showPrivateKey ? t("app.hideKey") : t("app.showKey")}
              </button>
            </div>
            <small>{t("app.privateKeyHelp")}</small>
          </label>
          <button type="button" onClick={() => void refreshBalances()}>
            {t("app.refreshBalances")}
          </button>
          <div className="meta">
            <h3>{t("app.balances")}</h3>
            <ul>
              {balances.length === 0 && <li className="hint">{t("app.balancesEmpty")}</li>}
              {balances.map((b, i) => (
                <li key={`${b.kind || "x"}-${b.asset}-${i}`}>{formatBalanceLabel(b)}</li>
              ))}
            </ul>
            <div>
              {t("app.address")}: <code>{address || "—"}</code>
            </div>
          </div>
        </section>
      )}

      {tab === "configure" && (
        <section className="panel grid-two">
          <div className="config-primary">
            <div className="market-card">
              <label className="market-symbol">
                <span className="field-label">{t("app.symbol")}</span>
                <select
                  value={form.symbol}
                  disabled={marketsLoading || markets.length === 0}
                  onChange={(e) => void applySymbol(e.target.value)}
                >
                  {markets.length === 0 && <option value={form.symbol}>{form.symbol}</option>}
                  {markets.map((m) => (
                    <option key={`${m.kind}-${m.symbol}`} value={m.symbol}>
                      {m.label} · {Number(m.mid).toLocaleString()}
                    </option>
                  ))}
                </select>
                <small>
                  {marketsLoading ? t("app.loadingMarkets") : t("app.symbolHelp")}
                </small>
              </label>

              <div className="leverage-panel">
                <div className="leverage-head">
                  <span className="field-label">{t("app.leverage")}</span>
                  <span className="leverage-badge">{form.leverage}x</span>
                </div>
                <input
                  type="range"
                  className="leverage-slider"
                  min={leverageBounds.min}
                  max={leverageBounds.max}
                  step={1}
                  value={clampLeverage(form.leverage, leverageBounds.min, leverageBounds.max)}
                  onChange={(e) =>
                    setForm({
                      ...form,
                      leverage: clampLeverage(
                        Number(e.target.value),
                        leverageBounds.min,
                        leverageBounds.max,
                      ),
                    })
                  }
                  style={
                    {
                      ["--lev-pct" as string]: `${
                        ((clampLeverage(form.leverage, leverageBounds.min, leverageBounds.max) -
                          leverageBounds.min) /
                          Math.max(1, leverageBounds.max - leverageBounds.min)) *
                        100
                      }%`,
                    } as CSSProperties
                  }
                />
                <div className="leverage-scale">
                  <span>{leverageBounds.min}x</span>
                  <span className="leverage-hint-inline">
                    {t("app.leverageRange", {
                      min: leverageBounds.min,
                      max: leverageBounds.max,
                    })}
                  </span>
                  <span>{leverageBounds.max}x</span>
                </div>
              </div>

              <div className="mid-panel">
                <div className="mid-main">
                  <span className="field-label">{t("app.liveMid")}</span>
                  <strong className="mid-value">
                    {midLoading ? "…" : mid > 0 ? mid.toLocaleString() : "—"}
                  </strong>
                </div>
                <div className="mid-actions">
                  <button type="button" className="ghost" onClick={() => void refreshMid()}>
                    {t("app.refreshPrice")}
                  </button>
                  <button
                    type="button"
                    className="ghost"
                    onClick={() => {
                      const range = suggestRange(mid);
                      setForm((f) => ({ ...f, lowerPrice: range.lower, upperPrice: range.upper }));
                    }}
                  >
                    {t("app.fitRange")}
                  </button>
                </div>
              </div>
            </div>
            <label>
              {t("app.lowerPrice")}
              <input
                value={form.lowerPrice}
                onChange={(e) => setForm({ ...form, lowerPrice: e.target.value })}
              />
              <small>{t("app.lowerHelp")}</small>
            </label>
            <label>
              {t("app.upperPrice")}
              <input
                value={form.upperPrice}
                onChange={(e) => setForm({ ...form, upperPrice: e.target.value })}
              />
              <small>{t("app.upperHelp")}</small>
            </label>
            <label>
              {t("app.gridCount")}
              <input
                type="number"
                value={form.gridCount}
                onChange={(e) => setForm({ ...form, gridCount: Number(e.target.value) })}
              />
              <small>{t("app.gridHelp")}</small>
            </label>
            <label>
              {t("app.totalBudget")}
              <input
                value={form.totalBudget}
                onChange={(e) => setForm({ ...form, totalBudget: e.target.value })}
              />
              <small>{t("app.budgetHelp")}</small>
            </label>
            <div className="row">
              <button type="button" onClick={doPreview}>
                {t("app.preview")}
              </button>
              <button type="button" className="primary" onClick={start}>
                {t("app.start")}
              </button>
            </div>
            {preview && (
              <p className="hint">
                {t("app.previewSummary", {
                  buys: preview.buy_count,
                  sells: preview.sell_count,
                  quote: Number(preview.estimated_quote_needed).toLocaleString(undefined, {
                    maximumFractionDigits: 2,
                  }),
                  base: Number(preview.estimated_base_needed).toLocaleString(undefined, {
                    maximumFractionDigits: 6,
                  }),
                })}
              </p>
            )}
          </div>
          <div className="config-chart-col">
            <GridChart
              mid={midNumber}
              levels={levels}
              restingOrders={status?.resting_orders ?? []}
              priceHistory={priceHistory}
              candles={candles}
              trades={chartTrades}
              mode={chartMode}
              onModeChange={setChartMode}
              interval={chartInterval}
              onIntervalChange={setChartInterval}
              loading={candlesLoading}
            />
            <div className="config-secondary">
              <h3 className="config-secondary-title">{t("app.tradeRiskSettings")}</h3>
              <div className="config-secondary-grid">
                <label>
                  {t("app.spacing")}
                  <select
                    value={form.spacing}
                    onChange={(e) => setForm({ ...form, spacing: e.target.value })}
                  >
                    <option value="arithmetic">{t("app.arithmetic")}</option>
                    <option value="geometric">{t("app.geometric")}</option>
                  </select>
                </label>
                <label>
                  {t("app.marginMode")}
                  <select
                    value={form.isCross ? "cross" : "isolated"}
                    disabled={leverageBounds.onlyIsolated}
                    onChange={(e) => setForm({ ...form, isCross: e.target.value === "cross" })}
                  >
                    <option value="cross">{t("app.marginCross")}</option>
                    <option value="isolated">{t("app.marginIsolated")}</option>
                  </select>
                  {leverageBounds.onlyIsolated && (
                    <small>{t("app.onlyIsolatedHint")}</small>
                  )}
                </label>
                <label>
                  {t("app.breakout")}
                  <select
                    value={form.breakoutAction}
                    onChange={(e) => setForm({ ...form, breakoutAction: e.target.value })}
                  >
                    <option value="alert_only">{t("app.alertOnly")}</option>
                    <option value="pause">{t("app.pause")}</option>
                    <option value="cancel_and_pause">{t("app.cancelAndPause")}</option>
                  </select>
                </label>
                <label>
                  {t("app.maxDrawdownPct")}
                  <input
                    value={form.maxDrawdownPct}
                    onChange={(e) => setForm({ ...form, maxDrawdownPct: e.target.value })}
                  />
                  <small>{t("app.drawdownHelp")}</small>
                </label>
                <label>
                  {t("app.maxDailyLoss")}
                  <input
                    value={form.maxDailyLoss}
                    onChange={(e) => setForm({ ...form, maxDailyLoss: e.target.value })}
                  />
                  <small>{t("app.dailyLossHelp")}</small>
                </label>
                <label>
                  {t("app.maxOrderFailures")}
                  <input
                    type="number"
                    min={1}
                    value={form.maxOrderFailures}
                    onChange={(e) =>
                      setForm({ ...form, maxOrderFailures: Number(e.target.value) || 1 })
                    }
                  />
                  <small>{t("app.orderFailHelp")}</small>
                </label>
              </div>
              <details className="advanced config-import">
                <summary>{t("app.importExport")}</summary>
                <div className="row">
                  <button
                    type="button"
                    onClick={async () => {
                      const json = await api<string>("export_strategy_config", {
                        cfg: {
                          symbol: form.symbol,
                          lower_price: form.lowerPrice,
                          upper_price: form.upperPrice,
                          grid_count: form.gridCount,
                          total_budget: form.totalBudget,
                          spacing: form.spacing,
                          breakout_action: form.breakoutAction,
                          max_drawdown_pct: form.maxDrawdownPct,
                          max_daily_loss: form.maxDailyLoss,
                          max_order_failures: form.maxOrderFailures,
                          leverage: form.leverage,
                          is_cross: form.isCross,
                        },
                      });
                      setConfigJson(json);
                    }}
                  >
                    {t("app.exportConfig")}
                  </button>
                  <button
                    type="button"
                    onClick={async () => {
                      if (!configJson.trim()) return;
                      const cfg = await api<any>("import_strategy_config", { json: configJson });
                      setForm({
                        ...form,
                        symbol: cfg.symbol,
                        lowerPrice: cfg.lower_price,
                        upperPrice: cfg.upper_price,
                        gridCount: cfg.grid_count,
                        totalBudget: cfg.total_budget,
                        spacing: cfg.spacing,
                        breakoutAction: cfg.breakout_action,
                        maxDrawdownPct: String(cfg.max_drawdown_pct ?? form.maxDrawdownPct),
                        maxDailyLoss: String(cfg.max_daily_loss ?? form.maxDailyLoss),
                        maxOrderFailures: Number(cfg.max_order_failures ?? form.maxOrderFailures),
                        leverage: Number(cfg.leverage ?? form.leverage),
                        isCross: cfg.is_cross ?? form.isCross,
                      });
                    }}
                  >
                    {t("app.importConfig")}
                  </button>
                </div>
                <textarea
                  rows={4}
                  value={configJson}
                  onChange={(e) => setConfigJson(e.target.value)}
                  placeholder="{}"
                />
              </details>
            </div>
          </div>
        </section>
      )}

      {tab === "dashboard" && (
        <section className="panel">
          <div className="stats">
            <div>
              <span className="stat-label">{t("app.status")}</span>
              <span className="stat-value">{botStatusLabel(status?.status)}</span>
            </div>
            <div>
              <span className="stat-label">{t("app.midPrice")}</span>
              <span className="stat-value">{fmtNum(status?.mid_price, 4)}</span>
            </div>
            <div>
              <span className="stat-label">{t("app.position")}</span>
              <span className="stat-value">
                {(() => {
                  const p = Number(status?.position_base ?? 0);
                  if (!Number.isFinite(p) || p === 0) return `0 ${status?.symbol || form.symbol}`;
                  const side = p > 0 ? t("app.legendBuy") : t("app.legendSell");
                  return `${side} ${fmtNum(Math.abs(p))} ${status?.symbol || form.symbol}`;
                })()}
              </span>
            </div>
            <div>
              <span className="stat-label">{t("app.avgEntry")}</span>
              <span className="stat-value">{fmtNum(status?.avg_entry_price, 4)}</span>
            </div>
            <div>
              <span className="stat-label">{t("app.openOrders")}</span>
              <span className="stat-value">{status?.open_orders ?? 0}</span>
            </div>
            <div>
              <span className="stat-label">{t("app.realizedPnl")}</span>
              <span className={`stat-value ${pnlClass(status?.realized_pnl)}`}>
                {fmtNum(status?.realized_pnl, 4)}
              </span>
            </div>
            <div>
              <span className="stat-label">{t("app.unrealizedPnl")}</span>
              <span className={`stat-value ${pnlClass(status?.unrealized_pnl)}`}>
                {fmtNum(status?.unrealized_pnl, 4)}
              </span>
            </div>
            <div>
              <span className="stat-label">{t("app.totalPnl")}</span>
              <span className={`stat-value ${pnlClass(totalPnl)}`}>{fmtNum(totalPnl, 4)}</span>
            </div>
          </div>
          <GridChart
            mid={midNumber}
            levels={levels}
            restingOrders={status?.resting_orders ?? []}
            priceHistory={priceHistory}
            candles={candles}
            trades={chartTrades}
            height={420}
            mode={chartMode}
            onModeChange={setChartMode}
            interval={chartInterval}
            onIntervalChange={setChartInterval}
            loading={candlesLoading}
          />
          <div className="row">
            <button
              type="button"
              className="btn-warn"
              onClick={async () => {
                setStatus(await api<BotSnapshot>("pause_bot"));
              }}
            >
              {t("app.pause")}
            </button>
            <button
              type="button"
              className="btn-ok"
              onClick={async () => {
                setStatus(await api<BotSnapshot>("resume_bot"));
              }}
            >
              {t("app.resume")}
            </button>
            <button
              type="button"
              className="btn-danger"
              onClick={async () => {
                setError("");
                try {
                  setStatus(await api<BotSnapshot>("stop_bot"));
                } catch (e: any) {
                  setError(String(e));
                }
              }}
            >
              {t("app.stop")}
            </button>
            <button
              type="button"
              className="btn-info"
              onClick={async () => {
                const path = `fills-${Date.now()}.csv`;
                await api("export_fills_csv", { path });
                alert(`exported ${path}`);
              }}
            >
              {t("app.exportCsv")}
            </button>
            <button
              type="button"
              className="btn-muted"
              onClick={async () => {
                const snap = await api<BotSnapshot | null>("clear_logs");
                setFills([]);
                setEvents([]);
                setChartTrades([]);
                if (snap) setStatus(snap);
              }}
            >
              {t("app.clearLogs")}
            </button>
          </div>
          <div className="log-header">
            <h3>{t("app.timeline")}</h3>
          </div>
          <ul className="log">
            {(status?.events_tail || []).map((e, i) => (
              <li key={i}>{e}</li>
            ))}
            {events.map((e, i) => (
              <li key={`ev-${i}`}>
                [{e.ts}] {e.kind}: {e.message}
              </li>
            ))}
          </ul>
          <div className="log-header">
            <h3>{t("app.fills")}</h3>
          </div>
          <ul className="log">
            {fills.length === 0 && <li>{t("app.noFills")}</li>}
            {fills.map((f, i) => (
              <li key={i}>
                {f.ts} {f.side} {f.size}@{f.price} pnl={f.pnl}
              </li>
            ))}
          </ul>
        </section>
      )}
    </div>
  );
}

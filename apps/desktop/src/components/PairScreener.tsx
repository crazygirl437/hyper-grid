import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { api, type Candle } from "../lib/api";
import {
  computeVolFromCandles,
  formatPct,
  formatUsdCompact,
  rankMarketsForGrid,
  type ScreenerMarket,
  type ScoredMarket,
  type VolMetrics,
} from "../lib/pairScore";

type Props = {
  markets: ScreenerMarket[];
  loading: boolean;
  currentSymbol?: string;
  onRefresh: () => void;
  onUse: (symbol: string, mid?: number, suggestedRangePct?: number) => void;
};

type SortKey =
  | "score"
  | "volume"
  | "change"
  | "vol"
  | "rv"
  | "funding"
  | "symbol"
  | "suggest";
type SortDir = "asc" | "desc";

const VOL_ANALYZE_LIMIT = 50;
const VOL_INTERVAL = "1h";
/**
 * Keep ≤60 bars so candleSnapshot weight stays at 20 + 1 = 21
 * (official: base 20 + extra weight per 60 items returned).
 * Docs: https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/rate-limits-and-user-limits
 */
const VOL_CANDLE_LIMIT = 60;
/**
 * IP budget = 1200 weight/min for all REST.
 * candleSnapshot ≈ 21 weight → theoretical max ≈ 57/min if exclusive.
 * Reserve ~half for mid/chart/markets → ~28/min → gap ≈ 2.1s.
 */
const VOL_REQUEST_GAP_MS = 2100;
/** After 429, wait for a meaningful slice of the 1-minute weight window to refill. */
const VOL_RATE_LIMIT_BACKOFF_MS = 20_000;

/** Survives tab switches even if the screener remounts. */
const volSessionCache: Record<string, VolMetrics> = {};

function formatFunding(rate: number | null) {
  if (rate == null || !Number.isFinite(rate)) return "—";
  const percent = rate * 100;
  const sign = percent > 0 ? "+" : "";
  return `${sign}${percent.toFixed(4)}%`;
}

function formatChange(pct: number | null) {
  if (pct == null || !Number.isFinite(pct)) return "—";
  const sign = pct > 0 ? "+" : "";
  return `${sign}${pct.toFixed(2)}%`;
}

function reasonLabel(t: (k: string) => string, key: string) {
  return t(`app.screenerReason.${key}`);
}

function sleep(ms: number) {
  return new Promise((r) => setTimeout(r, ms));
}

export function PairScreener({
  markets,
  loading,
  currentSymbol,
  onRefresh,
  onUse,
}: Props) {
  const { t } = useTranslation();
  const [query, setQuery] = useState("");
  const [minScore, setMinScore] = useState(50);
  const [minVolumeM, setMinVolumeM] = useState(1);
  const [maxFundingPct, setMaxFundingPct] = useState(0.02);
  const [sortKey, setSortKey] = useState<SortKey>("score");
  const [sortDir, setSortDir] = useState<SortDir>("desc");
  const [selected, setSelected] = useState<ScoredMarket | null>(null);
  const [volBySymbol, setVolBySymbol] = useState<Record<string, VolMetrics>>(
    () => ({ ...volSessionCache }),
  );
  const [volAnalyzing, setVolAnalyzing] = useState(false);
  const [volProgress, setVolProgress] = useState({ done: 0, total: 0 });
  const [volError, setVolError] = useState("");
  const volGen = useRef(0);

  const ranked = useMemo(
    () => rankMarketsForGrid(markets, volBySymbol),
    [markets, volBySymbol],
  );

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    const minVol = Math.max(0, minVolumeM) * 1e6;
    const maxFund = Math.max(0, maxFundingPct) / 100;

    let rows = ranked.filter((m) => {
      if (m.score < minScore) return false;
      if (m.volume < minVol) return false;
      if (m.fundingAbs != null && m.fundingAbs > maxFund) return false;
      if (!q) return true;
      const hay = `${m.symbol} ${m.label}`.toLowerCase();
      return hay.includes(q);
    });

    rows = [...rows].sort((a, b) => {
      let cmp = 0;
      switch (sortKey) {
        case "volume":
          cmp = a.volume - b.volume;
          break;
        case "change":
          cmp = Math.abs(a.dayChangePct ?? 0) - Math.abs(b.dayChangePct ?? 0);
          break;
        case "vol":
          cmp = (a.vol?.atrDailyPct ?? -1) - (b.vol?.atrDailyPct ?? -1);
          break;
        case "rv":
          cmp = (a.vol?.realizedVolDaily ?? -1) - (b.vol?.realizedVolDaily ?? -1);
          break;
        case "funding":
          cmp = (a.fundingAbs ?? 999) - (b.fundingAbs ?? 999);
          break;
        case "suggest":
          cmp = (a.suggestedRangePct ?? -1) - (b.suggestedRangePct ?? -1);
          break;
        case "symbol":
          cmp = a.symbol.localeCompare(b.symbol);
          break;
        default:
          cmp = a.score - b.score || a.volume - b.volume;
          break;
      }
      return sortDir === "asc" ? cmp : -cmp;
    });
    return rows;
  }, [ranked, query, minScore, minVolumeM, maxFundingPct, sortKey, sortDir]);

  function toggleSort(key: SortKey) {
    if (sortKey === key) {
      setSortDir((d) => (d === "asc" ? "desc" : "asc"));
      return;
    }
    setSortKey(key);
    setSortDir(key === "symbol" || key === "funding" ? "asc" : "desc");
  }

  function sortIndicator(key: SortKey) {
    if (sortKey !== key) return "";
    return sortDir === "asc" ? " ↑" : " ↓";
  }

  // Keep detail panel in sync when scores refresh after vol analysis.
  useEffect(() => {
    if (!selected) return;
    const next = ranked.find((m) => m.symbol === selected.symbol);
    if (next) setSelected(next);
  }, [ranked, selected?.symbol]);

  const top = filtered.slice(0, 3);
  const volCount = Object.keys(volBySymbol).length;

  async function analyzeVolatility() {
    if (volAnalyzing || markets.length === 0) return;
    const gen = ++volGen.current;
    setVolError("");
    setVolAnalyzing(true);

    // Always re-fetch top volume names so a second click refreshes ATR / scores.
    const candidates = [...markets]
      .sort((a, b) => Number(b.day_ntl_vlm || 0) - Number(a.day_ntl_vlm || 0))
      .slice(0, VOL_ANALYZE_LIMIT);

    setVolProgress({ done: 0, total: candidates.length });
    const next: Record<string, VolMetrics> = { ...volSessionCache };

    try {
      for (let i = 0; i < candidates.length; i++) {
        if (gen !== volGen.current) return;
        const m = candidates[i];
        let attempt = 0;
        while (attempt < 2) {
          attempt += 1;
          try {
            const candles = await api<Candle[]>("get_candles", {
              symbol: m.symbol,
              interval: VOL_INTERVAL,
              limit: VOL_CANDLE_LIMIT,
            });
            const metrics = computeVolFromCandles(
              candles,
              VOL_INTERVAL,
              VOL_CANDLE_LIMIT,
            );
            if (metrics) {
              next[m.symbol] = metrics;
              volSessionCache[m.symbol] = metrics;
            }
            break;
          } catch (e: any) {
            const msg = String(e);
            if (/429|too many requests/i.test(msg)) {
              setVolError(
                t("app.screenerVolRateLimited", {
                  done: i,
                  total: candidates.length,
                }),
              );
              await sleep(VOL_RATE_LIMIT_BACKOFF_MS);
              if (attempt >= 2) break;
              continue;
            }
            break;
          }
        }
        setVolBySymbol({ ...next });
        setVolProgress({ done: i + 1, total: candidates.length });
        if (i + 1 < candidates.length) await sleep(VOL_REQUEST_GAP_MS);
      }
      setVolBySymbol({ ...next });
      setVolError("");
    } finally {
      if (gen === volGen.current) setVolAnalyzing(false);
    }
  }

  function usePair(m: ScoredMarket) {
    const mid = Number(m.mid);
    onUse(
      m.symbol,
      Number.isFinite(mid) && mid > 0 ? mid : undefined,
      m.suggestedRangePct ?? undefined,
    );
  }

  return (
    <section className="panel screener">
      <div className="screener-head">
        <div>
          <h2 className="screener-title">{t("app.screenerTitle")}</h2>
          <p className="hint">{t("app.screenerHint")}</p>
          <p className="hint">
            {volCount > 0
              ? t("app.screenerVolStatus", { count: volCount })
              : t("app.screenerVolHint")}
          </p>
        </div>
        <div className="screener-head-actions">
          <button
            type="button"
            className="primary"
            onClick={() => void analyzeVolatility()}
            disabled={volAnalyzing || loading || markets.length === 0}
          >
            {volAnalyzing
              ? t("app.screenerVolAnalyzing", {
                  done: volProgress.done,
                  total: volProgress.total,
                })
              : volCount > 0
                ? t("app.screenerVolReanalyze")
                : t("app.screenerVolAnalyze")}
          </button>
          <button type="button" onClick={onRefresh} disabled={loading || volAnalyzing}>
            {loading ? t("app.loadingMarkets") : t("app.screenerRefresh")}
          </button>
        </div>
      </div>

      {volError && <p className="hint screener-vol-error">{volError}</p>}

      <div className="screener-filters">
        <label>
          {t("app.screenerSearch")}
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={t("app.symbolSearch")}
          />
        </label>
        <label>
          {t("app.screenerMinScore")}
          <input
            type="number"
            min={0}
            max={100}
            step={5}
            value={minScore}
            onChange={(e) => setMinScore(Number(e.target.value) || 0)}
          />
        </label>
        <label>
          {t("app.screenerMinVolume")}
          <input
            type="number"
            min={0}
            step={0.5}
            value={minVolumeM}
            onChange={(e) => setMinVolumeM(Number(e.target.value) || 0)}
          />
        </label>
        <label>
          {t("app.screenerMaxFunding")}
          <input
            type="number"
            min={0}
            step={0.005}
            value={maxFundingPct}
            onChange={(e) => setMaxFundingPct(Number(e.target.value) || 0)}
          />
        </label>
      </div>

      {top.length > 0 && (
        <div className="screener-top">
          {top.map((m, i) => (
            <div
              key={m.symbol}
              className={`screener-card grade-${m.grade.toLowerCase()}${
                selected?.symbol === m.symbol ? " selected" : ""
              }`}
              role="button"
              tabIndex={0}
              onClick={() => setSelected(m)}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") setSelected(m);
              }}
            >
              <div className="screener-card-rank">#{i + 1}</div>
              <div className="screener-card-main">
                <strong>{m.label || m.symbol}</strong>
                <span className={`screener-grade grade-${m.grade.toLowerCase()}`}>
                  {m.grade} · {m.score}
                </span>
              </div>
              <div className="screener-card-meta">
                <span>{formatUsdCompact(m.volume)}</span>
                <span title={t("app.screenerColAtr")}>
                  ATR {formatPct(m.vol?.atrDailyPct ?? null)}
                </span>
              </div>
              <button
                type="button"
                className="primary screener-use"
                onClick={(e) => {
                  e.stopPropagation();
                  usePair(m);
                }}
              >
                {t("app.screenerUse")}
              </button>
            </div>
          ))}
        </div>
      )}

      <div className="screener-table-wrap">
        <table className="screener-table">
          <thead>
            <tr>
              <th>{t("app.screenerColRank")}</th>
              <th>
                <button
                  type="button"
                  className={sortKey === "symbol" ? "screener-th-btn active" : "screener-th-btn"}
                  onClick={() => toggleSort("symbol")}
                >
                  {t("app.symbol")}
                  {sortIndicator("symbol")}
                </button>
              </th>
              <th>
                <button
                  type="button"
                  className={sortKey === "score" ? "screener-th-btn active" : "screener-th-btn"}
                  onClick={() => toggleSort("score")}
                >
                  {t("app.screenerColScore")}
                  {sortIndicator("score")}
                </button>
              </th>
              <th>
                <button
                  type="button"
                  className={sortKey === "volume" ? "screener-th-btn active" : "screener-th-btn"}
                  onClick={() => toggleSort("volume")}
                >
                  {t("app.screenerColVolume")}
                  {sortIndicator("volume")}
                </button>
              </th>
              <th>
                <button
                  type="button"
                  className={sortKey === "vol" ? "screener-th-btn active" : "screener-th-btn"}
                  onClick={() => toggleSort("vol")}
                >
                  {t("app.screenerColAtr")}
                  {sortIndicator("vol")}
                </button>
              </th>
              <th>
                <button
                  type="button"
                  className={sortKey === "rv" ? "screener-th-btn active" : "screener-th-btn"}
                  onClick={() => toggleSort("rv")}
                >
                  {t("app.screenerColRv")}
                  {sortIndicator("rv")}
                </button>
              </th>
              <th>
                <button
                  type="button"
                  className={sortKey === "change" ? "screener-th-btn active" : "screener-th-btn"}
                  onClick={() => toggleSort("change")}
                >
                  {t("app.screenerColChange")}
                  {sortIndicator("change")}
                </button>
              </th>
              <th>
                <button
                  type="button"
                  className={sortKey === "suggest" ? "screener-th-btn active" : "screener-th-btn"}
                  onClick={() => toggleSort("suggest")}
                >
                  {t("app.screenerColSuggest")}
                  {sortIndicator("suggest")}
                </button>
              </th>
              <th>
                <button
                  type="button"
                  className={sortKey === "funding" ? "screener-th-btn active" : "screener-th-btn"}
                  onClick={() => toggleSort("funding")}
                >
                  {t("app.fundingRate")}
                  {sortIndicator("funding")}
                </button>
              </th>
              <th />
            </tr>
          </thead>
          <tbody>
            {loading && markets.length === 0 ? (
              <tr>
                <td colSpan={10} className="screener-empty">
                  {t("app.loadingMarkets")}
                </td>
              </tr>
            ) : filtered.length === 0 ? (
              <tr>
                <td colSpan={10} className="screener-empty">
                  {t("app.screenerEmpty")}
                </td>
              </tr>
            ) : (
              filtered.map((m, i) => (
                <tr
                  key={m.symbol}
                  className={
                    selected?.symbol === m.symbol
                      ? "selected"
                      : currentSymbol === m.symbol
                        ? "current"
                        : undefined
                  }
                  onClick={() => setSelected(m)}
                >
                  <td>{i + 1}</td>
                  <td>
                    <div className="screener-sym">
                      <strong>{m.label || m.symbol}</strong>
                      {currentSymbol === m.symbol && (
                        <span className="screener-current-tag">
                          {t("app.screenerCurrent")}
                        </span>
                      )}
                      {m.vol && (
                        <span className="screener-current-tag screener-vol-tag">
                          K
                        </span>
                      )}
                    </div>
                  </td>
                  <td>
                    <span className={`screener-grade grade-${m.grade.toLowerCase()}`}>
                      {m.grade} {m.score}
                    </span>
                  </td>
                  <td>{formatUsdCompact(m.volume)}</td>
                  <td>{formatPct(m.vol?.atrDailyPct ?? null)}</td>
                  <td>{formatPct(m.vol?.realizedVolDaily ?? null)}</td>
                  <td className={changeClass(m.dayChangePct)}>
                    {formatChange(m.dayChangePct)}
                  </td>
                  <td>
                    {m.suggestedRangePct != null
                      ? `±${m.suggestedRangePct}%`
                      : "—"}
                  </td>
                  <td>{formatFunding(numOrNull(m.funding_rate))}</td>
                  <td>
                    <button
                      type="button"
                      className="primary"
                      onClick={(e) => {
                        e.stopPropagation();
                        usePair(m);
                      }}
                    >
                      {t("app.screenerUse")}
                    </button>
                  </td>
                </tr>
              ))
            )}
          </tbody>
        </table>
      </div>

      {selected && (
        <div className="screener-detail">
          <div className="screener-detail-head">
            <strong>
              {selected.label || selected.symbol} · {selected.grade} {selected.score}
            </strong>
            <button type="button" className="primary" onClick={() => usePair(selected)}>
              {t("app.screenerUse")}
            </button>
          </div>
          {selected.vol && (
            <div className="screener-vol-metrics">
              <span>
                {t("app.screenerColAtr")}: {formatPct(selected.vol.atrDailyPct)}
              </span>
              <span>
                {t("app.screenerColRv")}: {formatPct(selected.vol.realizedVolDaily)}
              </span>
              <span>
                {t("app.screenerColRange24h")}: {formatPct(selected.vol.range24hPct)}
              </span>
              <span>
                {t("app.screenerColChop")}: {selected.vol.choppiness.toFixed(2)}
              </span>
              {selected.suggestedRangePct != null && (
                <span>
                  {t("app.screenerColSuggest")}: ±{selected.suggestedRangePct}%
                </span>
              )}
            </div>
          )}
          <div className="screener-bars">
            {(
              [
                ["volume", selected.breakdown.volume],
                ["range", selected.breakdown.range],
                ["funding", selected.breakdown.funding],
                ["leverage", selected.breakdown.leverage],
              ] as const
            ).map(([key, val]) => (
              <div key={key} className="screener-bar-row">
                <span>{t(`app.screenerBreakdown.${key}`)}</span>
                <div className="screener-bar-track">
                  <div
                    className="screener-bar-fill"
                    style={{ width: `${Math.min(100, Math.max(0, val))}%` }}
                  />
                </div>
                <span>{val}</span>
              </div>
            ))}
          </div>
          <div className="screener-reasons">
            {selected.reasons.map((r) => (
              <span key={r} className="screener-chip">
                {reasonLabel(t, r)}
              </span>
            ))}
          </div>
          <p className="hint">{t("app.screenerWeights")}</p>
        </div>
      )}
    </section>
  );
}

function numOrNull(v?: string | number | null) {
  if (v === undefined || v === null || v === "") return null;
  const n = Number(v);
  return Number.isFinite(n) ? n : null;
}

function changeClass(pct: number | null) {
  if (pct == null || !Number.isFinite(pct) || pct === 0) return undefined;
  return pct > 0 ? "up" : "down";
}

/** Market row used by the pair screener (mirrors exchange MarketInfo). */
export type ScreenerMarket = {
  symbol: string;
  label: string;
  kind: string;
  mid: string;
  funding_rate?: string | null;
  day_ntl_vlm?: string | null;
  prev_day_px?: string | null;
  min_leverage?: number;
  max_leverage?: number;
  only_isolated?: boolean;
};

export type CandleBar = {
  time: number;
  open: string;
  high: string;
  low: string;
  close: string;
  volume: string;
};

/** Realized volatility from OHLCV (typically 1h bars). */
export type VolMetrics = {
  /** ATR(14) as % of last close (per bar). */
  atrPct: number;
  /** Daily-scaled ATR%: atrPct * sqrt(barsPerDay). */
  atrDailyPct: number;
  /** Std of log returns × sqrt(barsPerDay) × 100. */
  realizedVolDaily: number;
  /** (max high − min low) / close over last ~24h window × 100. */
  range24hPct: number;
  /** ATR / window-range — higher ≈ choppier (better for grids). */
  choppiness: number;
  barCount: number;
  interval: string;
};

export type ScoreBreakdown = {
  volume: number;
  range: number;
  funding: number;
  leverage: number;
};

export type ScoredMarket = ScreenerMarket & {
  score: number;
  grade: "A" | "B" | "C" | "D";
  dayChangePct: number | null;
  /** Metric used for the “move fit” score (ATR daily when available). */
  movePct: number | null;
  volume: number;
  fundingAbs: number | null;
  reasons: string[];
  breakdown: ScoreBreakdown;
  vol: VolMetrics | null;
  suggestedRangePct: number | null;
};

function clamp01(n: number) {
  if (!Number.isFinite(n)) return 0;
  return Math.min(1, Math.max(0, n));
}

function num(v?: string | number | null): number | null {
  if (v === undefined || v === null || v === "") return null;
  const n = Number(v);
  return Number.isFinite(n) ? n : null;
}

/** Ideal daily move band for range grids (percent). */
const RANGE_SWEET_MIN = 1.2;
const RANGE_SWEET_MAX = 5.5;
const RANGE_HARD_MAX = 14;

const ATR_PERIOD = 14;

function barsPerDay(interval: string): number {
  switch (interval) {
    case "1m":
      return 1440;
    case "5m":
      return 288;
    case "15m":
      return 96;
    case "1h":
      return 24;
    case "4h":
      return 6;
    case "1d":
      return 1;
    default:
      return 24;
  }
}

/**
 * Compute ATR / realized vol / 24h range from candles.
 * Expects chronological bars; uses the last `limit` bars.
 */
export function computeVolFromCandles(
  candles: CandleBar[],
  interval = "1h",
  limit = 72,
): VolMetrics | null {
  if (!candles.length) return null;
  const bars = candles.slice(-Math.max(ATR_PERIOD + 2, limit));
  if (bars.length < ATR_PERIOD + 1) return null;

  const closes: number[] = [];
  const highs: number[] = [];
  const lows: number[] = [];
  for (const c of bars) {
    const o = num(c.open);
    const h = num(c.high);
    const l = num(c.low);
    const cl = num(c.close);
    if (o == null || h == null || l == null || cl == null || cl <= 0) continue;
    closes.push(cl);
    highs.push(h);
    lows.push(l);
  }
  if (closes.length < ATR_PERIOD + 1) return null;

  const trs: number[] = [];
  for (let i = 1; i < closes.length; i++) {
    const prev = closes[i - 1];
    const h = highs[i];
    const l = lows[i];
    const tr = Math.max(h - l, Math.abs(h - prev), Math.abs(l - prev));
    trs.push(tr);
  }
  if (trs.length < ATR_PERIOD) return null;

  const atrWindow = trs.slice(-ATR_PERIOD);
  const atr = atrWindow.reduce((a, b) => a + b, 0) / atrWindow.length;
  const lastClose = closes[closes.length - 1];
  const atrPct = (atr / lastClose) * 100;
  const bpd = barsPerDay(interval);
  const atrDailyPct = atrPct * Math.sqrt(bpd);

  const logRets: number[] = [];
  for (let i = 1; i < closes.length; i++) {
    if (closes[i - 1] > 0 && closes[i] > 0) {
      logRets.push(Math.log(closes[i] / closes[i - 1]));
    }
  }
  let realizedVolDaily = 0;
  if (logRets.length >= 8) {
    const mean = logRets.reduce((a, b) => a + b, 0) / logRets.length;
    const variance =
      logRets.reduce((a, r) => a + (r - mean) ** 2, 0) / (logRets.length - 1);
    realizedVolDaily = Math.sqrt(Math.max(0, variance)) * Math.sqrt(bpd) * 100;
  }

  const win = Math.min(closes.length, Math.max(2, Math.round(bpd)));
  const sliceH = highs.slice(-win);
  const sliceL = lows.slice(-win);
  const maxH = Math.max(...sliceH);
  const minL = Math.min(...sliceL);
  const range24hPct = ((maxH - minL) / lastClose) * 100;

  const windowTrSum = trs.slice(-win).reduce((a, b) => a + b, 0);
  const windowSpan = Math.max(1e-12, maxH - minL);
  const choppiness = clamp01(windowTrSum / windowSpan / Math.max(1, win / 4));

  return {
    atrPct,
    atrDailyPct,
    realizedVolDaily,
    range24hPct,
    choppiness,
    barCount: closes.length,
    interval,
  };
}

/** Suggested mid ± % band from realized vol (for configure). */
export function suggestGridRangePct(vol: VolMetrics): number {
  const raw = Math.max(vol.atrDailyPct * 1.25, vol.range24hPct * 0.55);
  const rounded = Math.round(raw * 10) / 10;
  return Math.min(12, Math.max(2, rounded));
}

function scoreMovePct(abs: number): number {
  if (abs < RANGE_SWEET_MIN) {
    return 35 + (abs / RANGE_SWEET_MIN) * 40;
  }
  if (abs <= RANGE_SWEET_MAX) return 100;
  if (abs <= RANGE_HARD_MAX) {
    const t = (abs - RANGE_SWEET_MAX) / (RANGE_HARD_MAX - RANGE_SWEET_MAX);
    return 100 - t * 70;
  }
  return 15;
}

/**
 * Score a market for neutral range-grid suitability (0–100).
 * When `vol` is present, move-fit uses daily ATR instead of simple 24h change.
 */
export function scoreMarketForGrid(
  m: ScreenerMarket,
  maxVolume: number,
  vol?: VolMetrics | null,
): ScoredMarket {
  const mid = num(m.mid) ?? 0;
  const volume = Math.max(0, num(m.day_ntl_vlm) ?? 0);
  const prev = num(m.prev_day_px);
  const funding = num(m.funding_rate ?? null);
  const maxLev = Math.max(1, Number(m.max_leverage) || 1);
  const onlyIsolated = !!m.only_isolated;

  let dayChangePct: number | null = null;
  if (prev != null && prev > 0 && mid > 0) {
    dayChangePct = ((mid - prev) / prev) * 100;
  }

  const volMetrics = vol ?? null;
  let movePct: number | null = null;
  if (volMetrics) {
    movePct = volMetrics.atrDailyPct;
  } else if (dayChangePct != null) {
    movePct = Math.abs(dayChangePct);
  }

  // Volume: log scale vs the deepest book in the list.
  const volRatio =
    maxVolume > 0 && volume > 0
      ? Math.log10(volume + 1) / Math.log10(maxVolume + 1)
      : 0;
  const volumeScore = clamp01(volRatio) * 100;

  // Prefer moderate oscillation; punish dead markets and breakouts.
  let rangeScore = 55;
  if (movePct != null) {
    rangeScore = scoreMovePct(movePct);
    // Mild bonus when price is choppy (mean-reverting path) vs a one-way drift.
    if (volMetrics && volMetrics.choppiness >= 0.55 && movePct <= RANGE_HARD_MAX) {
      rangeScore = Math.min(100, rangeScore + 8);
    }
  }

  // Funding: lower |rate| is safer for inventories that linger.
  let fundingScore = 70;
  let fundingAbs: number | null = null;
  if (funding != null) {
    fundingAbs = Math.abs(funding);
    const hourlyPct = fundingAbs * 100;
    if (hourlyPct <= 0.001) fundingScore = 100;
    else if (hourlyPct <= 0.005) fundingScore = 90;
    else if (hourlyPct <= 0.01) fundingScore = 75;
    else if (hourlyPct <= 0.03) fundingScore = 45;
    else fundingScore = 15;
  }

  // Leverage headroom / margin mode.
  let leverageScore = 70;
  if (maxLev >= 20) leverageScore = 100;
  else if (maxLev >= 10) leverageScore = 85;
  else if (maxLev >= 5) leverageScore = 65;
  else leverageScore = 40;
  if (onlyIsolated) leverageScore = Math.max(20, leverageScore - 15);

  const breakdown: ScoreBreakdown = {
    volume: Math.round(volumeScore),
    range: Math.round(rangeScore),
    funding: Math.round(fundingScore),
    leverage: Math.round(leverageScore),
  };

  const score = Math.round(
    breakdown.volume * 0.4 +
      breakdown.range * 0.3 +
      breakdown.funding * 0.2 +
      breakdown.leverage * 0.1,
  );

  const grade: ScoredMarket["grade"] =
    score >= 80 ? "A" : score >= 65 ? "B" : score >= 50 ? "C" : "D";

  const reasons: string[] = [];
  if (breakdown.volume >= 75) reasons.push("highVolume");
  else if (breakdown.volume < 40) reasons.push("lowVolume");

  if (volMetrics) {
    reasons.push("volFromCandles");
    const abs = volMetrics.atrDailyPct;
    if (abs >= RANGE_SWEET_MIN && abs <= RANGE_SWEET_MAX) reasons.push("goodRange");
    else if (abs < RANGE_SWEET_MIN) reasons.push("tooQuiet");
    else if (abs > RANGE_HARD_MAX) reasons.push("tooVolatile");
    else reasons.push("elevatedMove");
    if (volMetrics.choppiness >= 0.55) reasons.push("choppyGood");
    else if (volMetrics.choppiness < 0.35) reasons.push("trendingPath");
  } else if (dayChangePct != null) {
    const abs = Math.abs(dayChangePct);
    if (abs >= RANGE_SWEET_MIN && abs <= RANGE_SWEET_MAX) reasons.push("goodRange");
    else if (abs < RANGE_SWEET_MIN) reasons.push("tooQuiet");
    else if (abs > RANGE_HARD_MAX) reasons.push("tooVolatile");
    else reasons.push("elevatedMove");
  } else {
    reasons.push("noDayChange");
  }

  if (fundingAbs != null) {
    if (fundingAbs * 100 <= 0.005) reasons.push("fundingOk");
    else if (fundingAbs * 100 > 0.01) reasons.push("fundingHigh");
  }
  if (onlyIsolated) reasons.push("isolatedOnly");

  const suggestedRangePct = volMetrics ? suggestGridRangePct(volMetrics) : null;

  return {
    ...m,
    score,
    grade,
    dayChangePct,
    movePct,
    volume,
    fundingAbs,
    reasons,
    breakdown,
    vol: volMetrics,
    suggestedRangePct,
  };
}

export function rankMarketsForGrid(
  markets: ScreenerMarket[],
  volBySymbol?: Record<string, VolMetrics | null | undefined>,
): ScoredMarket[] {
  const maxVolume = markets.reduce((mx, m) => {
    const v = num(m.day_ntl_vlm) ?? 0;
    return v > mx ? v : mx;
  }, 0);
  return markets
    .map((m) => scoreMarketForGrid(m, maxVolume, volBySymbol?.[m.symbol]))
    .sort((a, b) => b.score - a.score || b.volume - a.volume);
}

export function formatUsdCompact(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return "—";
  if (n >= 1e9) return `$${(n / 1e9).toFixed(2)}B`;
  if (n >= 1e6) return `$${(n / 1e6).toFixed(1)}M`;
  if (n >= 1e3) return `$${(n / 1e3).toFixed(0)}K`;
  return `$${n.toFixed(0)}`;
}

export function formatPct(n: number | null | undefined, digits = 2): string {
  if (n == null || !Number.isFinite(n)) return "—";
  return `${n.toFixed(digits)}%`;
}

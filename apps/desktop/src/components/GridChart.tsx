import { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import {
  CandlestickData,
  createChart,
  IChartApi,
  IPriceLine,
  ISeriesApi,
  LineData,
  LineStyle,
  SeriesMarker,
  Time,
} from "lightweight-charts";
import type { Candle, ChartInterval, ChartMode, GridLevel, RestingOrder } from "../lib/api";
import { CHART_INTERVALS } from "../lib/api";

export type ChartTrade = {
  id: string;
  time: number; // unix seconds
  price: number;
  side: "buy" | "sell";
  size?: string;
};

export type PricePoint = {
  time: number;
  value: number;
};

type SeriesAny = ISeriesApi<"Line"> | ISeriesApi<"Candlestick">;

type Props = {
  mid: number;
  levels: GridLevel[];
  restingOrders?: RestingOrder[];
  priceHistory?: PricePoint[];
  candles?: Candle[];
  trades?: ChartTrade[];
  height?: number;
  mode: ChartMode;
  onModeChange: (mode: ChartMode) => void;
  interval: ChartInterval;
  onIntervalChange: (interval: ChartInterval) => void;
  loading?: boolean;
};

function toTime(sec: number): Time {
  return Math.max(1, Math.floor(sec)) as Time;
}

function applyMidToLast(bars: CandlestickData[], mid: number): CandlestickData[] {
  if (bars.length === 0 || !(mid > 0)) return bars;
  const last = { ...bars[bars.length - 1] };
  last.close = mid;
  last.high = Math.max(last.high, mid);
  last.low = Math.min(last.low, mid);
  return [...bars.slice(0, -1), last];
}

function pickNearestOrders(orders: RestingOrder[], mid: number, limit = 2): RestingOrder[] {
  const valid = orders
    .map((o) => ({ order: o, price: Number(o.price) }))
    .filter((x) => Number.isFinite(x.price) && x.price > 0);
  if (valid.length === 0) return [];

  const ref = mid > 0 ? mid : valid.reduce((s, x) => s + x.price, 0) / valid.length;
  const buys = valid
    .filter((x) => x.order.side === "buy")
    .sort((a, b) => b.price - a.price); // nearest buy = highest below mid
  const sells = valid
    .filter((x) => x.order.side === "sell")
    .sort((a, b) => a.price - b.price); // nearest sell = lowest above mid

  const picked: RestingOrder[] = [];
  const bestBuy = buys.find((x) => x.price <= ref) ?? buys[0];
  const bestSell = sells.find((x) => x.price >= ref) ?? sells[0];
  if (bestBuy) picked.push(bestBuy.order);
  if (bestSell && bestSell.order !== bestBuy?.order) picked.push(bestSell.order);

  if (picked.length < limit) {
    const rest = valid
      .filter((x) => !picked.includes(x.order))
      .sort((a, b) => Math.abs(a.price - ref) - Math.abs(b.price - ref));
    for (const x of rest) {
      if (picked.length >= limit) break;
      picked.push(x.order);
    }
  }
  return picked.slice(0, limit);
}

function applyOverlays(
  series: SeriesAny,
  linesRef: { current: IPriceLine[] },
  levels: GridLevel[],
  restingOrders: RestingOrder[],
  trades: ChartTrade[],
  times: number[],
  mid: number,
  t: (key: string) => string,
  lang: string
) {
  for (const line of linesRef.current) {
    series.removePriceLine(line);
  }
  linesRef.current = [];

  const buyLabel = t("app.legendBuy").replace(/^▲\s*/, "");
  const sellLabel = t("app.legendSell").replace(/^▼\s*/, "");
  const orderBuy = t("app.orderLineBuy");
  const orderSell = t("app.orderLineSell");

  for (const level of levels) {
    const price = Number(level.price);
    if (!Number.isFinite(price)) continue;
    const line = series.createPriceLine({
      price,
      color: level.side === "buy" ? "#bbf7d0" : "#fecaca",
      lineWidth: 1,
      lineStyle: LineStyle.Dashed,
      axisLabelVisible: false,
      title: level.side === "buy" ? buyLabel : sellLabel,
    });
    linesRef.current.push(line);
  }

  // Only the two nearest resting orders (best bid / best ask), dashed.
  const nearest = pickNearestOrders(restingOrders, mid, 2);
  for (const order of nearest) {
    const price = Number(order.price);
    if (!Number.isFinite(price) || price <= 0) continue;
    const isBuy = order.side === "buy";
    const line = series.createPriceLine({
      price,
      color: isBuy ? "#16a34a" : "#dc2626",
      lineWidth: 2,
      lineStyle: LineStyle.Dashed,
      axisLabelVisible: true,
      title: isBuy ? orderBuy : orderSell,
    });
    linesRef.current.push(line);
  }

  const buyText = lang.startsWith("zh") ? "买" : "B";
  const sellText = lang.startsWith("zh") ? "卖" : "S";
  const markers: SeriesMarker<Time>[] = trades
    .filter((tr) => Number.isFinite(tr.price) && tr.price > 0)
    .map((tr) => {
      const isBuy = tr.side === "buy";
      return {
        time: toTime(tr.time),
        position: (isBuy ? "belowBar" : "aboveBar") as "belowBar" | "aboveBar",
        color: isBuy ? "#16a34a" : "#dc2626",
        shape: (isBuy ? "arrowUp" : "arrowDown") as "arrowUp" | "arrowDown",
        text: isBuy ? `${buyText} ${tr.price}` : `${sellText} ${tr.price}`,
      };
    })
    .sort((a, b) => Number(a.time) - Number(b.time));

  const snapped = markers.map((m) => {
    const tt = Number(m.time);
    if (times.includes(tt)) return m;
    let best = times[0] ?? tt;
    let bestDist = Math.abs(best - tt);
    for (const pt of times) {
      const d = Math.abs(pt - tt);
      if (d < bestDist) {
        best = pt;
        bestDist = d;
      }
    }
    return { ...m, time: toTime(best) };
  });
  const byTime = new Map<number, SeriesMarker<Time>>();
  for (const m of snapped) {
    byTime.set(Number(m.time), m);
  }
  series.setMarkers([...byTime.values()].sort((a, b) => Number(a.time) - Number(b.time)));
}

export function GridChart({
  mid,
  levels,
  restingOrders = [],
  priceHistory = [],
  candles = [],
  trades = [],
  height = 380,
  mode,
  onModeChange,
  interval,
  onIntervalChange,
  loading = false,
}: Props) {
  const { t, i18n } = useTranslation();
  const ref = useRef<HTMLDivElement | null>(null);
  const chartRef = useRef<IChartApi | null>(null);
  const seriesRef = useRef<SeriesAny | null>(null);
  const linesRef = useRef<IPriceLine[]>([]);
  const fittedKeyRef = useRef("");

  useEffect(() => {
    if (!ref.current) return;
    const chart = createChart(ref.current, {
      height,
      layout: {
        background: { color: "#ffffff" },
        textColor: "#111827",
      },
      grid: {
        vertLines: { color: "#f3f4f6" },
        horzLines: { color: "#f3f4f6" },
      },
      rightPriceScale: { borderVisible: false },
      timeScale: {
        borderVisible: false,
        timeVisible: true,
        secondsVisible: mode === "line",
      },
    });

    const series: SeriesAny =
      mode === "candle"
        ? chart.addCandlestickSeries({
            upColor: "#16a34a",
            downColor: "#dc2626",
            borderUpColor: "#16a34a",
            borderDownColor: "#dc2626",
            wickUpColor: "#16a34a",
            wickDownColor: "#dc2626",
            priceLineVisible: false,
            lastValueVisible: true,
          })
        : chart.addLineSeries({
            color: "#0f766e",
            lineWidth: 2,
            priceLineVisible: false,
            lastValueVisible: true,
          });

    chartRef.current = chart;
    seriesRef.current = series;
    fittedKeyRef.current = "";
    const ro = new ResizeObserver(() => {
      if (ref.current) chart.applyOptions({ width: ref.current.clientWidth });
    });
    ro.observe(ref.current);
    return () => {
      ro.disconnect();
      chart.remove();
      chartRef.current = null;
      seriesRef.current = null;
      linesRef.current = [];
    };
  }, [height, mode]);

  useEffect(() => {
    const series = seriesRef.current;
    const chart = chartRef.current;
    if (!series || !chart) return;

    let times: number[] = [];

    if (mode === "candle") {
      let data: CandlestickData[] = candles
        .map((c) => {
          const open = Number(c.open);
          const high = Number(c.high);
          const low = Number(c.low);
          const close = Number(c.close);
          if (
            !Number.isFinite(open) ||
            !Number.isFinite(high) ||
            !Number.isFinite(low) ||
            !Number.isFinite(close) ||
            c.time <= 0
          ) {
            return null;
          }
          return { time: toTime(c.time), open, high, low, close };
        })
        .filter((x): x is CandlestickData => x !== null);

      if (data.length === 0 && mid > 0) {
        const now = Math.floor(Date.now() / 1000);
        data = [{ time: toTime(now), open: mid, high: mid, low: mid, close: mid }];
      }
      if (data.length === 0) return;
      data = applyMidToLast(data, mid);
      (series as ISeriesApi<"Candlestick">).setData(data);
      times = data.map((d) => Number(d.time));
    } else {
      let data: LineData[] = [];
      if (priceHistory.length > 0) {
        const sorted = [...priceHistory].sort((a, b) => a.time - b.time);
        const dedup = new Map<number, number>();
        for (const p of sorted) {
          if (Number.isFinite(p.value) && p.value > 0) {
            dedup.set(Math.floor(p.time), p.value);
          }
        }
        data = [...dedup.entries()].map(([time, value]) => ({
          time: toTime(time),
          value,
        }));
      } else if (mid > 0) {
        const now = Math.floor(Date.now() / 1000);
        data = [{ time: toTime(now), value: mid }];
      }
      if (data.length === 0) return;
      (series as ISeriesApi<"Line">).setData(data);
      times = data.map((d) => Number(d.time));
    }

    applyOverlays(series, linesRef, levels, restingOrders, trades, times, mid, t, i18n.language);

    const fitKey =
      mode === "candle"
        ? `candle:${interval}:${times.length}:${times[0] ?? 0}`
        : `line:${times.length}:${times[0] ?? 0}`;
    if (fittedKeyRef.current !== fitKey) {
      fittedKeyRef.current = fitKey;
      chart.timeScale().fitContent();
    }
  }, [
    mode,
    mid,
    levels,
    restingOrders,
    priceHistory,
    candles,
    trades,
    interval,
    t,
    i18n.language,
  ]);

  return (
    <div className="chart-wrap">
      <div className="chart-toolbar">
        <div className="chart-controls">
          <div className="chart-interval-group" role="group" aria-label={t("app.chartMode")}>
            <button
              type="button"
              className={`chart-interval-btn${mode === "line" ? " active" : ""}`}
              onClick={() => onModeChange("line")}
            >
              {t("app.chartModeLine")}
            </button>
            <button
              type="button"
              className={`chart-interval-btn${mode === "candle" ? " active" : ""}`}
              onClick={() => onModeChange("candle")}
            >
              {t("app.chartModeCandle")}
            </button>
          </div>
          {mode === "candle" && (
            <div className="chart-interval-group" role="group" aria-label={t("app.chartInterval")}>
              {CHART_INTERVALS.map((iv) => (
                <button
                  key={iv}
                  type="button"
                  className={`chart-interval-btn${interval === iv ? " active" : ""}`}
                  onClick={() => onIntervalChange(iv)}
                >
                  {iv}
                </button>
              ))}
            </div>
          )}
        </div>
        <div className="chart-legend-bar">
          {loading && mode === "candle" && (
            <span className="legend-loading">{t("app.chartLoading")}</span>
          )}
          {mode === "line" && <span className="legend-price">{t("app.legendMid")}</span>}
          <span className="legend-buy">{t("app.legendBuy")}</span>
          <span className="legend-sell">{t("app.legendSell")}</span>
          <span className="legend-order">{t("app.legendOrder")}</span>
          <span className="legend-grid">{t("app.legendGrid")}</span>
        </div>
      </div>
      <div ref={ref} className="chart" />
    </div>
  );
}

type EquityPoint = {
  ts_ms: number;
  net_pnl: string;
  realized_pnl: string;
  unrealized_pnl: string;
  funding_cum: string;
};

type Props = {
  points: EquityPoint[];
  mode: "mark" | "closed";
  height?: number;
  emptyLabel: string;
};

function num(v: string | number | null | undefined) {
  const n = typeof v === "number" ? v : Number(v);
  return Number.isFinite(n) ? n : 0;
}

/** Nice round ticks covering [min, max], always including 0 when range crosses it. */
function niceTicks(min: number, max: number, count = 5): number[] {
  if (!Number.isFinite(min) || !Number.isFinite(max)) return [0];
  if (min === max) {
    const pad = Math.abs(min) || 1;
    min -= pad * 0.1;
    max += pad * 0.1;
  }
  const span = max - min;
  const raw = span / Math.max(count - 1, 1);
  const mag = Math.pow(10, Math.floor(Math.log10(Math.abs(raw) || 1)));
  const norm = raw / mag;
  const step =
    (norm <= 1.5 ? 1 : norm <= 3 ? 2 : norm <= 7 ? 5 : 10) * mag;
  const start = Math.floor(min / step) * step;
  const end = Math.ceil(max / step) * step;
  const ticks: number[] = [];
  for (let v = start; v <= end + step * 0.5; v += step) {
    const rounded = Math.abs(v) < step * 1e-9 ? 0 : Number(v.toPrecision(12));
    ticks.push(rounded);
  }
  if (!ticks.includes(0) && min < 0 && max > 0) {
    ticks.push(0);
    ticks.sort((a, b) => a - b);
  }
  return ticks.length ? ticks : [min, max];
}

function formatAxisValue(v: number): string {
  const abs = Math.abs(v);
  if (abs >= 1000) {
    return v.toLocaleString(undefined, { maximumFractionDigits: 1 });
  }
  if (abs >= 1) {
    return v.toLocaleString(undefined, {
      minimumFractionDigits: 0,
      maximumFractionDigits: 2,
    });
  }
  if (abs === 0) return "0";
  return v.toLocaleString(undefined, {
    minimumFractionDigits: 2,
    maximumFractionDigits: 4,
  });
}

function formatAxisTime(tsMs: number): string {
  if (!tsMs) return "";
  const d = new Date(tsMs);
  if (Number.isNaN(d.getTime())) return "";
  return d.toLocaleString(undefined, {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  });
}

/** Lightweight SVG equity curve with Y/X scales. */
export function EquityCurve({ points, mode, height = 220, emptyLabel }: Props) {
  if (!points.length) {
    return (
      <div className="equity-curve-empty" style={{ height }}>
        {emptyLabel}
      </div>
    );
  }

  const values = points.map((p) =>
    mode === "mark"
      ? num(p.net_pnl)
      : num(p.realized_pnl) + num(p.funding_cum),
  );
  const dataMin = Math.min(...values);
  const dataMax = Math.max(...values);
  const pad = Math.max(
    (dataMax - dataMin) * 0.08,
    Math.abs(dataMax || dataMin) * 0.02,
    0.01,
  );
  let min = Math.min(dataMin, 0) - (dataMin < 0 ? pad : 0);
  let max =
    Math.max(dataMax, 0) +
    (dataMax > 0 ? pad : dataMin === 0 && dataMax === 0 ? pad : 0);
  const yTicks = niceTicks(min, max, 5);
  min = Math.min(...yTicks);
  max = Math.max(...yTicks);
  const span = max - min || 1;

  const plotH = 180;

  const w = 640;
  const h = 200;
  const padL = 2;
  const padR = 4;
  const padT = 10;
  const padB = 10;
  const innerW = w - padL - padR;
  const innerH = h - padT - padB;

  const yOf = (v: number) => padT + (1 - (v - min) / span) * innerH;
  const xOf = (i: number) =>
    padL + (i / Math.max(values.length - 1, 1)) * innerW;

  const coords = values.map((v, i) => `${xOf(i).toFixed(1)},${yOf(v).toFixed(1)}`);
  const last = values[values.length - 1] ?? 0;
  const stroke = last >= 0 ? "#0f766e" : "#b91c1c";

  const xLabelIdx = [
    0,
    Math.floor((values.length - 1) / 2),
    values.length - 1,
  ].filter((v, i, arr) => arr.indexOf(v) === i);

  return (
    <div className="equity-curve-frame" style={{ minHeight: height }}>
      <div className="equity-y-scale" style={{ height: plotH }} aria-hidden="true">
        {yTicks.map((tick) => {
          const topPct = (yOf(tick) / h) * 100;
          return (
            <span key={`y-${tick}`} style={{ top: `${topPct}%` }}>
              {formatAxisValue(tick)}
            </span>
          );
        })}
      </div>
      <div className="equity-plot-col">
        <svg
          className="equity-curve"
          viewBox={`0 0 ${w} ${h}`}
          preserveAspectRatio="none"
          role="img"
          aria-label="equity curve"
          style={{ height: plotH }}
        >
          {yTicks.map((tick) => {
            const y = yOf(tick);
            const isZero = tick === 0;
            return (
              <line
                key={`g-${tick}`}
                x1={padL}
                x2={w - padR}
                y1={y}
                y2={y}
                className={isZero ? "equity-curve-zero" : "equity-curve-grid"}
              />
            );
          })}
          <polyline
            fill="none"
            stroke={stroke}
            strokeWidth="2"
            vectorEffect="non-scaling-stroke"
            points={coords.join(" ")}
          />
        </svg>
        <div className="equity-x-scale" aria-hidden="true">
          {xLabelIdx.map((i) => {
            const align =
              i === 0 ? "start" : i === values.length - 1 ? "end" : "center";
            const left =
              i === 0
                ? "0%"
                : i === values.length - 1
                  ? "100%"
                  : `${(i / Math.max(values.length - 1, 1)) * 100}%`;
            return (
              <span
                key={`x-${i}`}
                className={`equity-x-label equity-x-${align}`}
                style={{ left }}
              >
                {formatAxisTime(points[i]?.ts_ms ?? 0)}
              </span>
            );
          })}
        </div>
      </div>
    </div>
  );
}

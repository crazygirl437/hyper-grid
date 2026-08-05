import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "../lib/api";
import { ConfirmDialog } from "./ConfirmDialog";
import { EquityCurve } from "./EquityCurve";
import { botStatusI18nKey } from "../lib/botStatus";

export type SessionPnlSummary = {
  session_id: string;
  symbol: string;
  fill_count: number;
  gross_closed_pnl: string;
  fees: string;
  funding: string;
  net_pnl: string;
};

export type SessionListItem = {
  session_id: string;
  strategy_id: string;
  symbol: string;
  status: string;
  created_at_ms: number;
  updated_at_ms: number;
  active: boolean;
  fill_count: number;
  net_pnl: string;
  fees: string;
  funding: string;
};

export type DailyPnlRow = {
  date: string;
  fill_count: number;
  gross_closed_pnl: string;
  fees: string;
  funding: string;
  net_pnl: string;
};

export type EquitySnapshot = {
  session_id: string;
  ts_ms: number;
  realized_pnl: string;
  unrealized_pnl: string;
  fees_cum: string;
  funding_cum: string;
  net_pnl: string;
};

type Props = {
  active?: boolean;
  sessionId?: string | null;
  unrealized?: string | null;
  onTip?: (msg: string) => void;
  onError?: (msg: string) => void;
  fmtNum: (v?: string | number | null, digits?: number) => string;
  pnlClass: (v?: string | null) => string;
};

export function PnlAnalytics({
  active = true,
  sessionId,
  unrealized,
  onTip,
  onError,
  fmtNum,
  pnlClass,
}: Props) {
  const { t } = useTranslation();
  const [summary, setSummary] = useState<SessionPnlSummary | null>(null);
  const [sessions, setSessions] = useState<SessionListItem[]>([]);
  const [daily, setDaily] = useState<DailyPnlRow[]>([]);
  const [equity, setEquity] = useState<EquitySnapshot[]>([]);
  const [selectedId, setSelectedId] = useState<string>("");
  const [curveScope, setCurveScope] = useState<"period" | "session">("period");
  const [curveDays, setCurveDays] = useState<7 | 30 | 0>(7); // 0 = all time
  const [curveMode, setCurveMode] = useState<"mark" | "closed">("mark");
  const [dailyDays, setDailyDays] = useState<7 | 30>(7);
  const [exporting, setExporting] = useState(false);
  const [clearing, setClearing] = useState(false);
  const [clearConfirm, setClearConfirm] = useState<"session" | "all" | null>(null);

  const activeId = selectedId || sessionId || "";

  const refresh = useCallback(async () => {
    try {
      const sid = selectedId || sessionId || undefined;
      const curveArgs =
        curveScope === "period"
          ? {
              session_id: null,
              all: true,
              days: curveDays === 0 ? null : curveDays,
              limit: 800,
            }
          : {
              session_id: sid ?? null,
              all: false,
              days: null,
              limit: 800,
            };
      const [sum, sess, day, curve] = await Promise.all([
        api<SessionPnlSummary | null>("get_session_pnl", {
          session_id: null,
          all: true,
        }),
        api<SessionListItem[]>("list_sessions", { limit: 40 }),
        api<DailyPnlRow[]>("get_daily_pnl", {
          session_id: null,
          days: dailyDays,
        }),
        api<EquitySnapshot[]>("list_equity_curve", curveArgs),
      ]);
      setSummary(sum);
      setSessions(sess || []);
      setDaily(day || []);
      setEquity(curve || []);
      if (!selectedId && sessionId) {
        setSelectedId(sessionId);
      } else if (!selectedId && sess?.length) {
        const active = sess.find((s) => s.active) || sess[0];
        if (active) setSelectedId(active.session_id);
      }
    } catch (e: any) {
      onError?.(String(e));
    }
  }, [selectedId, sessionId, dailyDays, curveScope, curveDays, onError]);

  useEffect(() => {
    if (!active) return;
    void refresh();
    const id = window.setInterval(() => void refresh(), 15_000);
    return () => window.clearInterval(id);
  }, [refresh, active]);

  useEffect(() => {
    if (sessionId && !selectedId) setSelectedId(sessionId);
  }, [sessionId, selectedId]);

  async function exportPack() {
    setExporting(true);
    try {
      const res = await api<{ path: string }>("export_analytics_pack", {
        session_id: activeId || null,
        dir: null,
      });
      onTip?.(t("app.analyticsExportDone", { path: res.path }));
    } catch (e: any) {
      onError?.(String(e));
    } finally {
      setExporting(false);
    }
  }

  async function doClear(scope: "session" | "all") {
    setClearing(true);
    setClearConfirm(null);
    try {
      const res = await api<{ cleared: number }>("clear_analytics", {
        session_id: scope === "session" ? activeId || null : null,
        all: scope === "all",
      });
      if (scope === "all") {
        setSelectedId(sessionId || "");
      }
      await refresh();
      onTip?.(
        t("app.analyticsClearDone", {
          count: res.cleared ?? 0,
        }),
      );
    } catch (e: any) {
      onError?.(String(e));
    } finally {
      setClearing(false);
    }
  }

  function shortId(id: string) {
    return id.length > 10 ? `${id.slice(0, 8)}…` : id;
  }

  return (
    <div className="pnl-analytics">
      <ConfirmDialog
        open={clearConfirm === "session"}
        title={t("app.analyticsClearSessionTitle")}
        message={t("app.analyticsClearSessionMsg", {
          id: shortId(activeId || "—"),
        })}
        cancelLabel={t("app.dialogCancel")}
        confirmLabel={t("app.analyticsClearConfirm")}
        danger
        onCancel={() => setClearConfirm(null)}
        onConfirm={() => void doClear("session")}
      />
      <ConfirmDialog
        open={clearConfirm === "all"}
        title={t("app.analyticsClearAllTitle")}
        message={t("app.analyticsClearAllMsg")}
        cancelLabel={t("app.dialogCancel")}
        confirmLabel={t("app.analyticsClearConfirm")}
        danger
        onCancel={() => setClearConfirm(null)}
        onConfirm={() => void doClear("all")}
      />
      <div className="pnl-analytics-head">
        <div>
          <h2 className="pnl-analytics-title">{t("app.analyticsTitle")}</h2>
          <p className="hint">{t("app.analyticsHint")}</p>
        </div>
        <div className="pnl-analytics-actions">
          <button type="button" className="btn-muted" onClick={() => void refresh()}>
            {t("app.analyticsRefresh")}
          </button>
          <button
            type="button"
            className="btn-info"
            disabled={exporting}
            onClick={() => void exportPack()}
          >
            {t("app.analyticsExport")}
          </button>
          <button
            type="button"
            className="btn-danger"
            disabled={clearing || !activeId}
            onClick={() => setClearConfirm("session")}
          >
            {t("app.analyticsClearSession")}
          </button>
          <button
            type="button"
            className="btn-danger"
            disabled={clearing}
            onClick={() => setClearConfirm("all")}
          >
            {t("app.analyticsClearAll")}
          </button>
        </div>
      </div>

      <div className="pnl-analytics-cards">
        <div className="pnl-card">
          <span className="stat-label">{t("app.analyticsGross")}</span>
          <span className={`stat-value ${pnlClass(summary?.gross_closed_pnl)}`}>
            {fmtNum(summary?.gross_closed_pnl, 4)}
          </span>
        </div>
        <div className="pnl-card">
          <span className="stat-label">{t("app.analyticsFees")}</span>
          <span className="stat-value pnl-neg">{fmtNum(summary?.fees, 4)}</span>
        </div>
        <div className="pnl-card">
          <span className="stat-label">{t("app.analyticsFunding")}</span>
          <span className={`stat-value ${pnlClass(summary?.funding)}`}>
            {fmtNum(summary?.funding, 4)}
          </span>
        </div>
        <div className="pnl-card pnl-card-emphasis">
          <span className="stat-label">{t("app.analyticsNetClosed")}</span>
          <span className={`stat-value ${pnlClass(summary?.net_pnl)}`}>
            {fmtNum(summary?.net_pnl, 4)}
          </span>
        </div>
      </div>

      <div className="pnl-analytics-meta">
        <span>
          {t("app.analyticsFills")}: {summary?.fill_count ?? 0}
        </span>
        <span title={t("app.analyticsUnrealizedHint")}>
          {t("app.analyticsUnrealized")}:{" "}
          <span className={pnlClass(unrealized)}>{fmtNum(unrealized, 4)}</span>
        </span>
        <span className="pnl-session-id" title={t("app.analyticsAllSessionsHint")}>
          {t("app.analyticsAllSessions")}
        </span>
      </div>

      <div className="pnl-curve-block">
        <div className="pnl-curve-toolbar">
          <span className="stat-label">{t("app.analyticsCurve")}</span>
          <div className="pnl-seg">
            <button
              type="button"
              className={curveScope === "period" ? "active" : ""}
              onClick={() => setCurveScope("period")}
            >
              {t("app.analyticsCurvePeriod")}
            </button>
            <button
              type="button"
              className={curveScope === "session" ? "active" : ""}
              onClick={() => setCurveScope("session")}
              disabled={!activeId}
            >
              {t("app.analyticsCurveSession")}
            </button>
          </div>
          {curveScope === "period" ? (
            <div className="pnl-seg">
              <button
                type="button"
                className={curveDays === 7 ? "active" : ""}
                onClick={() => setCurveDays(7)}
              >
                7d
              </button>
              <button
                type="button"
                className={curveDays === 30 ? "active" : ""}
                onClick={() => setCurveDays(30)}
              >
                30d
              </button>
              <button
                type="button"
                className={curveDays === 0 ? "active" : ""}
                onClick={() => setCurveDays(0)}
              >
                {t("app.analyticsCurveAllTime")}
              </button>
            </div>
          ) : (
            <span className="hint pnl-curve-session" title={activeId}>
              {t("app.analyticsSession")}: {shortId(activeId)}
            </span>
          )}
          <div className="pnl-seg">
            <button
              type="button"
              className={curveMode === "mark" ? "active" : ""}
              onClick={() => setCurveMode("mark")}
            >
              {t("app.analyticsCurveMark")}
            </button>
            <button
              type="button"
              className={curveMode === "closed" ? "active" : ""}
              onClick={() => setCurveMode("closed")}
            >
              {t("app.analyticsCurveClosed")}
            </button>
          </div>
        </div>
        <EquityCurve
          points={equity}
          mode={curveMode}
          emptyLabel={t("app.analyticsCurveEmpty")}
        />
      </div>

      <div className="pnl-split">
        <div className="pnl-panel">
          <div className="pnl-panel-head">
            <span className="stat-label">{t("app.analyticsSessions")}</span>
          </div>
          <div className="pnl-table-wrap">
            <table className="pnl-table">
              <thead>
                <tr>
                  <th>{t("app.analyticsColSession")}</th>
                  <th>{t("app.symbol")}</th>
                  <th>{t("app.analyticsNetClosed")}</th>
                  <th>{t("app.analyticsFills")}</th>
                  <th>{t("app.status")}</th>
                </tr>
              </thead>
              <tbody>
                {sessions.length === 0 ? (
                  <tr>
                    <td colSpan={5}>{t("app.analyticsNoSessions")}</td>
                  </tr>
                ) : (
                  sessions.map((s) => (
                    <tr
                      key={s.session_id}
                      className={
                        s.session_id === activeId ? "pnl-row-active" : undefined
                      }
                      onClick={() => {
                        setSelectedId(s.session_id);
                        setCurveScope("session");
                      }}
                    >
                      <td title={s.session_id}>
                        {shortId(s.session_id)}
                        {s.active ? " ●" : ""}
                      </td>
                      <td>{s.symbol}</td>
                      <td className={pnlClass(String(s.net_pnl))}>
                        {fmtNum(s.net_pnl, 4)}
                      </td>
                      <td>{s.fill_count}</td>
                      <td>{t(botStatusI18nKey(s.status))}</td>
                    </tr>
                  ))
                )}
              </tbody>
            </table>
          </div>
        </div>

        <div className="pnl-panel">
          <div className="pnl-panel-head">
            <span className="stat-label">{t("app.analyticsDaily")}</span>
            <div className="pnl-seg">
              <button
                type="button"
                className={dailyDays === 7 ? "active" : ""}
                onClick={() => setDailyDays(7)}
              >
                7d
              </button>
              <button
                type="button"
                className={dailyDays === 30 ? "active" : ""}
                onClick={() => setDailyDays(30)}
              >
                30d
              </button>
            </div>
          </div>
          <div className="pnl-table-wrap">
            <table className="pnl-table">
              <thead>
                <tr>
                  <th>{t("app.analyticsColDate")}</th>
                  <th>{t("app.analyticsNetClosed")}</th>
                  <th>{t("app.analyticsFees")}</th>
                  <th>{t("app.analyticsFunding")}</th>
                  <th>{t("app.analyticsFills")}</th>
                </tr>
              </thead>
              <tbody>
                {daily.length === 0 ? (
                  <tr>
                    <td colSpan={5}>{t("app.analyticsNoDaily")}</td>
                  </tr>
                ) : (
                  daily.map((d) => (
                    <tr key={d.date}>
                      <td>{d.date}</td>
                      <td className={pnlClass(String(d.net_pnl))}>
                        {fmtNum(d.net_pnl, 4)}
                      </td>
                      <td>{fmtNum(d.fees, 4)}</td>
                      <td className={pnlClass(String(d.funding))}>
                        {fmtNum(d.funding, 4)}
                      </td>
                      <td>{d.fill_count}</td>
                    </tr>
                  ))
                )}
              </tbody>
            </table>
          </div>
        </div>
      </div>
    </div>
  );
}

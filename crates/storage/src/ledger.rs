use anyhow::Result;
use chrono::{Local, TimeZone, Utc};
use rust_decimal::Decimal;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use crate::Storage;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FillLedgerRow {
    pub session_id: String,
    pub strategy_id: String,
    pub exchange_tid: Option<String>,
    pub exchange_oid: Option<String>,
    pub cloid: Option<String>,
    pub client_id: String,
    pub exchange_time_ms: Option<i64>,
    pub symbol: String,
    pub side: String,
    pub direction: Option<String>,
    pub price: Decimal,
    pub size: Decimal,
    pub notional: Decimal,
    pub crossed: bool,
    pub fee: Decimal,
    pub fee_token: Option<String>,
    pub gross_closed_pnl: Decimal,
    pub position_before: Option<Decimal>,
    pub position_after: Option<Decimal>,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FundingRow {
    pub session_id: String,
    pub strategy_id: String,
    pub symbol: String,
    pub exchange_time_ms: i64,
    pub usdc: Decimal,
    pub position_size: Option<Decimal>,
    pub funding_rate: Option<Decimal>,
    pub event_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquitySnapshotRow {
    pub session_id: String,
    pub strategy_id: String,
    pub ts_ms: i64,
    pub realized_pnl: Decimal,
    pub unrealized_pnl: Decimal,
    pub fees_cum: Decimal,
    pub funding_cum: Decimal,
    pub net_pnl: Decimal,
    pub position_base: Decimal,
    pub avg_entry: Option<Decimal>,
    pub mark: Option<Decimal>,
    pub liquidation_price: Option<Decimal>,
    pub account_equity: Option<Decimal>,
    pub margin_used: Option<Decimal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionPnlSummary {
    pub session_id: String,
    pub symbol: String,
    pub fill_count: i64,
    pub gross_closed_pnl: Decimal,
    pub fees: Decimal,
    pub funding: Decimal,
    pub net_pnl: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyPnlRow {
    pub date: String,
    pub fill_count: i64,
    pub gross_closed_pnl: Decimal,
    pub fees: Decimal,
    pub funding: Decimal,
    pub net_pnl: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionListItem {
    pub session_id: String,
    pub strategy_id: String,
    pub symbol: String,
    pub status: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub active: bool,
    pub fill_count: i64,
    pub net_pnl: Decimal,
    pub fees: Decimal,
    pub funding: Decimal,
}

impl Storage {
    /// Idempotent fill insert by exchange_tid when present.
    pub fn record_fill_ledger(&self, row: &FillLedgerRow) -> Result<bool> {
        if let Some(tid) = row.exchange_tid.as_ref().filter(|t| !t.is_empty()) {
            let exists: Option<i64> = self
                .db
                .query_row(
                    "SELECT id FROM fills WHERE exchange_tid = ?1 LIMIT 1",
                    params![tid],
                    |r| r.get(0),
                )
                .optional()?;
            if exists.is_some() {
                return Ok(false);
            }
        }
        let now = Utc::now().to_rfc3339();
        self.db.execute(
            r#"
            INSERT INTO fills (
              ts, symbol, side, price, size, pnl, client_id,
              session_id, strategy_id, exchange_tid, exchange_oid, cloid,
              exchange_time_ms, direction, notional, crossed, fee, fee_token,
              gross_closed_pnl, position_before, position_after, source
            ) VALUES (
              ?1,?2,?3,?4,?5,?6,?7,
              ?8,?9,?10,?11,?12,
              ?13,?14,?15,?16,?17,?18,
              ?19,?20,?21,?22
            )
            "#,
            params![
                now,
                row.symbol,
                row.side,
                row.price.to_string(),
                row.size.to_string(),
                (row.gross_closed_pnl - row.fee).to_string(),
                row.client_id,
                row.session_id,
                row.strategy_id,
                row.exchange_tid,
                row.exchange_oid,
                row.cloid,
                row.exchange_time_ms,
                row.direction,
                row.notional.to_string(),
                if row.crossed { 1 } else { 0 },
                row.fee.to_string(),
                row.fee_token,
                row.gross_closed_pnl.to_string(),
                row.position_before.map(|d| d.to_string()),
                row.position_after.map(|d| d.to_string()),
                row.source,
            ],
        )?;
        Ok(true)
    }

    pub fn record_funding(&self, row: &FundingRow) -> Result<bool> {
        let exists: Option<i64> = self
            .db
            .query_row(
                "SELECT id FROM funding_payments WHERE event_key = ?1 LIMIT 1",
                params![row.event_key],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_some() {
            return Ok(false);
        }
        self.db.execute(
            r#"
            INSERT INTO funding_payments (
              session_id, strategy_id, symbol, exchange_time_ms, usdc,
              position_size, funding_rate, event_key
            ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
            "#,
            params![
                row.session_id,
                row.strategy_id,
                row.symbol,
                row.exchange_time_ms,
                row.usdc.to_string(),
                row.position_size.map(|d| d.to_string()),
                row.funding_rate.map(|d| d.to_string()),
                row.event_key,
            ],
        )?;
        Ok(true)
    }

    pub fn record_equity_snapshot(&self, row: &EquitySnapshotRow) -> Result<()> {
        self.db.execute(
            r#"
            INSERT INTO equity_snapshots (
              session_id, strategy_id, ts_ms, realized_pnl, unrealized_pnl,
              fees_cum, funding_cum, net_pnl, position_base, avg_entry,
              mark, liquidation_price, account_equity, margin_used
            ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)
            "#,
            params![
                row.session_id,
                row.strategy_id,
                row.ts_ms,
                row.realized_pnl.to_string(),
                row.unrealized_pnl.to_string(),
                row.fees_cum.to_string(),
                row.funding_cum.to_string(),
                row.net_pnl.to_string(),
                row.position_base.to_string(),
                row.avg_entry.map(|d| d.to_string()),
                row.mark.map(|d| d.to_string()),
                row.liquidation_price.map(|d| d.to_string()),
                row.account_equity.map(|d| d.to_string()),
                row.margin_used.map(|d| d.to_string()),
            ],
        )?;
        Ok(())
    }

    pub fn session_fees_cum(&self, session_id: &str) -> Result<Decimal> {
        let fees: f64 = self
            .db
            .query_row(
                r#"
                SELECT COALESCE(SUM(CAST(fee AS REAL)),0) FROM fills WHERE session_id = ?1
                "#,
                params![session_id],
                |r| r.get(0),
            )
            .unwrap_or(0.0);
        Ok(parse_dec(fees.to_string()))
    }

    pub fn session_pnl_summary(&self, session_id: &str) -> Result<SessionPnlSummary> {
        let (symbol, fill_count, gross, fees): (String, i64, String, String) = self.db.query_row(
            r#"
            SELECT COALESCE(MAX(symbol),''), COUNT(*),
                   COALESCE(SUM(CAST(gross_closed_pnl AS REAL)),0),
                   COALESCE(SUM(CAST(fee AS REAL)),0)
            FROM fills WHERE session_id = ?1
            "#,
            params![session_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get::<_, f64>(2)?.to_string(),
                    r.get::<_, f64>(3)?.to_string(),
                ))
            },
        )?;
        let funding: String = self
            .db
            .query_row(
                r#"
                SELECT COALESCE(SUM(CAST(usdc AS REAL)),0) FROM funding_payments WHERE session_id = ?1
                "#,
                params![session_id],
                |r| Ok(r.get::<_, f64>(0)?.to_string()),
            )
            .unwrap_or_else(|_| "0".into());
        let g = gross.parse::<Decimal>().unwrap_or(Decimal::ZERO);
        let f = fees.parse::<Decimal>().unwrap_or(Decimal::ZERO);
        let fund = funding.parse::<Decimal>().unwrap_or(Decimal::ZERO);
        let symbol = if symbol.is_empty() {
            self.db
                .query_row(
                    "SELECT symbol FROM bot_sessions WHERE session_id = ?1 LIMIT 1",
                    params![session_id],
                    |r| r.get::<_, String>(0),
                )
                .unwrap_or_default()
        } else {
            symbol
        };
        Ok(SessionPnlSummary {
            session_id: session_id.to_string(),
            symbol,
            fill_count,
            gross_closed_pnl: g,
            fees: f,
            funding: fund,
            net_pnl: g - f + fund,
        })
    }

    /// Aggregate fills/funding across every session (top-level analytics totals).
    pub fn all_sessions_pnl_summary(&self) -> Result<SessionPnlSummary> {
        let (fill_count, gross, fees): (i64, String, String) = self.db.query_row(
            r#"
            SELECT COUNT(*),
                   COALESCE(SUM(CAST(gross_closed_pnl AS REAL)),0),
                   COALESCE(SUM(CAST(fee AS REAL)),0)
            FROM fills
            "#,
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get::<_, f64>(1)?.to_string(),
                    r.get::<_, f64>(2)?.to_string(),
                ))
            },
        )?;
        let funding: String = self
            .db
            .query_row(
                r#"
                SELECT COALESCE(SUM(CAST(usdc AS REAL)),0) FROM funding_payments
                "#,
                [],
                |r| Ok(r.get::<_, f64>(0)?.to_string()),
            )
            .unwrap_or_else(|_| "0".into());
        let g = gross.parse::<Decimal>().unwrap_or(Decimal::ZERO);
        let f = fees.parse::<Decimal>().unwrap_or(Decimal::ZERO);
        let fund = funding.parse::<Decimal>().unwrap_or(Decimal::ZERO);
        Ok(SessionPnlSummary {
            session_id: String::new(),
            symbol: String::new(),
            fill_count,
            gross_closed_pnl: g,
            fees: f,
            funding: fund,
            net_pnl: g - f + fund,
        })
    }

    pub fn list_session_summaries(&self, limit: usize) -> Result<Vec<SessionListItem>> {
        let sessions = self.list_bot_sessions(limit)?;
        let mut out = Vec::with_capacity(sessions.len());
        for s in sessions {
            let summary = self.session_pnl_summary(&s.session_id)?;
            out.push(SessionListItem {
                session_id: s.session_id,
                strategy_id: s.strategy_id,
                symbol: if summary.symbol.is_empty() {
                    s.symbol
                } else {
                    summary.symbol
                },
                status: s.status,
                created_at_ms: s.created_at_ms,
                updated_at_ms: s.updated_at_ms,
                active: s.active,
                fill_count: summary.fill_count,
                net_pnl: summary.net_pnl,
                fees: summary.fees,
                funding: summary.funding,
            });
        }
        Ok(out)
    }

    pub fn list_equity_snapshots(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<EquitySnapshotRow>> {
        self.list_equity_snapshots_range(session_id, None, limit, true)
    }

    /// Equity snapshots; `ascending` true returns oldest→newest for charts.
    pub fn list_equity_snapshots_range(
        &self,
        session_id: &str,
        since_ts_ms: Option<i64>,
        limit: usize,
        ascending: bool,
    ) -> Result<Vec<EquitySnapshotRow>> {
        let order = if ascending { "ASC" } else { "DESC" };
        let sql = format!(
            r#"
            SELECT session_id, strategy_id, ts_ms, realized_pnl, unrealized_pnl,
                   fees_cum, funding_cum, net_pnl, position_base, avg_entry,
                   mark, liquidation_price, account_equity, margin_used
            FROM equity_snapshots
            WHERE session_id = ?1 AND (?2 IS NULL OR ts_ms >= ?2)
            ORDER BY ts_ms {order}
            LIMIT ?3
            "#
        );
        let mut stmt = self.db.prepare(&sql)?;
        let since = since_ts_ms;
        let rows = stmt.query_map(params![session_id, since, limit as i64], |r| {
            Ok(EquitySnapshotRow {
                session_id: r.get(0)?,
                strategy_id: r.get(1)?,
                ts_ms: r.get(2)?,
                realized_pnl: parse_dec(r.get::<_, String>(3)?),
                unrealized_pnl: parse_dec(r.get::<_, String>(4)?),
                fees_cum: parse_dec(r.get::<_, String>(5)?),
                funding_cum: parse_dec(r.get::<_, String>(6)?),
                net_pnl: parse_dec(r.get::<_, String>(7)?),
                position_base: parse_dec(r.get::<_, String>(8)?),
                avg_entry: r.get::<_, Option<String>>(9)?.map(parse_dec),
                mark: r.get::<_, Option<String>>(10)?.map(parse_dec),
                liquidation_price: r.get::<_, Option<String>>(11)?.map(parse_dec),
                account_equity: r.get::<_, Option<String>>(12)?.map(parse_dec),
                margin_used: r.get::<_, Option<String>>(13)?.map(parse_dec),
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Combined equity curve across sessions in a time window.
    /// Stitches per-session cumulatives so the series is total portfolio PnL over time.
    pub fn list_equity_curve_all(
        &self,
        since_ts_ms: Option<i64>,
        limit: usize,
    ) -> Result<Vec<EquitySnapshotRow>> {
        let limit = limit.clamp(10, 5000);
        let fetch_cap = (limit as i64).saturating_mul(8).clamp(500, 20_000);
        let mut stmt = self.db.prepare(
            r#"
            SELECT session_id, strategy_id, ts_ms, realized_pnl, unrealized_pnl,
                   fees_cum, funding_cum, net_pnl, position_base, avg_entry,
                   mark, liquidation_price, account_equity, margin_used
            FROM equity_snapshots
            WHERE (?1 IS NULL OR ts_ms >= ?1)
            ORDER BY ts_ms ASC
            LIMIT ?2
            "#,
        )?;
        let rows = stmt.query_map(params![since_ts_ms, fetch_cap], |r| {
            Ok(EquitySnapshotRow {
                session_id: r.get(0)?,
                strategy_id: r.get(1)?,
                ts_ms: r.get(2)?,
                realized_pnl: parse_dec(r.get::<_, String>(3)?),
                unrealized_pnl: parse_dec(r.get::<_, String>(4)?),
                fees_cum: parse_dec(r.get::<_, String>(5)?),
                funding_cum: parse_dec(r.get::<_, String>(6)?),
                net_pnl: parse_dec(r.get::<_, String>(7)?),
                position_base: parse_dec(r.get::<_, String>(8)?),
                avg_entry: r.get::<_, Option<String>>(9)?.map(parse_dec),
                mark: r.get::<_, Option<String>>(10)?.map(parse_dec),
                liquidation_price: r.get::<_, Option<String>>(11)?.map(parse_dec),
                account_equity: r.get::<_, Option<String>>(12)?.map(parse_dec),
                margin_used: r.get::<_, Option<String>>(13)?.map(parse_dec),
            })
        })?;

        #[derive(Clone, Copy, Default)]
        struct Acc {
            realized: Decimal,
            unrealized: Decimal,
            fees: Decimal,
            funding: Decimal,
            net: Decimal,
            position: Decimal,
        }

        let mut by_session: std::collections::HashMap<String, Acc> =
            std::collections::HashMap::new();
        let mut combined: Vec<EquitySnapshotRow> = Vec::new();
        for row in rows {
            let row = row?;
            by_session.insert(
                row.session_id.clone(),
                Acc {
                    realized: row.realized_pnl,
                    unrealized: row.unrealized_pnl,
                    fees: row.fees_cum,
                    funding: row.funding_cum,
                    net: row.net_pnl,
                    position: row.position_base,
                },
            );
            let mut tot = Acc::default();
            for v in by_session.values() {
                tot.realized += v.realized;
                tot.unrealized += v.unrealized;
                tot.fees += v.fees;
                tot.funding += v.funding;
                tot.net += v.net;
                tot.position += v.position;
            }
            combined.push(EquitySnapshotRow {
                session_id: String::new(),
                strategy_id: "all".into(),
                ts_ms: row.ts_ms,
                realized_pnl: tot.realized,
                unrealized_pnl: tot.unrealized,
                fees_cum: tot.fees,
                funding_cum: tot.funding,
                net_pnl: tot.net,
                position_base: tot.position,
                avg_entry: None,
                mark: row.mark,
                liquidation_price: None,
                account_equity: row.account_equity,
                margin_used: None,
            });
        }

        if combined.len() <= limit {
            return Ok(combined);
        }
        // Even downsample keeping first/last.
        let mut out = Vec::with_capacity(limit);
        let last_i = combined.len() - 1;
        for i in 0..limit {
            let idx = if limit == 1 {
                last_i
            } else {
                i * last_i / (limit - 1)
            };
            if out.last().map(|p: &EquitySnapshotRow| p.ts_ms) == Some(combined[idx].ts_ms) {
                continue;
            }
            out.push(combined[idx].clone());
        }
        if out.last().map(|p| p.ts_ms) != Some(combined[last_i].ts_ms) {
            out.push(combined[last_i].clone());
        }
        Ok(out)
    }

    /// Aggregate net closed PnL by local calendar day (fills + funding).
    pub fn daily_pnl(
        &self,
        session_id: Option<&str>,
        days: u32,
    ) -> Result<Vec<DailyPnlRow>> {
        let days = days.clamp(1, 365);
        let mut fill_map: std::collections::BTreeMap<String, (i64, Decimal, Decimal)> =
            std::collections::BTreeMap::new();
        let mut funding_map: std::collections::BTreeMap<String, Decimal> =
            std::collections::BTreeMap::new();

        {
            let mut stmt = self.db.prepare(
                r#"
                SELECT exchange_time_ms, ts, CAST(gross_closed_pnl AS REAL), CAST(fee AS REAL), session_id
                FROM fills
                "#,
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, f64>(2)?,
                    r.get::<_, f64>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            })?;
            for row in rows {
                let (ex_ms, ts, gross, fee, sid) = row?;
                if let Some(filter) = session_id {
                    if sid.as_deref() != Some(filter) {
                        continue;
                    }
                } else if sid.as_ref().map(|s| s.is_empty()).unwrap_or(true) {
                    continue;
                }
                let date = local_date_key(ex_ms, &ts);
                let entry = fill_map
                    .entry(date)
                    .or_insert((0, Decimal::ZERO, Decimal::ZERO));
                entry.0 += 1;
                entry.1 += parse_dec(gross.to_string());
                entry.2 += parse_dec(fee.to_string());
            }
        }

        {
            let mut stmt = self.db.prepare(
                r#"
                SELECT exchange_time_ms, CAST(usdc AS REAL), session_id FROM funding_payments
                "#,
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, f64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (ex_ms, usdc, sid) = row?;
                if let Some(filter) = session_id {
                    if sid != filter {
                        continue;
                    }
                }
                let date = local_date_from_ms(ex_ms);
                *funding_map.entry(date).or_insert(Decimal::ZERO) += parse_dec(usdc.to_string());
            }
        }

        let mut dates: Vec<String> = fill_map
            .keys()
            .chain(funding_map.keys())
            .cloned()
            .collect();
        dates.sort();
        dates.dedup();
        if dates.len() > days as usize {
            dates = dates.split_off(dates.len() - days as usize);
        }

        let mut out = Vec::new();
        for date in dates {
            let (fill_count, gross, fees) = fill_map
                .get(&date)
                .cloned()
                .unwrap_or((0, Decimal::ZERO, Decimal::ZERO));
            let funding = funding_map.get(&date).copied().unwrap_or(Decimal::ZERO);
            out.push(DailyPnlRow {
                date,
                fill_count,
                gross_closed_pnl: gross,
                fees,
                funding,
                net_pnl: gross - fees + funding,
            });
        }
        Ok(out)
    }

    pub fn export_funding_csv(&self, path: &Path, session_id: Option<&str>) -> Result<usize> {
        let mut stmt = self.db.prepare(
            r#"
            SELECT session_id, symbol, exchange_time_ms, usdc, position_size, funding_rate, event_key
            FROM funding_payments ORDER BY exchange_time_ms
            "#,
        )?;
        let mapped = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, String>(6)?,
            ))
        })?;
        let mut csv = String::from(
            "session_id,symbol,exchange_time_ms,usdc,position_size,funding_rate,event_key\n",
        );
        let mut n = 0usize;
        for row in mapped {
            let (sid, symbol, ms, usdc, pos, rate, key) = row?;
            if let Some(filter) = session_id {
                if sid != filter {
                    continue;
                }
            }
            csv.push_str(&format!(
                "{},{},{},{},{},{},{}\n",
                sid,
                symbol,
                ms,
                usdc,
                pos.unwrap_or_default(),
                rate.unwrap_or_default(),
                key
            ));
            n += 1;
        }
        fs::write(path, csv)?;
        Ok(n)
    }

    pub fn export_equity_csv(&self, path: &Path, session_id: Option<&str>) -> Result<usize> {
        let mut stmt = self.db.prepare(
            r#"
            SELECT session_id, ts_ms, realized_pnl, unrealized_pnl, fees_cum, funding_cum, net_pnl,
                   position_base, avg_entry, mark
            FROM equity_snapshots ORDER BY ts_ms
            "#,
        )?;
        let mapped = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, Option<String>>(9)?,
            ))
        })?;
        let mut csv = String::from(
            "session_id,ts_ms,realized_pnl,unrealized_pnl,fees_cum,funding_cum,net_pnl,position_base,avg_entry,mark\n",
        );
        let mut n = 0usize;
        for row in mapped {
            let (sid, ts, real, unreal, fees, fund, net, pos, avg, mark) = row?;
            if let Some(filter) = session_id {
                if sid != filter {
                    continue;
                }
            }
            csv.push_str(&format!(
                "{},{},{},{},{},{},{},{},{},{}\n",
                sid,
                ts,
                real,
                unreal,
                fees,
                fund,
                net,
                pos,
                avg.unwrap_or_default(),
                mark.unwrap_or_default()
            ));
            n += 1;
        }
        fs::write(path, csv)?;
        Ok(n)
    }

    pub fn export_session_fills_csv(&self, path: &Path, session_id: Option<&str>) -> Result<usize> {
        let mut stmt = self.db.prepare(
            r#"
            SELECT ts, session_id, symbol, side, price, size, fee, gross_closed_pnl, pnl, client_id, exchange_tid
            FROM fills ORDER BY id
            "#,
        )?;
        let mapped = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, String>(9)?,
                r.get::<_, Option<String>>(10)?,
            ))
        })?;
        let mut csv = String::from(
            "ts,session_id,symbol,side,price,size,fee,gross_closed_pnl,pnl,client_id,exchange_tid\n",
        );
        let mut n = 0usize;
        for row in mapped {
            let (ts, sid, symbol, side, price, size, fee, gross, pnl, cid, tid) = row?;
            let sid_s = sid.unwrap_or_default();
            if let Some(filter) = session_id {
                if sid_s != filter {
                    continue;
                }
            } else if sid_s.is_empty() {
                continue;
            }
            csv.push_str(&format!(
                "{},{},{},{},{},{},{},{},{},{},{}\n",
                ts,
                sid_s,
                symbol,
                side,
                price,
                size,
                fee.unwrap_or_default(),
                gross.unwrap_or_default(),
                pnl,
                cid,
                tid.unwrap_or_default()
            ));
            n += 1;
        }
        fs::write(path, csv)?;
        Ok(n)
    }

    /// Write fills.csv, funding.csv, equity.csv, summary.json into `dir`.
    pub fn export_analytics_pack(
        &self,
        dir: &Path,
        session_id: Option<&str>,
    ) -> Result<serde_json::Value> {
        fs::create_dir_all(dir)?;
        let fills_n = self.export_session_fills_csv(&dir.join("fills.csv"), session_id)?;
        let funding_n = self.export_funding_csv(&dir.join("funding.csv"), session_id)?;
        let equity_n = self.export_equity_csv(&dir.join("equity.csv"), session_id)?;
        let summary = if let Some(sid) = session_id {
            serde_json::to_value(self.session_pnl_summary(sid)?)?
        } else {
            let sessions = self.list_session_summaries(200)?;
            serde_json::json!({
                "sessions": sessions,
                "fill_rows": fills_n,
                "funding_rows": funding_n,
                "equity_rows": equity_n,
            })
        };
        let pack = serde_json::json!({
            "exported_at": Utc::now().to_rfc3339(),
            "session_id": session_id,
            "fill_rows": fills_n,
            "funding_rows": funding_n,
            "equity_rows": equity_n,
            "summary": summary,
        });
        fs::write(
            dir.join("summary.json"),
            serde_json::to_string_pretty(&pack)?,
        )?;
        Ok(pack)
    }
}

fn local_date_from_ms(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "unknown".into())
}

fn local_date_key(exchange_time_ms: Option<i64>, ts: &str) -> String {
    if let Some(ms) = exchange_time_ms {
        return local_date_from_ms(ms);
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts) {
        return dt.with_timezone(&Local).format("%Y-%m-%d").to_string();
    }
    ts.chars().take(10).collect()
}

fn parse_dec(s: String) -> Decimal {
    s.parse().unwrap_or(Decimal::ZERO)
}

use anyhow::Result;
use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use serde_json::Value;

use crate::Storage;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BotSessionRow {
    pub session_id: String,
    pub strategy_id: String,
    pub symbol: String,
    pub status: String,
    pub config_json: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub active: bool,
}

impl Storage {
    pub fn upsert_bot_session(
        &self,
        session_id: &str,
        strategy_id: &str,
        symbol: &str,
        status: &str,
        config_json: &str,
        active: bool,
    ) -> Result<()> {
        let now = Utc::now().timestamp_millis();
        if active {
            self.db
                .execute("UPDATE bot_sessions SET active = 0 WHERE active = 1", [])?;
        }
        self.db.execute(
            r#"
            INSERT INTO bot_sessions (session_id, strategy_id, symbol, status, config_json, created_at_ms, updated_at_ms, active)
            VALUES (?1,?2,?3,?4,?5,?6,?6,?7)
            ON CONFLICT(session_id) DO UPDATE SET
              status=excluded.status,
              config_json=excluded.config_json,
              updated_at_ms=excluded.updated_at_ms,
              active=excluded.active
            "#,
            params![
                session_id,
                strategy_id,
                symbol,
                status,
                config_json,
                now,
                if active { 1 } else { 0 }
            ],
        )?;
        Ok(())
    }

    pub fn get_active_session(&self) -> Result<Option<BotSessionRow>> {
        let mut stmt = self.db.prepare(
            r#"
            SELECT session_id, strategy_id, symbol, status, config_json, created_at_ms, updated_at_ms, active
            FROM bot_sessions WHERE active = 1 ORDER BY updated_at_ms DESC LIMIT 1
            "#,
        )?;
        let row = stmt
            .query_row([], |r| {
                Ok(BotSessionRow {
                    session_id: r.get(0)?,
                    strategy_id: r.get(1)?,
                    symbol: r.get(2)?,
                    status: r.get(3)?,
                    config_json: r.get(4)?,
                    created_at_ms: r.get(5)?,
                    updated_at_ms: r.get(6)?,
                    active: r.get::<_, i64>(7)? != 0,
                })
            })
            .optional()?;
        Ok(row)
    }

    /// Mark session inactive. When `final_status` is set, also overwrite status
    /// (e.g. stop → `idle` so history does not keep showing `running`).
    pub fn deactivate_session(
        &self,
        session_id: &str,
        final_status: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().timestamp_millis();
        if let Some(status) = final_status {
            self.db.execute(
                "UPDATE bot_sessions SET active = 0, status = ?3, updated_at_ms = ?2 WHERE session_id = ?1",
                params![session_id, now, status],
            )?;
        } else {
            self.db.execute(
                "UPDATE bot_sessions SET active = 0, updated_at_ms = ?2 WHERE session_id = ?1",
                params![session_id, now],
            )?;
        }
        Ok(())
    }

    pub fn list_bot_sessions(&self, limit: usize) -> Result<Vec<BotSessionRow>> {
        let mut stmt = self.db.prepare(
            r#"
            SELECT session_id, strategy_id, symbol, status, config_json, created_at_ms, updated_at_ms, active
            FROM bot_sessions
            ORDER BY updated_at_ms DESC
            LIMIT ?1
            "#,
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok(BotSessionRow {
                session_id: r.get(0)?,
                strategy_id: r.get(1)?,
                symbol: r.get(2)?,
                status: r.get(3)?,
                config_json: r.get(4)?,
                created_at_ms: r.get(5)?,
                updated_at_ms: r.get(6)?,
                active: r.get::<_, i64>(7)? != 0,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Two-phase checkpoint: write intent/result payload for the active session.
    pub fn save_checkpoint(
        &self,
        session_id: &str,
        phase: &str,
        payload: &Value,
    ) -> Result<()> {
        let now = Utc::now().timestamp_millis();
        let text = serde_json::to_string(payload)?;
        // Deactivate previous active checkpoints, then insert the new one.
        self.db.execute(
            "UPDATE session_checkpoints SET is_active = 0 WHERE session_id = ?1 AND is_active = 1",
            params![session_id],
        )?;
        self.db.execute(
            r#"
            INSERT INTO session_checkpoints (session_id, phase, payload, created_at_ms, is_active)
            VALUES (?1,?2,?3,?4,1)
            "#,
            params![session_id, phase, text, now],
        )?;
        Ok(())
    }

    pub fn latest_checkpoint(&self, session_id: &str) -> Result<Option<(String, Value)>> {
        let mut stmt = self.db.prepare(
            r#"
            SELECT phase, payload FROM session_checkpoints
            WHERE session_id = ?1 AND is_active = 1
            ORDER BY id DESC LIMIT 1
            "#,
        )?;
        let row = stmt
            .query_row(params![session_id], |r| {
                let phase: String = r.get(0)?;
                let payload: String = r.get(1)?;
                Ok((phase, payload))
            })
            .optional()?;
        Ok(match row {
            Some((phase, payload)) => Some((phase, serde_json::from_str(&payload)?)),
            None => None,
        })
    }

    pub fn begin_recenter_op(
        &self,
        session_id: &str,
        generation: u32,
        intent_json: &str,
    ) -> Result<i64> {
        let now = Utc::now().timestamp_millis();
        self.db.execute(
            r#"
            INSERT INTO recenter_operations (session_id, generation, phase, intent_json, result_json, created_at_ms, updated_at_ms)
            VALUES (?1,?2,'intent',?3,NULL,?4,?4)
            "#,
            params![session_id, generation as i64, intent_json, now],
        )?;
        Ok(self.db.last_insert_rowid())
    }

    pub fn complete_recenter_op(&self, op_id: i64, phase: &str, result_json: &str) -> Result<()> {
        let now = Utc::now().timestamp_millis();
        self.db.execute(
            r#"
            UPDATE recenter_operations
            SET phase = ?2, result_json = ?3, updated_at_ms = ?4
            WHERE id = ?1
            "#,
            params![op_id, phase, result_json, now],
        )?;
        Ok(())
    }

    pub fn incomplete_recenter_op(
        &self,
        session_id: &str,
    ) -> Result<Option<(i64, u32, String, String)>> {
        let mut stmt = self.db.prepare(
            r#"
            SELECT id, generation, phase, intent_json FROM recenter_operations
            WHERE session_id = ?1 AND phase NOT IN ('committed','failed','aborted')
            ORDER BY id DESC LIMIT 1
            "#,
        )?;
        let row = stmt
            .query_row(params![session_id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)? as u32,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .optional()?;
        Ok(row)
    }
}

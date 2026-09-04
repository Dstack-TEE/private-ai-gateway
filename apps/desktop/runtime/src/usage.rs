use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

use rusqlite::{params, params_from_iter, types::Value as SqlValue, Connection, Row};
use serde::{Deserialize, Serialize};

use crate::contracts::{RequestActivity, UsageSummary};

const DEFAULT_PAGE_SIZE: usize = 20;
const MAX_PAGE_SIZE: usize = 100;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageQuery {
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub since: Option<u64>,
    #[serde(default)]
    pub until: Option<u64>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsagePoint {
    pub day: String,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub tokens: u64,
    pub cost_usd: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsagePage {
    pub items: Vec<RequestActivity>,
    pub next_cursor: Option<String>,
    pub summary: UsageSummary,
    pub series: Vec<UsagePoint>,
    pub agents: Vec<String>,
    pub models: Vec<String>,
}

pub struct UsageStore {
    connection: Mutex<Connection>,
}

impl UsageStore {
    pub fn open(path: PathBuf) -> Result<Self, String> {
        secure_parent(&path)?;
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err("Refusing to open a symlink as the usage database".to_string());
            }
            Ok(_) => secure_file(&path)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                desktop_gateway::tokens::write_private(&path, "")
                    .map_err(|error| format!("Cannot create the usage database: {error}"))?;
            }
            Err(error) => return Err(format!("Cannot inspect the usage database: {error}")),
        }
        let connection = Connection::open(&path)
            .map_err(|error| format!("Cannot open the usage database: {error}"))?;
        initialize(&connection)?;
        secure_file(&path)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn memory() -> Result<Self, String> {
        let connection = Connection::open_in_memory()
            .map_err(|error| format!("Cannot open the fallback usage database: {error}"))?;
        initialize(&connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn upsert(&self, item: &RequestActivity) -> Result<(), String> {
        if item.path == "/v1/models" {
            return Ok(());
        }
        self.lock()?
            .execute(
                "INSERT INTO usage_records (
                   id, session_id, at, agent, model, method, path, status, streamed,
                   receipt_id, verified, detail, locally_constrained, rewritten,
                   left_device, input_tokens, output_tokens, cache_read_tokens,
                   cache_write_tokens, cost_usd, updated_at
                 ) VALUES (
                   ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                   ?14, ?15, ?16, ?17, ?18, ?19, ?20, unixepoch()
                 )
                 ON CONFLICT(id) DO UPDATE SET
                   session_id = excluded.session_id,
                   at = min(usage_records.at, excluded.at),
                   agent = coalesce(excluded.agent, usage_records.agent),
                   model = coalesce(excluded.model, usage_records.model),
                   method = excluded.method,
                   path = excluded.path,
                   status = excluded.status,
                   streamed = max(usage_records.streamed, excluded.streamed),
                   receipt_id = coalesce(excluded.receipt_id, usage_records.receipt_id),
                   verified = coalesce(excluded.verified, usage_records.verified),
                   detail = CASE WHEN excluded.detail = '' THEN usage_records.detail ELSE excluded.detail END,
                   locally_constrained = coalesce(excluded.locally_constrained, usage_records.locally_constrained),
                   rewritten = coalesce(excluded.rewritten, usage_records.rewritten),
                   left_device = max(usage_records.left_device, excluded.left_device),
                   input_tokens = coalesce(excluded.input_tokens, usage_records.input_tokens),
                   output_tokens = coalesce(excluded.output_tokens, usage_records.output_tokens),
                   cache_read_tokens = coalesce(excluded.cache_read_tokens, usage_records.cache_read_tokens),
                   cache_write_tokens = coalesce(excluded.cache_write_tokens, usage_records.cache_write_tokens),
                   cost_usd = coalesce(excluded.cost_usd, usage_records.cost_usd),
                   updated_at = unixepoch()",
                params![
                    item.id,
                    item.session_id,
                    item.at,
                    item.agent,
                    item.model,
                    item.method,
                    item.path,
                    item.status,
                    item.streamed,
                    item.receipt_id,
                    item.verified,
                    item.detail,
                    item.locally_constrained,
                    item.rewritten,
                    item.left_device,
                    item.input_tokens,
                    item.output_tokens,
                    item.cache_read_tokens,
                    item.cache_write_tokens,
                    item.cost_usd,
                ],
            )
            .map_err(|error| format!("Cannot save usage: {error}"))?;
        Ok(())
    }

    pub fn page(&self, query: &UsageQuery) -> Result<UsagePage, String> {
        let (where_sql, bindings) = filters(query, true)?;
        let (summary_where, summary_bindings) = filters(query, false)?;
        let limit = query
            .limit
            .unwrap_or(DEFAULT_PAGE_SIZE)
            .clamp(1, MAX_PAGE_SIZE);
        let connection = self.lock()?;
        let mut item_bindings = bindings;
        item_bindings.push(SqlValue::Integer((limit + 1) as i64));
        let mut statement = connection
            .prepare(&format!(
                "SELECT {} FROM usage_records {where_sql} ORDER BY at DESC, id DESC LIMIT ?",
                columns()
            ))
            .map_err(db_error)?;
        let rows = statement
            .query_map(params_from_iter(item_bindings.iter()), row_to_activity)
            .map_err(db_error)?;
        let mut items = rows.collect::<Result<Vec<_>, _>>().map_err(db_error)?;
        let has_more = items.len() > limit;
        items.truncate(limit);
        let next_cursor = has_more
            .then(|| items.last().map(|item| format!("{}:{}", item.at, item.id)))
            .flatten();

        let summary = summary(&connection, &summary_where, &summary_bindings)?;
        let series = series(&connection, &summary_where, &summary_bindings)?;
        let agents = facet(&connection, "agent")?;
        let models = facet(&connection, "model")?;
        Ok(UsagePage {
            items,
            next_cursor,
            summary,
            series,
            agents,
            models,
        })
    }

    pub fn session_summary(&self, session_id: &str) -> Result<UsageSummary, String> {
        let query = UsageQuery {
            session_id: Some(session_id.to_string()),
            ..UsageQuery::default()
        };
        let (where_sql, bindings) = filters(&query, false)?;
        let connection = self.lock()?;
        summary(&connection, &where_sql, &bindings)
    }

    pub fn export_csv(&self, query: &UsageQuery, path: &Path) -> Result<usize, String> {
        if path.as_os_str().is_empty() {
            return Err("Choose a destination for the CSV export".to_string());
        }
        let mut all = query.clone();
        all.cursor = None;
        let (where_sql, bindings) = filters(&all, false)?;
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(&format!(
                "SELECT {} FROM usage_records {where_sql} ORDER BY at DESC, id DESC",
                columns()
            ))
            .map_err(db_error)?;
        let rows = statement
            .query_map(params_from_iter(bindings.iter()), row_to_activity)
            .map_err(db_error)?;
        let records = rows.collect::<Result<Vec<_>, _>>().map_err(db_error)?;
        let mut csv = String::from("timestamp,id,session_id,agent,model,method,path,status,streamed,left_device,receipt_id,verified,input_tokens,output_tokens,cache_read_tokens,cache_write_tokens,cost_usd,detail\n");
        for item in &records {
            let fields = [
                item.at.to_string(),
                item.id.clone(),
                item.session_id.clone(),
                item.agent.clone().unwrap_or_default(),
                item.model.clone().unwrap_or_default(),
                item.method.clone(),
                item.path.clone(),
                item.status.to_string(),
                item.streamed.to_string(),
                item.left_device.to_string(),
                item.receipt_id.clone().unwrap_or_default(),
                item.verified
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                optional_number(item.input_tokens),
                optional_number(item.output_tokens),
                optional_number(item.cache_read_tokens),
                optional_number(item.cache_write_tokens),
                item.cost_usd
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                item.detail.clone(),
            ];
            csv.push_str(
                &fields
                    .iter()
                    .map(|field| csv_field(field))
                    .collect::<Vec<_>>()
                    .join(","),
            );
            csv.push('\n');
        }
        fs::write(path, csv).map_err(|error| format!("Cannot write the usage export: {error}"))?;
        Ok(records.len())
    }

    pub fn clear(&self) -> Result<u64, String> {
        let changed = self
            .lock()?
            .execute("DELETE FROM usage_records", [])
            .map_err(db_error)?;
        Ok(changed as u64)
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, String> {
        self.connection
            .lock()
            .map_err(|_| "The usage database is unavailable".to_string())
    }
}

fn initialize(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS usage_records (
               id TEXT PRIMARY KEY NOT NULL,
               session_id TEXT NOT NULL,
               at INTEGER NOT NULL,
               agent TEXT,
               model TEXT,
               method TEXT NOT NULL,
               path TEXT NOT NULL,
               status INTEGER NOT NULL,
               streamed INTEGER NOT NULL,
               receipt_id TEXT,
               verified INTEGER,
               detail TEXT NOT NULL,
               locally_constrained INTEGER,
               rewritten INTEGER,
               left_device INTEGER NOT NULL,
               input_tokens INTEGER,
               output_tokens INTEGER,
               cache_read_tokens INTEGER,
               cache_write_tokens INTEGER,
               cost_usd REAL,
               updated_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS usage_records_at ON usage_records(at DESC, id DESC);
             CREATE INDEX IF NOT EXISTS usage_records_agent ON usage_records(agent, at DESC);
             CREATE INDEX IF NOT EXISTS usage_records_model ON usage_records(model, at DESC);
             CREATE INDEX IF NOT EXISTS usage_records_session ON usage_records(session_id, at DESC);
             DELETE FROM usage_records WHERE path = '/v1/models';",
        )
        .map_err(|error| format!("Cannot initialize the usage database: {error}"))
}

fn filters(query: &UsageQuery, include_cursor: bool) -> Result<(String, Vec<SqlValue>), String> {
    let mut clauses = vec!["path != '/v1/models'".to_string()];
    let mut values = Vec::new();
    if let Some(agent) = clean_filter(query.agent.as_deref(), "agent")? {
        clauses.push("agent = ?".to_string());
        values.push(SqlValue::Text(agent));
    }
    if let Some(model) = clean_filter(query.model.as_deref(), "model")? {
        clauses.push("model = ?".to_string());
        values.push(SqlValue::Text(model));
    }
    if let Some(session_id) = clean_filter(query.session_id.as_deref(), "session")? {
        clauses.push("session_id = ?".to_string());
        values.push(SqlValue::Text(session_id));
    }
    if let Some(since) = query.since {
        clauses.push("at >= ?".to_string());
        values.push(SqlValue::Integer(to_i64(since)?));
    }
    if let Some(until) = query.until {
        clauses.push("at < ?".to_string());
        values.push(SqlValue::Integer(to_i64(until)?));
    }
    if include_cursor {
        if let Some(cursor) = query.cursor.as_deref() {
            let (at, id) = parse_cursor(cursor)?;
            clauses.push("(at < ? OR (at = ? AND id < ?))".to_string());
            values.push(SqlValue::Integer(to_i64(at)?));
            values.push(SqlValue::Integer(to_i64(at)?));
            values.push(SqlValue::Text(id));
        }
    }
    Ok((
        if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        },
        values,
    ))
}

fn clean_filter(value: Option<&str>, name: &str) -> Result<Option<String>, String> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.len() > 200 || value.chars().any(char::is_control) {
        return Err(format!("Invalid usage {name} filter"));
    }
    Ok(Some(value.to_string()))
}

fn parse_cursor(value: &str) -> Result<(u64, String), String> {
    let (at, id) = value
        .split_once(':')
        .ok_or_else(|| "Invalid usage cursor".to_string())?;
    let at = at
        .parse::<u64>()
        .map_err(|_| "Invalid usage cursor".to_string())?;
    if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
        return Err("Invalid usage cursor".to_string());
    }
    Ok((at, id.to_string()))
}

fn summary(
    connection: &Connection,
    where_sql: &str,
    bindings: &[SqlValue],
) -> Result<UsageSummary, String> {
    connection
        .query_row(
            &format!(
                "SELECT count(*), coalesce(sum(input_tokens), 0), coalesce(sum(output_tokens), 0),
                        coalesce(sum(cache_read_tokens), 0), coalesce(sum(cache_write_tokens), 0),
                        coalesce(sum(cost_usd), 0),
                        coalesce(sum(CASE WHEN verified = 1 THEN 1 ELSE 0 END), 0),
                        coalesce(sum(CASE WHEN left_device = 0 THEN 1 ELSE 0 END), 0),
                        coalesce(sum(CASE WHEN verified = 0 THEN 1 ELSE 0 END), 0)
                 FROM usage_records {where_sql}"
            ),
            params_from_iter(bindings.iter()),
            |row| {
                Ok(UsageSummary {
                    requests: row.get(0)?,
                    input_tokens: row.get(1)?,
                    output_tokens: row.get(2)?,
                    cache_read_tokens: row.get(3)?,
                    cache_write_tokens: row.get(4)?,
                    cost_usd: row.get(5)?,
                    protected: row.get(6)?,
                    blocked_locally: row.get(7)?,
                    failed_proof: row.get(8)?,
                })
            },
        )
        .map_err(db_error)
}

fn series(
    connection: &Connection,
    where_sql: &str,
    bindings: &[SqlValue],
) -> Result<Vec<UsagePoint>, String> {
    let mut statement = connection
        .prepare(&format!(
            "SELECT strftime('%Y-%m-%d', at, 'unixepoch', 'localtime') AS day, count(*),
                    coalesce(sum(input_tokens), 0),
                    coalesce(sum(output_tokens), 0),
                    coalesce(sum(coalesce(input_tokens, 0) + coalesce(output_tokens, 0)), 0),
                    coalesce(sum(cost_usd), 0)
             FROM usage_records {where_sql} GROUP BY day ORDER BY day ASC"
        ))
        .map_err(db_error)?;
    let rows = statement
        .query_map(params_from_iter(bindings.iter()), |row| {
            Ok(UsagePoint {
                day: row.get(0)?,
                requests: row.get(1)?,
                input_tokens: row.get(2)?,
                output_tokens: row.get(3)?,
                tokens: row.get(4)?,
                cost_usd: row.get(5)?,
            })
        })
        .map_err(db_error)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
}

fn facet(connection: &Connection, column: &str) -> Result<Vec<String>, String> {
    let mut statement = connection
        .prepare(&format!(
            "SELECT DISTINCT {column} FROM usage_records \
             WHERE path != '/v1/models' AND {column} IS NOT NULL AND {column} != '' \
             ORDER BY {column}"
        ))
        .map_err(db_error)?;
    let rows = statement
        .query_map([], |row| row.get(0))
        .map_err(db_error)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
}

fn columns() -> &'static str {
    "id, session_id, method, path, model, status, streamed, receipt_id, verified,
     detail, at, agent, locally_constrained, rewritten, left_device, input_tokens,
     output_tokens, cache_read_tokens, cache_write_tokens, cost_usd"
}

fn row_to_activity(row: &Row<'_>) -> rusqlite::Result<RequestActivity> {
    Ok(RequestActivity {
        id: row.get(0)?,
        session_id: row.get(1)?,
        method: row.get(2)?,
        path: row.get(3)?,
        model: row.get(4)?,
        status: row.get(5)?,
        streamed: row.get(6)?,
        receipt_id: row.get(7)?,
        verified: row.get(8)?,
        detail: row.get(9)?,
        at: row.get(10)?,
        agent: row.get(11)?,
        locally_constrained: row.get(12)?,
        rewritten: row.get(13)?,
        left_device: row.get(14)?,
        input_tokens: row.get(15)?,
        output_tokens: row.get(16)?,
        cache_read_tokens: row.get(17)?,
        cache_write_tokens: row.get(18)?,
        cost_usd: row.get(19)?,
    })
}

fn optional_number(value: Option<u64>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

fn csv_field(value: &str) -> String {
    let protected = if value.starts_with(['=', '+', '-', '@']) {
        format!("'{value}")
    } else {
        value.to_string()
    };
    if protected.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", protected.replace('"', "\"\""))
    } else {
        protected
    }
}

fn to_i64(value: u64) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| "Usage timestamp is out of range".to_string())
}

fn db_error(error: rusqlite::Error) -> String {
    format!("Cannot read usage: {error}")
}

fn secure_parent(path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "The usage database path has no parent".to_string())?;
    desktop_gateway::tokens::create_private_dir(parent)
        .map_err(|error| format!("Cannot create the app data directory: {error}"))
}

#[cfg(unix)]
fn secure_file(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("Cannot secure the usage database: {error}"))
}

#[cfg(not(unix))]
fn secure_file(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, at: u64, agent: &str, model: &str) -> RequestActivity {
        RequestActivity {
            id: id.to_string(),
            session_id: "session-1".to_string(),
            method: "POST".to_string(),
            path: "/v1/responses".to_string(),
            model: Some(model.to_string()),
            status: 200,
            streamed: true,
            receipt_id: Some(format!("receipt-{id}")),
            verified: Some(true),
            detail: "receipt verified".to_string(),
            at,
            agent: Some(agent.to_string()),
            locally_constrained: Some(true),
            rewritten: Some(false),
            left_device: true,
            input_tokens: Some(100),
            output_tokens: Some(25),
            cache_read_tokens: Some(10),
            cache_write_tokens: None,
            cost_usd: Some(0.0125),
        }
    }

    #[test]
    fn persists_filters_paginates_updates_and_clears() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite3");
        let store = UsageStore::open(path.clone()).unwrap();
        store.upsert(&item("a1", 10, "codex", "model-a")).unwrap();
        store.upsert(&item("b2", 20, "pi", "model-b")).unwrap();
        let mut discovery = item("catalog", 30, "codex", "catalog-only");
        discovery.path = "/v1/models".to_string();
        store.upsert(&discovery).unwrap();
        store
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO usage_records (
                   id, session_id, at, agent, model, method, path, status, streamed,
                   receipt_id, verified, detail, locally_constrained, rewritten,
                   left_device, input_tokens, output_tokens, cache_read_tokens,
                   cache_write_tokens, cost_usd, updated_at
                 ) SELECT
                   'legacy-catalog', session_id, at, agent, 'legacy-catalog-only', method,
                   '/v1/models', status, streamed, receipt_id, verified, detail,
                   locally_constrained, rewritten, left_device, input_tokens, output_tokens,
                   cache_read_tokens, cache_write_tokens, cost_usd, updated_at
                 FROM usage_records WHERE id = 'a1'",
                [],
            )
            .unwrap();
        let first = store
            .page(&UsageQuery {
                limit: Some(1),
                ..UsageQuery::default()
            })
            .unwrap();
        assert_eq!(first.items[0].id, "b2");
        assert!(first.next_cursor.is_some());
        assert_eq!(first.summary.requests, 2);
        assert_eq!(first.summary.input_tokens, 200);
        assert!(!first.models.iter().any(|model| model == "catalog-only"));
        let second = store
            .page(&UsageQuery {
                cursor: first.next_cursor,
                limit: Some(1),
                ..UsageQuery::default()
            })
            .unwrap();
        assert_eq!(second.items[0].id, "a1");
        let pi = store
            .page(&UsageQuery {
                agent: Some("pi".into()),
                ..UsageQuery::default()
            })
            .unwrap();
        assert_eq!(pi.items.len(), 1);
        drop(store);
        let reopened = UsageStore::open(path).unwrap();
        let legacy_count: u64 = reopened
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM usage_records WHERE path = '/v1/models'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(legacy_count, 0);
        assert_eq!(
            reopened
                .page(&UsageQuery::default())
                .unwrap()
                .summary
                .requests,
            2
        );
        assert_eq!(reopened.clear().unwrap(), 2);
    }

    #[test]
    fn session_summary_is_complete_beyond_the_activity_preview_limit() {
        let store = UsageStore::memory().unwrap();
        for index in 0..75 {
            let mut record = item(&format!("record-{index}"), index, "codex", "model-a");
            record.session_id = "long-session".to_string();
            store.upsert(&record).unwrap();
        }
        let summary = store.session_summary("long-session").unwrap();
        assert_eq!(summary.requests, 75);
        assert_eq!(summary.input_tokens, 7_500);
        assert_eq!(summary.output_tokens, 1_875);
    }

    #[test]
    fn legacy_ids_are_valid_cursor_tiebreakers() {
        let store = UsageStore::memory().unwrap();
        store
            .upsert(&item("tag:legacy/agent@example", 10, "hermes", "model-a"))
            .unwrap();
        store
            .upsert(&item("older", 9, "hermes", "model-a"))
            .unwrap();
        let first = store
            .page(&UsageQuery {
                limit: Some(1),
                ..UsageQuery::default()
            })
            .unwrap();
        assert_eq!(first.items[0].id, "tag:legacy/agent@example");
        let second = store
            .page(&UsageQuery {
                cursor: first.next_cursor,
                limit: Some(1),
                ..UsageQuery::default()
            })
            .unwrap();
        assert_eq!(second.items[0].id, "older");
    }

    #[test]
    fn export_columns_align_and_filters_are_validated() {
        let dir = tempfile::tempdir().unwrap();
        let store = UsageStore::open(dir.path().join("usage.sqlite3")).unwrap();
        store.upsert(&item("a1", 10, "codex", "model-a")).unwrap();
        let output = dir.path().join("usage.csv");
        assert_eq!(
            store.export_csv(&UsageQuery::default(), &output).unwrap(),
            1
        );
        let csv = fs::read_to_string(output).unwrap();
        let mut lines = csv.lines();
        assert_eq!(lines.next().unwrap().split(',').count(), 18);
        assert_eq!(lines.next().unwrap().split(',').count(), 18);
        assert!(store
            .page(&UsageQuery {
                cursor: Some("not-a-cursor".to_string()),
                ..UsageQuery::default()
            })
            .unwrap_err()
            .contains("Invalid usage cursor"));

        let mut unsafe_record = item("formula", 11, "codex", "=MODEL()");
        unsafe_record.detail = "+SUM(1,1)".to_string();
        store.upsert(&unsafe_record).unwrap();
        let output = dir.path().join("safe-usage.csv");
        store.export_csv(&UsageQuery::default(), &output).unwrap();
        let csv = fs::read_to_string(output).unwrap();
        assert!(csv.contains("'=MODEL()"));
        assert!(csv.contains("\"'+SUM(1,1)\""));
    }

    #[test]
    fn memory_store_supports_the_same_queries() {
        let store = UsageStore::memory().unwrap();
        store.upsert(&item("a1", 10, "hermes", "model-a")).unwrap();
        assert_eq!(
            store.page(&UsageQuery::default()).unwrap().summary.requests,
            1
        );
    }
}

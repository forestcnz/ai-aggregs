use rusqlite::{params, Connection};
use serde::Serialize;

use crate::config::types::{
    ApiKeyEntry, Config, ConsumerConfig, LogConfig, ModelMapping, Protocol, ProviderConfig,
};

pub fn open(path: &str) -> anyhow::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    Ok(conn)
}

pub fn init_tables(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS settings (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS providers (
            id               INTEGER PRIMARY KEY AUTOINCREMENT,
            name             TEXT NOT NULL UNIQUE,
            protocol         TEXT NOT NULL DEFAULT 'chat',
            base_url         TEXT NOT NULL DEFAULT '',
            timeout_secs     INTEGER NOT NULL DEFAULT 3000,
            enabled          INTEGER NOT NULL DEFAULT 1,
            reasoning_effort TEXT,
            sort_order       INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS provider_keys (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            provider_id INTEGER NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
            key         TEXT NOT NULL,
            enabled     INTEGER NOT NULL DEFAULT 1,
            sort_order  INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS provider_models (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            provider_id INTEGER NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
            model       TEXT NOT NULL,
            sort_order  INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS usage_logs (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            consumer_key    TEXT    NOT NULL DEFAULT '',
            model           TEXT    NOT NULL,
            input_tokens    INTEGER NOT NULL DEFAULT 0,
            output_tokens   INTEGER NOT NULL DEFAULT 0,
            total_tokens    INTEGER NOT NULL DEFAULT 0,
            created_at      INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_usage_key     ON usage_logs(consumer_key);
        CREATE INDEX IF NOT EXISTS idx_usage_created  ON usage_logs(created_at);

        CREATE TABLE IF NOT EXISTS provider_usage_logs (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            provider_id     INTEGER NOT NULL DEFAULT 0,
            provider_key    TEXT    NOT NULL DEFAULT '',
            model           TEXT    NOT NULL,
            input_tokens    INTEGER NOT NULL DEFAULT 0,
            output_tokens   INTEGER NOT NULL DEFAULT 0,
            total_tokens    INTEGER NOT NULL DEFAULT 0,
            created_at      INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_pusage_name   ON provider_usage_logs(provider_id);
        CREATE INDEX IF NOT EXISTS idx_pusage_key     ON provider_usage_logs(provider_id, provider_key);
        CREATE INDEX IF NOT EXISTS idx_pusage_created ON provider_usage_logs(created_at);
        "#,
    )?;

    // v2 流式工程化：provider 表加 4 个可选配置列。
    // ALTER TABLE ADD COLUMN 不支持 IF NOT EXISTS，需先检查列是否存在（幂等）。
    migrate_add_column(conn, "providers", "stream_keepalive_interval_secs", "INTEGER")?;
    migrate_add_column(conn, "providers", "stream_first_output_timeout_secs", "INTEGER")?;
    migrate_add_column(conn, "providers", "stream_interval_timeout_secs", "INTEGER")?;
    migrate_add_column(conn, "providers", "detect_infinite_whitespace", "INTEGER")?;

    Ok(())
}

/// 幂等添加 SQLite 表的列：若列已存在则跳过。
fn migrate_add_column(
    conn: &Connection,
    table: &str,
    column: &str,
    sql_type: &str,
) -> Result<(), rusqlite::Error> {
    let cols: Vec<String> = conn
        .prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |r| r.get::<_, String>(1))?
        .filter_map(Result::ok)
        .collect();
    if cols.iter().any(|c| c == column) {
        return Ok(());
    }
    let sql = format!("ALTER TABLE {table} ADD COLUMN {column} {sql_type}");
    tracing::debug!(sql = %sql, "db migration: add column");
    conn.execute(&sql, [])?;
    Ok(())
}

pub fn get_setting(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
        r.get(0)
    })
    .ok()
}

pub fn load_config(conn: &Connection) -> anyhow::Result<Config> {
    let listen = get_setting(conn, "listen").unwrap_or_else(|| "127.0.0.1:8849".into());
    let key_blacklist_secs: u64 = get_setting(conn, "key_blacklist_secs")
        .and_then(|v| v.parse().ok())
        .unwrap_or(600);
    let log_level = get_setting(conn, "log_level").unwrap_or_else(|| "info".into());
    let auto_start_gateway = get_setting(conn, "auto_start_gateway")
        .map(|v| v == "1")
        .unwrap_or(false);

    let consumer_api_keys: Vec<String> = get_setting(conn, "consumer_api_keys")
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default();

    let model_mappings: Vec<ModelMapping> = get_setting(conn, "model_mappings")
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default();

    let mut providers = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT id, name, protocol, base_url, timeout_secs, enabled, reasoning_effort,
                stream_keepalive_interval_secs, stream_first_output_timeout_secs,
                stream_interval_timeout_secs, detect_infinite_whitespace
         FROM providers ORDER BY sort_order",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, u64>(4)?,
            row.get::<_, i64>(5)? != 0,
            row.get::<_, Option<String>>(6)?,
            row.get::<_, Option<i64>>(7)?,
            row.get::<_, Option<i64>>(8)?,
            row.get::<_, Option<i64>>(9)?,
            row.get::<_, Option<i64>>(10)?,
        ))
    })?;

    for row_result in rows {
        let (
            pid,
            name,
            protocol,
            base_url,
            timeout_secs,
            enabled,
            reasoning_effort,
            keepalive,
            first_output,
            interval,
            detect_ws,
        ) = row_result?;

        let mut api_keys = Vec::new();
        {
            let mut ks = conn.prepare(
                "SELECT key, enabled FROM provider_keys WHERE provider_id = ?1 ORDER BY sort_order",
            )?;
            let key_rows = ks.query_map([pid], |r| {
                Ok(ApiKeyEntry::Object {
                    key: r.get(0)?,
                    enabled: r.get::<_, i64>(1)? != 0,
                })
            })?;
            for kr in key_rows {
                api_keys.push(kr?);
            }
        }

        let mut models = Vec::new();
        {
            let mut ms = conn.prepare(
                "SELECT model FROM provider_models WHERE provider_id = ?1 ORDER BY sort_order",
            )?;
            let let_models = ms.query_map([pid], |r| r.get::<_, String>(0))?;
            for mr in let_models {
                models.push(mr?);
            }
        }

        providers.push(ProviderConfig {
            id: pid,
            name,
            protocol: Protocol::from_str(&protocol),
            base_url,
            api_keys,
            models,
            timeout_secs,
            enabled,
            reasoning_effort,
            // v2 流式工程化：DB 中存储为 INTEGER（0 表示禁用），运行时转为 Option<u64>
            stream_keepalive_interval_secs: keepalive.map(|v| v.max(0) as u64),
            stream_first_output_timeout_secs: first_output.map(|v| v.max(0) as u64),
            stream_interval_timeout_secs: interval.map(|v| v.max(0) as u64),
            // detect_infinite_whitespace：0 = false, 非 0 = true, NULL = None（默认 true）
            detect_infinite_whitespace: detect_ws.map(|v| v != 0),
        });
    }

    Ok(Config {
        listen,
        providers,
        consumer: ConsumerConfig {
            api_keys: consumer_api_keys,
            models: vec![],
        },
        log: LogConfig { level: log_level },
        key_blacklist_secs,
        auto_start_gateway,
        model_mappings,
    })
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = ?2",
        params![key, value],
    )?;
    Ok(())
}

pub fn save_config(conn: &Connection, cfg: &Config) -> anyhow::Result<()> {
    let tx = conn.unchecked_transaction()?;

    set_setting(&tx, "listen", &cfg.listen)?;
    set_setting(
        &tx,
        "key_blacklist_secs",
        &cfg.key_blacklist_secs.to_string(),
    )?;
    set_setting(
        &tx,
        "consumer_api_keys",
        &serde_json::to_string(&cfg.consumer.api_keys)?,
    )?;
    set_setting(&tx, "log_level", &cfg.log.level)?;
    set_setting(
        &tx,
        "auto_start_gateway",
        if cfg.auto_start_gateway { "1" } else { "0" },
    )?;
    set_setting(
        &tx,
        "model_mappings",
        &serde_json::to_string(&cfg.model_mappings)?,
    )?;

    // upsert 每个 provider：id>0 走 UPDATE（保留原 ID），id=0 走 INSERT（新建）
    let mut seen_ids: Vec<i64> = Vec::new();
    for (i, p) in cfg.providers.iter().enumerate() {
        // 流式工程化字段：Option<u64> → Option<i64>（0 表示禁用）
        let keepalive: Option<i64> = p.stream_keepalive_interval_secs.map(|v| v as i64);
        let first_output: Option<i64> = p.stream_first_output_timeout_secs.map(|v| v as i64);
        let interval: Option<i64> = p.stream_interval_timeout_secs.map(|v| v as i64);
        let detect_ws: Option<i64> = p.detect_infinite_whitespace.map(|v| v as i64);
        let pid = if p.id > 0 {
            tx.execute(
                "UPDATE providers SET name=?2, protocol=?3, base_url=?4, timeout_secs=?5,
                 enabled=?6, reasoning_effort=?7, sort_order=?8,
                 stream_keepalive_interval_secs=?9, stream_first_output_timeout_secs=?10,
                 stream_interval_timeout_secs=?11, detect_infinite_whitespace=?12
                 WHERE id=?1",
                params![
                    p.id,
                    p.name,
                    p.protocol.as_str(),
                    p.base_url,
                    p.timeout_secs,
                    p.enabled as i64,
                    p.reasoning_effort,
                    i as i64,
                    keepalive,
                    first_output,
                    interval,
                    detect_ws,
                ],
            )?;
            p.id
        } else {
            tx.execute(
                "INSERT INTO providers
                    (name, protocol, base_url, timeout_secs, enabled, reasoning_effort, sort_order,
                     stream_keepalive_interval_secs, stream_first_output_timeout_secs,
                     stream_interval_timeout_secs, detect_infinite_whitespace)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    p.name,
                    p.protocol.as_str(),
                    p.base_url,
                    p.timeout_secs,
                    p.enabled as i64,
                    p.reasoning_effort,
                    i as i64,
                    keepalive,
                    first_output,
                    interval,
                    detect_ws,
                ],
            )?;
            tx.last_insert_rowid()
        };
        seen_ids.push(pid);

        // 清理该 provider 的 key/model 后重新插入
        tx.execute("DELETE FROM provider_keys WHERE provider_id = ?1", [pid])?;
        tx.execute("DELETE FROM provider_models WHERE provider_id = ?1", [pid])?;

        for (ki, entry) in p.api_keys.iter().enumerate() {
            let (key, enabled) = match entry {
                ApiKeyEntry::Object { key, enabled } => (key.as_str(), *enabled),
                ApiKeyEntry::Plain(k) => (k.as_str(), true),
            };
            tx.execute(
                "INSERT INTO provider_keys (provider_id, key, enabled, sort_order) VALUES (?1, ?2, ?3, ?4)",
                params![pid, key, enabled as i64, ki as i64],
            )?;
        }

        for (mi, m) in p.models.iter().enumerate() {
            tx.execute(
                "INSERT INTO provider_models (provider_id, model, sort_order) VALUES (?1, ?2, ?3)",
                params![pid, m, mi as i64],
            )?;
        }
    }

    // 删除配置中已不存在的 providers（级联删除 key/model）
    if seen_ids.is_empty() {
        tx.execute("DELETE FROM providers", [])?;
    } else {
        let placeholders = seen_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!("DELETE FROM providers WHERE id NOT IN ({placeholders})");
        let params: Vec<&dyn rusqlite::ToSql> = seen_ids
            .iter()
            .map(|id| id as &dyn rusqlite::ToSql)
            .collect();
        tx.execute(&sql, params.as_slice())?;
    }

    tx.commit()?;
    Ok(())
}

// ===================== 用量统计 =====================

/// 单个模型的聚合用量行
#[derive(Debug, Serialize)]
pub struct UsageRow {
    pub model: String,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

/// 记录一次请求的 token 用量
pub fn record_usage(
    conn: &Connection,
    consumer_key: &str,
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
) -> anyhow::Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    conn.execute(
        "INSERT INTO usage_logs (consumer_key, model, input_tokens, output_tokens, total_tokens, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![consumer_key, model, input_tokens, output_tokens, total_tokens, now],
    )?;
    Ok(())
}

/// 按模型聚合查询用量。
/// consumer_keys 为空 = 不过滤；调用方应从当前 config 解析出实际 key 列表传入。
pub fn query_usage(
    conn: &Connection,
    consumer_keys: &[String],
    since: i64,
) -> anyhow::Result<Vec<UsageRow>> {
    fn map_row(r: &rusqlite::Row) -> rusqlite::Result<UsageRow> {
        Ok(UsageRow {
            model: r.get(0)?,
            requests: r.get::<_, i64>(1)? as u64,
            input_tokens: r.get::<_, i64>(2)? as u64,
            output_tokens: r.get::<_, i64>(3)? as u64,
            total_tokens: r.get::<_, i64>(4)? as u64,
        })
    }

    if consumer_keys.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders: Vec<String> = (0..consumer_keys.len())
        .map(|i| format!("?{}", i + 2))
        .collect();
    let sql = format!(
        "SELECT model, COUNT(*),
                COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0), COALESCE(SUM(total_tokens),0)
         FROM usage_logs WHERE created_at >= ?1 AND consumer_key IN ({})
         GROUP BY model ORDER BY SUM(total_tokens) DESC",
        placeholders.join(",")
    );
    let mut stmt = conn.prepare(&sql)?;
    let binds: Vec<&dyn rusqlite::ToSql> = std::iter::once(&since as &dyn rusqlite::ToSql)
        .chain(consumer_keys.iter().map(|k| k as &dyn rusqlite::ToSql))
        .collect();
    let iter = stmt.query_map(binds.as_slice(), map_row)?;
    Ok(iter.collect::<rusqlite::Result<Vec<_>>>()?)
}

// ===================== 供应商用量统计 =====================

/// 记录一次请求的供应商侧 token 用量（独立表）
pub fn record_provider_usage(
    conn: &Connection,
    provider_id: i64,
    provider_key: &str,
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
) -> anyhow::Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    conn.execute(
        "INSERT INTO provider_usage_logs (provider_id, provider_key, model, input_tokens, output_tokens, total_tokens, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![provider_id, provider_key, model, input_tokens, output_tokens, total_tokens, now],
    )?;
    Ok(())
}

/// 按模型聚合查询供应商用量。
/// provider_ids 为空 = 不过滤供应商；provider_keys 为空 = 不过滤 key。
/// 调用方应从当前 config 解析出实际的 id/key 列表传入。
pub fn query_provider_usage(
    conn: &Connection,
    provider_ids: &[i64],
    provider_keys: &[String],
    since: i64,
) -> anyhow::Result<Vec<UsageRow>> {
    fn map_row(r: &rusqlite::Row) -> rusqlite::Result<UsageRow> {
        Ok(UsageRow {
            model: r.get(0)?,
            requests: r.get::<_, i64>(1)? as u64,
            input_tokens: r.get::<_, i64>(2)? as u64,
            output_tokens: r.get::<_, i64>(3)? as u64,
            total_tokens: r.get::<_, i64>(4)? as u64,
        })
    }

    // 动态构建 WHERE 条件
    let mut conditions = vec!["created_at >= ?1".to_string()];
    let mut binds: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(since)];
    let mut idx = 2usize;

    if !provider_ids.is_empty() {
        let placeholders: Vec<String> = provider_ids
            .iter()
            .map(|id| {
                let p = format!("?{idx}");
                binds.push(Box::new(*id));
                idx += 1;
                p
            })
            .collect();
        conditions.push(format!("provider_id IN ({})", placeholders.join(",")));
    }

    if !provider_keys.is_empty() {
        let placeholders: Vec<String> = provider_keys
            .iter()
            .map(|k| {
                let p = format!("?{idx}");
                binds.push(Box::new(k.clone()));
                idx += 1;
                p
            })
            .collect();
        conditions.push(format!("provider_key IN ({})", placeholders.join(",")));
    }

    let sql = format!(
        "SELECT model, COUNT(*),
                COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0), COALESCE(SUM(total_tokens),0)
         FROM provider_usage_logs WHERE {}
         GROUP BY model ORDER BY SUM(total_tokens) DESC",
        conditions.join(" AND ")
    );

    let refs: Vec<&dyn rusqlite::ToSql> = binds.iter().map(|b| b.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;
    let iter = stmt.query_map(refs.as_slice(), map_row)?;
    Ok(iter.collect::<rusqlite::Result<Vec<_>>>()?)
}

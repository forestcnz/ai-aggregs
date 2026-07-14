use std::collections::HashMap;

use rusqlite::{params, Connection};

use crate::config::types::{
    ApiKeyEntry, Config, ConsumerConfig, LogConfig, Protocol, ProviderConfig,
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
            timeout_secs     INTEGER NOT NULL DEFAULT 300,
            max_retries      INTEGER NOT NULL DEFAULT 2,
            enabled          INTEGER NOT NULL DEFAULT 1,
            reasoning_effort TEXT,
            extra_headers    TEXT NOT NULL DEFAULT '{}',
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
        "#,
    )?;
    Ok(())
}

fn get_setting(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
        r.get(0)
    })
    .ok()
}

pub fn load_config(conn: &Connection) -> anyhow::Result<Config> {
    let listen = get_setting(conn, "listen").unwrap_or_else(|| "127.0.0.1:8000".into());
    let key_blacklist_secs: u64 = get_setting(conn, "key_blacklist_secs")
        .and_then(|v| v.parse().ok())
        .unwrap_or(600);
    let log_level = get_setting(conn, "log_level").unwrap_or_else(|| "info".into());

    let consumer_api_keys: Vec<String> = get_setting(conn, "consumer_api_keys")
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default();

    let mut providers = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT id, name, protocol, base_url, timeout_secs, max_retries, enabled, reasoning_effort, extra_headers
         FROM providers ORDER BY sort_order",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, u64>(4)?,
            row.get::<_, u32>(5)?,
            row.get::<_, i64>(6)? != 0,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, String>(8)?,
        ))
    })?;

    for row_result in rows {
        let (
            pid,
            name,
            protocol,
            base_url,
            timeout_secs,
            max_retries,
            enabled,
            reasoning_effort,
            extra_headers_json,
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
            let model_rows = ms.query_map([pid], |r| r.get::<_, String>(0))?;
            for mr in model_rows {
                models.push(mr?);
            }
        }

        let extra_headers: HashMap<String, String> =
            serde_json::from_str(&extra_headers_json).unwrap_or_default();

        providers.push(ProviderConfig {
            name,
            protocol: Protocol::from_str(&protocol),
            base_url,
            api_keys,
            models,
            timeout_secs,
            max_retries,
            extra_headers,
            enabled,
            reasoning_effort,
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
    })
}

fn set_setting(conn: &Connection, key: &str, value: &str) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = ?2",
        params![key, value],
    )?;
    Ok(())
}

pub fn save_config(conn: &Connection, cfg: &Config) -> anyhow::Result<()> {
    let tx = conn.unchecked_transaction()?;

    tx.execute("DELETE FROM provider_keys", [])?;
    tx.execute("DELETE FROM provider_models", [])?;
    tx.execute("DELETE FROM providers", [])?;

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

    for (i, p) in cfg.providers.iter().enumerate() {
        tx.execute(
            "INSERT INTO providers
                (name, protocol, base_url, timeout_secs, max_retries, enabled, reasoning_effort, extra_headers, sort_order)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                p.name,
                p.protocol.as_str(),
                p.base_url,
                p.timeout_secs,
                p.max_retries,
                p.enabled as i64,
                p.reasoning_effort,
                serde_json::to_string(&p.extra_headers)?,
                i as i64,
            ],
        )?;
        let pid = tx.last_insert_rowid();

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

    tx.commit()?;
    Ok(())
}

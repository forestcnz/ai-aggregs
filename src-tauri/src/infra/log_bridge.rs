use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

use log4rs::append::rolling_file::policy::compound::roll::fixed_window::FixedWindowRollerBuilder;
use log4rs::append::rolling_file::policy::compound::trigger::size::SizeTrigger;
use log4rs::append::rolling_file::policy::compound::trigger::time::{
    TimeTrigger, TimeTriggerConfig, TimeTriggerInterval,
};
use log4rs::append::rolling_file::policy::compound::trigger::Trigger;
use log4rs::append::rolling_file::policy::compound::CompoundPolicy;
use log4rs::append::rolling_file::RollingFileAppender;
use log4rs::config::{Appender, Config, Root};
use log4rs::encode::pattern::PatternEncoder;

#[derive(Debug)]
struct CompositeTrigger {
    time: TimeTrigger,
    size: SizeTrigger,
}

impl Trigger for CompositeTrigger {
    fn trigger(&self, file: &log4rs::append::rolling_file::LogFile<'_>) -> anyhow::Result<bool> {
        let t = self.time.trigger(file)?;
        let s = self.size.trigger(file)?;
        Ok(t || s)
    }

    fn is_pre_process(&self) -> bool {
        false
    }
}

fn init_log4rs(log_dir: &std::path::Path) -> anyhow::Result<()> {
    let current_log = log_dir.join("ai-aggregs.log");
    let archive_pattern = log_dir
        .join("ai-aggregs.{}.log.gz")
        .to_string_lossy()
        .to_string();

    let time_config = TimeTriggerConfig {
        interval: TimeTriggerInterval::Day(1),
        modulate: false,
        max_random_delay: 0,
    };
    let time_trigger = TimeTrigger::new(time_config);

    let size_trigger = SizeTrigger::new(10 * 1024 * 1024);

    let composite = CompositeTrigger {
        time: time_trigger,
        size: size_trigger,
    };

    let roller = FixedWindowRollerBuilder::default().build(&archive_pattern, 100)?;

    let policy = CompoundPolicy::new(Box::new(composite), Box::new(roller));

    let encoder = PatternEncoder::new("{d(%Y-%m-%d %H:%M:%S%.3f)} {l} {t} - {m}{n}");

    let appender = RollingFileAppender::builder()
        .encoder(Box::new(encoder))
        .append(true)
        .build(current_log, Box::new(policy))?;

    let config = Config::builder()
        .appender(Appender::builder().build("file", Box::new(appender)))
        .build(
            Root::builder()
                .appender("file")
                .build(log::LevelFilter::Trace),
        )?;

    log4rs::init_config(config)?;
    Ok(())
}

struct Log4rsBridgeLayer;

impl<S> Layer<S> for Log4rsBridgeLayer
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let log_level = match *event.metadata().level() {
            tracing::Level::ERROR => log::Level::Error,
            tracing::Level::WARN => log::Level::Warn,
            tracing::Level::INFO => log::Level::Info,
            tracing::Level::DEBUG => log::Level::Debug,
            tracing::Level::TRACE => log::Level::Trace,
        };

        let target = event.metadata().target();
        let mut visitor = MessageCollector::default();
        event.record(&mut visitor);
        let msg = visitor.result;
        let args = format_args!("{}", msg);
        let record = log::Record::builder()
            .level(log_level)
            .target(target)
            .args(args)
            .file(event.metadata().file())
            .line(event.metadata().line())
            .module_path(event.metadata().module_path())
            .build();

        log::logger().log(&record);
    }
}

#[derive(Clone, Serialize)]
pub struct LogEntry {
    pub level: String,
    pub target: String,
    pub message: String,
    pub ts: u64,
}

pub type AppHandleSlot = Arc<Mutex<Option<AppHandle>>>;

pub struct LogLevelSetter {
    inner: Box<dyn Fn(&str) + Send + Sync>,
}

impl LogLevelSetter {
    pub fn set(&self, level: &str) {
        (self.inner)(level);
    }
}

pub fn create_slot() -> AppHandleSlot {
    Arc::new(Mutex::new(None))
}

pub fn set_app_handle(slot: &AppHandleSlot, app: AppHandle) {
    *slot.lock().unwrap() = Some(app);
}

pub fn purge_old_logs(log_dir: &std::path::Path, max_days: u64, max_total_bytes: u64) {
    let cutoff =
        std::time::SystemTime::now() - std::time::Duration::from_secs(max_days * 24 * 60 * 60);

    let mut files: Vec<(std::path::PathBuf, std::time::SystemTime, u64)> = Vec::new();

    if let Ok(entries) = std::fs::read_dir(log_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if let Ok(meta) = entry.metadata() {
                let modified = meta.modified().unwrap_or(std::time::SystemTime::now());
                let size = meta.len();
                files.push((path, modified, size));
            }
        }
    }

    let mut remaining: Vec<(std::path::PathBuf, std::time::SystemTime, u64)> = Vec::new();
    let mut total_size: u64 = 0;
    for (path, modified, size) in files {
        if modified < cutoff {
            let _ = std::fs::remove_file(&path);
        } else {
            total_size += size;
            remaining.push((path, modified, size));
        }
    }

    if total_size > max_total_bytes {
        remaining.sort_by_key(|(_, modified, _)| *modified);
        for (path, _, size) in &remaining {
            if total_size <= max_total_bytes {
                break;
            }
            let _ = std::fs::remove_file(path);
            total_size -= *size;
        }
    }
}

pub fn install(level: &str, slot: AppHandleSlot, log_dir: std::path::PathBuf) -> LogLevelSetter {
    if let Err(e) = init_log4rs(&log_dir) {
        eprintln!("[log_bridge] log4rs 初始化失败: {e}");
    }

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));
    let (reloadable_filter, handle) = tracing_subscriber::reload::Layer::new(filter);

    let tauri_layer = TauriLogLayer { slot };
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_timer(tracing_subscriber::fmt::time::ChronoLocal::default());
    let bridge_layer = Log4rsBridgeLayer;

    let _ = tracing_subscriber::registry()
        .with(tauri_layer)
        .with(fmt_layer)
        .with(bridge_layer)
        .with(reloadable_filter)
        .try_init();

    let setter = Box::new(move |new_level: &str| {
        let new_filter =
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(new_level));
        if let Err(e) = handle.reload(new_filter) {
            eprintln!("[log_bridge] 热更新日志级别失败: {e}");
        } else {
            eprintln!("[log_bridge] 日志级别已更新为 {new_level}");
        }
    });

    LogLevelSetter { inner: setter }
}

struct TauriLogLayer {
    slot: AppHandleSlot,
}

impl<S> Layer<S> for TauriLogLayer
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let target = event.metadata().target();
        if !target.starts_with("ai_aggregs_lib") {
            return;
        }

        let mut visitor = MessageCollector::default();
        event.record(&mut visitor);

        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let entry = LogEntry {
            level: event.metadata().level().to_string(),
            target: target.to_string(),
            message: visitor.result,
            ts,
        };

        if let Some(app) = self.slot.lock().unwrap().as_ref() {
            let _ = app.emit("gateway-log", entry);
        }
    }
}

#[derive(Default)]
struct MessageCollector {
    result: String,
}

impl MessageCollector {
    fn add_field(&mut self, name: &str, value: &str) {
        if !self.result.is_empty() {
            self.result.push(' ');
        }
        self.result.push_str(name);
        self.result.push_str("=");
        self.result.push_str(value);
    }
}

impl Visit for MessageCollector {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            if !self.result.is_empty() {
                self.result.push(' ');
            }
            self.result.push_str(value);
        } else {
            self.add_field(field.name(), value);
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            if !self.result.is_empty() {
                self.result.push(' ');
            }
            self.result.push_str(&format!("{:?}", value));
        } else {
            self.add_field(field.name(), &format!("{:?}", value));
        }
    }
}

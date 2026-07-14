//! 日志桥接：把 tracing 日志事件转发到前端（通过 Tauri event "gateway-log"）
//! 同时通过 log4rs 实现文件日志的按天+按大小双滚动、gzip 归档压缩、保留天数和总大小上限。
//!
//! 使用 AppHandleSlot 模式：subscriber 在 run() 初始化时注册（此时还没有 AppHandle），
//! setup hook 中再注入 AppHandle。
//!
//! 日志级别可通过动态过滤层运行时更新，无需重启应用。

use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

// ===================== log4rs 相关 =====================

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

/// 复合触发器：时间 + 大小，任一条件满足即滚动
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

/// 初始化 log4rs 文件日志（按天+按大小双滚动，gzip 归档）
fn init_log4rs(log_dir: &std::path::Path) -> anyhow::Result<()> {
    let current_log = log_dir.join("ai-aggregs.log");
    let archive_pattern = log_dir
        .join("ai-aggregs.{}.log.gz")
        .to_string_lossy()
        .to_string();

    // 时间触发器：每天滚动一次
    let time_config = TimeTriggerConfig {
        interval: TimeTriggerInterval::Day(1),
        modulate: false,
        max_random_delay: 0,
    };
    let time_trigger = TimeTrigger::new(time_config);

    // 大小触发器：单文件超过 10MB 时滚动
    let size_trigger = SizeTrigger::new(10 * 1024 * 1024);

    let composite = CompositeTrigger {
        time: time_trigger,
        size: size_trigger,
    };

    // 固定窗口滚动器：最多保留 100 个归档文件，.gz 扩展名自动 gzip 压缩
    let roller = FixedWindowRollerBuilder::default().build(&archive_pattern, 100)?;

    let policy = CompoundPolicy::new(Box::new(composite), Box::new(roller));

    // 日志格式：2026-07-14 12:30:45.123 INFO ai_aggregs_lib::handler - 消息内容
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

// ===================== tracing → log4rs 桥接层 =====================

/// 把 tracing 事件转发到 log4rs（通过 log 门面）
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

// ===================== 前端事件层 =====================

/// 一条日志（推送到前端）
#[derive(Clone, Serialize)]
pub struct LogEntry {
    pub level: String,
    pub target: String,
    pub message: String,
    /// 事件发生时的 UNIX 时间戳（毫秒）
    pub ts: u64,
}

/// 共享的 AppHandle 持有者（setup 时注入）
pub type AppHandleSlot = Arc<Mutex<Option<AppHandle>>>;

// ===================== 日志级别设置器 =====================

/// 日志级别设置器（类型擦除，避免 reload::Handle 的复杂泛型）
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

/// 注入 AppHandle（在 Tauri setup hook 中调用）
pub fn set_app_handle(slot: &AppHandleSlot, app: AppHandle) {
    *slot.lock().unwrap() = Some(app);
}

// ===================== 旧日志清理 =====================

/// 清理日志目录：删除超过 max_days 天的文件，且总大小不超过 max_total_bytes
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

    // 第一步：删除超过 max_days 的文件
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

    // 第二步：如果总大小超限，按最老优先删除
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

// ===================== 安装 =====================

/// 安装 tracing subscriber + log4rs，返回日志级别设置器
///
/// - 终端输出：tracing-subscriber fmt 层（本地时间）
/// - 文件输出：log4rs RollingFileAppender（按天+按大小双滚动，gzip 归档）
/// - 前端转发：TauriLogLayer（Tauri event "gateway-log"）
/// - 级别控制：EnvFilter reload（运行时热更新）
pub fn install(level: &str, slot: AppHandleSlot, log_dir: std::path::PathBuf) -> LogLevelSetter {
    // 先初始化 log4rs（设置 log 门面全局 logger）
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

// ===================== Tauri 前端转发层 =====================

/// 自定义 tracing Layer：把事件转发到 Tauri 前端
struct TauriLogLayer {
    slot: AppHandleSlot,
}

impl<S> Layer<S> for TauriLogLayer
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let target = event.metadata().target();
        // 只转发网关自身产生的日志（过滤掉第三方库噪音）
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

// ===================== 消息收集器 =====================

/// 收集 tracing 事件中的字段，拼成可读消息
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

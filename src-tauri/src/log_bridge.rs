//! 日志桥接：把 tracing 日志事件转发到前端（通过 Tauri event "gateway-log"）
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

/// 安装 tracing subscriber，返回日志级别设置器
pub fn install(level: &str, slot: AppHandleSlot) -> LogLevelSetter {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(level));
    let (reloadable_filter, handle) = tracing_subscriber::reload::Layer::new(filter);

    let tauri_layer = TauriLogLayer { slot };
    let fmt_layer = tracing_subscriber::fmt::layer();

    let _ = tracing_subscriber::registry()
        .with(tauri_layer)
        .with(fmt_layer)
        .with(reloadable_filter)
        .try_init();

    let setter = Box::new(move |new_level: &str| {
        let new_filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(new_level));
        if let Err(e) = handle.reload(new_filter) {
            // 此时 tracing 可能还未完全初始化，用 eprintln 兜底
            eprintln!("[log_bridge] 热更新日志级别失败: {e}");
        } else {
            // 通过最新的 handle 发一条 info 日志（此时新级别已生效）
            let _ = handle.reload(EnvFilter::new(new_level));
            eprintln!("[log_bridge] 日志级别已更新为 {new_level}");
        }
    });

    LogLevelSetter { inner: setter }
}

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

        // 记录事件发生时的 UNIX 时间戳（毫秒）
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
            // 消息本体直接写入（不加 message= 前缀）
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

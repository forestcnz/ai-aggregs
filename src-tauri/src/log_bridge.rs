//! 日志桥接：把 tracing 日志事件转发到前端（通过 Tauri event "gateway-log"）
//!
//! 使用 AppHandleSlot 模式：subscriber 在 run() 初始化时注册（此时还没有 AppHandle），
//! setup hook 中再注入 AppHandle。

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
}

/// 共享的 AppHandle 持有者（setup 时注入）
pub type AppHandleSlot = Arc<Mutex<Option<AppHandle>>>;

pub fn create_slot() -> AppHandleSlot {
    Arc::new(Mutex::new(None))
}

/// 注入 AppHandle（在 Tauri setup hook 中调用）
pub fn set_app_handle(slot: &AppHandleSlot, app: AppHandle) {
    *slot.lock().unwrap() = Some(app);
}

/// 安装 tracing subscriber：控制台 fmt 层 + Tauri event 层
pub fn install(level: &str, slot: AppHandleSlot) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));

    let tauri_layer = TauriLogLayer { slot }
        .with_filter(filter.clone());

    let _ = tracing_subscriber::registry()
        .with(tauri_layer)
        .with(tracing_subscriber::fmt::layer().with_filter(filter))
        .try_init();
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

        let entry = LogEntry {
            level: event.metadata().level().to_string(),
            target: target.to_string(),
            message: visitor.result,
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
        self.result.push('=');
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

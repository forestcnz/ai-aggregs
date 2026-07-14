use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager};

use crate::config::state::{AppCtrl, TrayItems};
use crate::gateway::manager::{start_gateway_inner, stop_gateway_inner};

pub fn update_tray(app: &tauri::AppHandle, running: bool) {
    if let Some(ts) = app.try_state::<TrayItems>() {
        let _ = ts.status.set_text(if running {
            "状态: 运行中"
        } else {
            "状态: 已停止"
        });
        let _ = ts.toggle_gw.set_text(if running {
            "停止网关"
        } else {
            "启动网关"
        });
    }
    if let Some(tray) = app.tray_by_id("main-tray") {
        let tip = if running {
            "AI 聚合网关 - 运行中"
        } else {
            "AI 聚合网关 - 已停止"
        };
        let _ = tray.set_tooltip(Some(tip));
    }
    let _ = app.emit("gateway-state-changed", running);
}

pub fn build_tray(app: &tauri::AppHandle) -> tauri::Result<TrayItems> {
    let status_item = MenuItem::with_id(app, "status", "状态: 已停止", false, None::<&str>)?;
    let show_item = MenuItem::with_id(app, "show", "显示窗口", true, None::<&str>)?;
    let toggle_item = MenuItem::with_id(app, "toggle_gw", "启动网关", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;

    let menu = Menu::with_items(
        app,
        &[
            &status_item,
            &sep1,
            &show_item,
            &toggle_item,
            &sep2,
            &quit_item,
        ],
    )?;

    let _tray = TrayIconBuilder::with_id("main-tray")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("AI 聚合网关 - 已停止")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            "toggle_gw" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let running = app.state::<AppCtrl>().server.lock().unwrap().is_some();
                    if running {
                        let _ = stop_gateway_inner(&app).await;
                    } else {
                        let _ = start_gateway_inner(&app).await;
                    }
                });
            }
            "quit" => {
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                if let Some(window) = app.get_webview_window("main") {
                    if window.is_visible().unwrap_or(false) {
                        let _ = window.hide();
                    } else {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
            }
        })
        .build(app)?;

    Ok(TrayItems {
        status: status_item,
        toggle_gw: toggle_item,
    })
}

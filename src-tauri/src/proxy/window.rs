use std::sync::atomic::{AtomicU64, Ordering};

use tauri::webview::{Color, NewWindowResponse, WebviewBuilder};
use tauri::{AppHandle, LogicalPosition, LogicalSize, Manager, WebviewUrl};

use crate::security::navigation_allowed;

use super::{save_as_request, save_from_stage};

const ISOLATION_SCRIPT: &str = r#"
(() => {
  const lock = (key) => {
    try { delete window[key]; } catch (_) {}
    try {
      Object.defineProperty(window, key, {
        configurable: false,
        enumerable: false,
        writable: false,
        value: undefined
      });
    } catch (_) {}
  };
  for (const key of ["__TAURI_INTERNALS__", "__TAURI__", "__TAURI_EVENT_PLUGIN_INTERNALS__"]) {
    lock(key);
  }
})();
"#;

pub fn next_stage_label() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("stage-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// Attach the stall page under the main window. Its label is `stage-*`, which
/// matches no capability, so the page cannot call Tauri.
pub fn attach_stage(app: &AppHandle, label: &str, target: &str, port: u16) -> anyhow::Result<()> {
    let url: url::Url = target.parse()?;
    if !navigation_allowed(&url, port) {
        anyhow::bail!("这个地址不能在闪摊里打开");
    }
    let window = app
        .get_window("main")
        .ok_or_else(|| anyhow::anyhow!("找不到闪摊窗口"))?;
    let (width, height) = stage_size(&window);
    let app_handle = app.clone();
    window
        .add_child(
            WebviewBuilder::new(label, WebviewUrl::External(url))
                .incognito(true)
                .devtools(cfg!(debug_assertions))
                .disable_drag_drop_handler()
                .initialization_script(ISOLATION_SCRIPT)
                .initialization_script_for_all_frames(ISOLATION_SCRIPT)
                .on_navigation(move |next| {
                    if let Some((media_path, file_name)) = save_as_request(&next, port) {
                        let app = app_handle.clone();
                        let origin = format!("http://127.0.0.1:{port}");
                        tauri::async_runtime::spawn(async move {
                            if let Err(err) =
                                save_from_stage(app, origin, media_path, file_name).await
                            {
                                tracing::warn!(%err, "stall save-as failed");
                            }
                        });
                        return false;
                    }
                    navigation_allowed(&next, port)
                })
                .on_new_window(|_url, _features| NewWindowResponse::Deny)
                .background_color(Color(0xf3, 0xec, 0xdf, 255)),
            // Match StageView header (h-14 ≈ 56px). place_stage corrects once layout settles.
            LogicalPosition::new(0.0, 56.0),
            LogicalSize::new(width, height),
        )
        .map_err(|err| anyhow::anyhow!("打开摊位页面失败: {err}"))?;
    Ok(())
}

/// Floor so a bad first layout cannot park the child webview over Stage chrome.
const STAGE_CHROME_FLOOR: f64 = 56.0;

pub fn place_stage(app: &AppHandle, label: &str, x: f64, y: f64, width: f64, height: f64) -> anyhow::Result<()> {
    let Some(webview) = app.get_webview(label) else {
        return Ok(());
    };
    let top = y.max(STAGE_CHROME_FLOOR);
    let size = if let Some(window) = app.get_window("main") {
        let (win_w, win_h) = stage_window_logical(&window);
        (
            width.max(1.0).min(win_w.max(1.0)),
            height.max(1.0).min((win_h - top).max(1.0)),
        )
    } else {
        (width.max(1.0), height.max(1.0))
    };
    webview
        .set_position(LogicalPosition::new(x.max(0.0), top))
        .map_err(|err| anyhow::anyhow!("移动摊位页面失败: {err}"))?;
    webview
        .set_size(LogicalSize::new(size.0, size.1))
        .map_err(|err| anyhow::anyhow!("调整摊位页面失败: {err}"))?;
    Ok(())
}

fn stage_window_logical(window: &tauri::Window) -> (f64, f64) {
    let Ok(inner) = window.inner_size() else {
        return (960.0, 640.0);
    };
    let scale = window.scale_factor().unwrap_or(1.0);
    let logical: LogicalSize<f64> = inner.to_logical(scale);
    (logical.width.max(1.0), logical.height.max(1.0))
}

pub fn close_stage_view(app: &AppHandle, label: &str) {
    if let Some(webview) = app.get_webview(label) {
        let _ = webview.close();
    }
}

fn stage_size(window: &tauri::Window) -> (f64, f64) {
    let (width, height) = stage_window_logical(window);
    (width, (height - 56.0).max(1.0))
}

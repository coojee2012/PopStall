use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, State};
use tokio::net::TcpStream;

use crate::p2p::{HostTarget, Node, PlazaSnapshot};
use crate::proxy::{GuestProxy, attach_stage, close_stage_view, next_stage_label, place_stage};
use crate::server::{ServeSpec, StaticHost, Theme, scan_folder as classify_folder};
use crate::state::{AppState, GuestSession, HostSession, StageSession};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostInfo {
    pub share_code: String,
    pub endpoint_id: String,
    pub local_url: String,
    pub root: String,
    pub relay_ready: bool,
    pub mode_label: String,
    pub locked: bool,
}

#[tauri::command]
pub async fn scan_folder(root: String) -> Result<crate::server::ScanReport, String> {
    let path = PathBuf::from(root);
    tokio::task::spawn_blocking(move || classify_folder(&path).map(|shelf| shelf.report))
        .await
        .map_err(|err| err.to_string())?
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn start_host(
    state: State<'_, AppState>,
    mode: String,
    root: Option<String>,
    port: Option<u16>,
    blurb: Option<String>,
    password: Option<String>,
) -> Result<HostInfo, String> {
    start_host_inner(&state, mode, root, port, blurb.unwrap_or_default(), password.unwrap_or_default())
        .await
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn stop_host(state: State<'_, AppState>) -> Result<(), String> {
    stop_host_inner(&state).await.map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn host_status(state: State<'_, AppState>) -> Result<Option<HostInfo>, String> {
    let material = state.started_node().map(|node| node.share_snapshot());
    let mut host = state.host.lock().await;
    Ok(host.as_mut().map(|session| {
        if let Some(material) = material {
            session.share_code.clone_from(&material.code);
            session.endpoint_id.clone_from(&material.endpoint_id);
            session.relay_ready = material.relay_ready;
        }
        HostInfo {
            share_code: session.share_code.clone(),
            endpoint_id: session.endpoint_id.clone(),
            local_url: session.site.local_url(),
            root: session.site.root_label.clone(),
            relay_ready: session.relay_ready,
            mode_label: session.site.mode_label.clone(),
            locked: session.locked,
        }
    }))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteStatus {
    pub relay_only: bool,
}

#[tauri::command]
pub async fn route_status(state: State<'_, AppState>) -> Result<RouteStatus, String> {
    let relay_only = state.started_node().is_some_and(|node| node.relay_only());
    Ok(RouteStatus { relay_only })
}

#[tauri::command]
pub async fn list_plaza(state: State<'_, AppState>) -> Result<PlazaSnapshot, String> {
    let node = state.node().await.map_err(|err| err.to_string())?;
    Ok(node.plaza_snapshot().await)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TalkSnapshot {
    pub lines: Vec<crate::p2p::talk::TalkLineDto>,
}

#[tauri::command]
pub async fn talk_snapshot(state: State<'_, AppState>) -> Result<TalkSnapshot, String> {
    let Some(node) = state.started_node() else {
        return Ok(TalkSnapshot { lines: Vec::new() });
    };
    Ok(TalkSnapshot {
        lines: node.talk_lines().await,
    })
}

#[tauri::command]
pub async fn talk_send(state: State<'_, AppState>, text: String) -> Result<TalkSnapshot, String> {
    let node = state.node().await.map_err(|err| err.to_string())?;
    node.talk_say(&text).await?;
    Ok(TalkSnapshot {
        lines: node.talk_lines().await,
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrafficSnapshot {
    pub sent: u64,
    pub received: u64,
}

#[tauri::command]
pub async fn traffic(state: State<'_, AppState>) -> Result<TrafficSnapshot, String> {
    let Some(node) = state.started_node() else {
        return Ok(TrafficSnapshot {
            sent: 0,
            received: 0,
        });
    };
    let (sent, received) = node.traffic().snapshot();
    Ok(TrafficSnapshot { sent, received })
}

#[tauri::command]
pub async fn open_discovered(
    app: AppHandle,
    state: State<'_, AppState>,
    endpoint_id: String,
    title: String,
    password: Option<String>,
) -> Result<StageInfo, String> {
    open_discovered_inner(&app, &state, endpoint_id, title, password.unwrap_or_default())
        .await
        .map_err(|err| err.to_string())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StageInfo {
    pub label: String,
    pub title: String,
    pub kind: String,
    pub endpoint_id: Option<String>,
}

#[tauri::command]
pub async fn open_preview(
    app: AppHandle,
    state: State<'_, AppState>,
    url: String,
    title: String,
) -> Result<StageInfo, String> {
    open_preview_inner(&app, &state, url, title)
        .await
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn open_guest(
    app: AppHandle,
    state: State<'_, AppState>,
    share_code: String,
    password: Option<String>,
) -> Result<StageInfo, String> {
    open_guest_inner(&app, &state, share_code, password.unwrap_or_default())
        .await
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn place_stage_bounds(
    app: AppHandle,
    state: State<'_, AppState>,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<(), String> {
    let label = state
        .stage
        .lock()
        .await
        .as_ref()
        .map(|stage| stage.label.clone());
    let Some(label) = label else {
        return Ok(());
    };
    place_stage(&app, &label, x, y, width, height).map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn close_stage(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    dismiss_stage(&app, &state).await;
    Ok(())
}

async fn start_host_inner(
    state: &AppState,
    mode: String,
    root: Option<String>,
    port: Option<u16>,
    blurb: String,
    password: String,
) -> anyhow::Result<HostInfo> {
    let _operation = state.host_ops.lock().await;
    let spec = prepare_spec(mode, root, port).await?;
    stop_locked(state).await?;
    let password = normalize_password(&password)?;
    let node = state.node().await?;
    let site = StaticHost::open(spec).await?;
    node.set_host(HostTarget {
        router: site.router.clone(),
        password: password.clone(),
    })
    .await;
    let (title, label) = stall_blurb(&site);
    node.announce_stall(&title, &label, &blurb, password.is_some());
    let material = node.share_material().await;
    let info = HostInfo {
        share_code: material.code.clone(),
        endpoint_id: material.endpoint_id.clone(),
        local_url: site.local_url(),
        root: site.root_label.clone(),
        relay_ready: material.relay_ready,
        mode_label: site.mode_label.clone(),
        locked: password.is_some(),
    };
    *state.host.lock().await = Some(HostSession {
        site,
        share_code: material.code,
        endpoint_id: material.endpoint_id,
        relay_ready: material.relay_ready,
        locked: password.is_some(),
    });
    Ok(info)
}

async fn prepare_spec(
    mode: String,
    root: Option<String>,
    port: Option<u16>,
) -> anyhow::Result<ServeSpec> {
    match mode.as_str() {
        "site" => {
            let path = require_dir(root)?;
            if !path.join("index.html").is_file() {
                anyhow::bail!("这个文件夹没有 index.html");
            }
            Ok(ServeSpec::Site { root: path })
        }
        "app" => {
            let port = port.ok_or_else(|| anyhow::anyhow!("先填写本机端口"))?;
            ensure_port_open(port).await?;
            Ok(ServeSpec::App { port })
        }
        other => {
            let theme = Theme::parse(other).ok_or_else(|| anyhow::anyhow!("未知的开张方式"))?;
            let path = require_dir(root)?;
            let shelf = tokio::task::spawn_blocking({
                let path = path.clone();
                move || classify_folder(&path)
            })
            .await??;
            if shelf.report.counts.get(theme) == 0 {
                anyhow::bail!("这个文件夹里没有可以展出的{}", theme.label());
            }
            Ok(ServeSpec::Exhibition { root: path, theme })
        }
    }
}

fn require_dir(root: Option<String>) -> anyhow::Result<PathBuf> {
    let Some(root) = root else {
        anyhow::bail!("先选择一个文件夹");
    };
    let path = PathBuf::from(root);
    if !path.is_dir() {
        anyhow::bail!("这个路径不是文件夹");
    }
    Ok(path)
}

async fn ensure_port_open(port: u16) -> anyhow::Result<()> {
    if port == 0 {
        anyhow::bail!("端口无效");
    }
    match tokio::time::timeout(
        Duration::from_millis(700),
        TcpStream::connect((Ipv4Addr::LOCALHOST, port)),
    )
    .await
    {
        Ok(Ok(stream)) => {
            drop(stream);
            Ok(())
        }
        _ => anyhow::bail!("127.0.0.1:{port} 没有在监听。先在本机把应用跑起来。"),
    }
}

async fn stop_host_inner(state: &AppState) -> anyhow::Result<()> {
    let _operation = state.host_ops.lock().await;
    stop_locked(state).await
}

async fn stop_locked(state: &AppState) -> anyhow::Result<()> {
    let session = state.host.lock().await.take();
    if let Some(session) = session {
        if let Some(node) = state.started_node() {
            node.retract_stall();
            node.clear_host().await;
        }
        session.site.shutdown();
    }
    Ok(())
}

async fn open_preview_inner(
    app: &AppHandle,
    state: &AppState,
    url: String,
    title: String,
) -> anyhow::Result<StageInfo> {
    let port = loopback_port(&url)?;
    let label = next_stage_label();
    attach_stage(app, &label, &url, port)?;
    dismiss_stage(app, state).await;
    let title = title.trim();
    let title = if title.is_empty() {
        "本机预览".to_string()
    } else {
        title.to_string()
    };
    *state.stage.lock().await = Some(StageSession {
        label: label.clone(),
        guest: None,
    });
    Ok(StageInfo {
        label,
        title,
        kind: "preview".to_string(),
        endpoint_id: None,
    })
}

async fn open_guest_inner(
    app: &AppHandle,
    state: &AppState,
    share_code: String,
    password: String,
) -> anyhow::Result<StageInfo> {
    let share_code = share_code.trim().to_string();
    if share_code.is_empty() {
        anyhow::bail!("先粘贴对方的分享码");
    }
    let password = normalize_password(&password)?.unwrap_or_default();
    let endpoint_id = Node::endpoint_id_of(&share_code)?;
    let node = state.node().await?;
    let connection = node.connect(&share_code).await?;
    open_connection_stage(app, state, connection, "对方的摊位", endpoint_id, password).await
}

async fn open_discovered_inner(
    app: &AppHandle,
    state: &AppState,
    endpoint_id: String,
    title: String,
    password: String,
) -> anyhow::Result<StageInfo> {
    let endpoint_id = endpoint_id.trim().to_string();
    if endpoint_id.is_empty() {
        anyhow::bail!("这个摊位的地址无效");
    }
    let title = title.trim();
    let title = if title.is_empty() {
        "广场上的摊位"
    } else {
        title
    };
    let password = normalize_password(&password)?.unwrap_or_default();
    let node = state.node().await?;
    let connection = node.connect_endpoint(&endpoint_id).await?;
    open_connection_stage(app, state, connection, title, endpoint_id, password).await
}

async fn open_connection_stage(
    app: &AppHandle,
    state: &AppState,
    connection: iroh::endpoint::Connection,
    title: &str,
    endpoint_id: String,
    password: String,
) -> anyhow::Result<StageInfo> {
    if let Err(err) = check_password(&connection, &password).await {
        connection.close(0u8.into(), b"password");
        return Err(err);
    }
    let traffic = state.node().await?.traffic();
    let proxy = GuestProxy::start(connection.clone(), traffic, password).await?;
    let label = next_stage_label();
    let proxy_url = proxy.url();
    if let Err(err) = attach_stage(app, &label, &proxy_url, proxy.port) {
        proxy.shutdown();
        connection.close(0u8.into(), b"stage failed");
        return Err(err);
    }
    dismiss_stage(app, state).await;
    *state.stage.lock().await = Some(StageSession {
        label: label.clone(),
        guest: Some(GuestSession { proxy, connection }),
    });
    Ok(StageInfo {
        label,
        title: title.to_string(),
        kind: "guest".to_string(),
        endpoint_id: Some(endpoint_id),
    })
}

fn normalize_password(raw: &str) -> anyhow::Result<Option<String>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > 48 {
        anyhow::bail!("密码最多 48 个字");
    }
    if trimmed.chars().any(|ch| ch.is_control()) {
        anyhow::bail!("密码里不能有控制字符");
    }
    Ok(Some(trimmed.to_string()))
}

async fn check_password(connection: &iroh::endpoint::Connection, password: &str) -> anyhow::Result<()> {
    if password.is_empty() {
        return Ok(());
    }
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|err| anyhow::anyhow!("打开加密流失败: {err:?}"))?;
    let frame = crate::p2p::encode_request(&crate::p2p::RequestFrame {
        method: crate::p2p::Method::Get,
        target: "/".into(),
        range: None,
        content_type: None,
        body: Vec::new(),
        pass: password.to_string(),
    })?;
    send.write_all(&frame).await?;
    send.finish()
        .map_err(|err| anyhow::anyhow!("结束密码确认失败: {err:?}"))?;
    let head = crate::p2p::read_response_head(&mut recv).await?;
    if head.status != 401 {
        return Ok(());
    }
    let body = recv.read_to_end(256).await.unwrap_or_default();
    let message = String::from_utf8_lossy(&body).trim().to_string();
    anyhow::bail!(if message.is_empty() { "密码不对".to_string() } else { message })
}

fn stall_blurb(site: &StaticHost) -> (String, String) {
    let title = std::path::Path::new(&site.root_label)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(site.root_label.as_str());
    (title.to_string(), site.mode_label.clone())
}

async fn dismiss_stage(app: &AppHandle, state: &AppState) {
    let previous = state.stage.lock().await.take();
    if let Some(stage) = previous {
        close_stage_view(app, &stage.label);
        if let Some(guest) = stage.guest {
            guest.shutdown();
        }
    }
}

fn loopback_port(raw: &str) -> anyhow::Result<u16> {
    let url = url::Url::parse(raw)?;
    if url.scheme() != "http" || url.host_str() != Some("127.0.0.1") {
        anyhow::bail!("只能在闪摊里打开本机摊位");
    }
    url.port()
        .ok_or_else(|| anyhow::anyhow!("预览地址没有端口"))
}

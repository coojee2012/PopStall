use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::stream::unfold;
use http_body_util::{BodyExt, Limited};
use iroh::endpoint::{Connection, RecvStream};
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::sync::watch;

use crate::p2p::{Method, RequestFrame, Traffic, encode_request, read_response_head};
use crate::security::{MAX_REQUEST_BODY, MAX_RESPONSE_BYTES, is_restricted_host, validate_target};
use crate::server::{Theme, render_exhibit, stall_asset};

use super::PROXY_CSP;
use super::media::{MediaHub, is_media_target};

pub struct GuestProxy {
    pub port: u16,
    shutdown: watch::Sender<bool>,
}

impl GuestProxy {
    pub async fn start(connection: Connection, traffic: Arc<Traffic>, password: String) -> anyhow::Result<Self> {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await?;
        let port = listener.local_addr()?.port();
        let media = MediaHub::new(connection.clone(), traffic.clone(), password.clone());
        let app = Router::new()
            .fallback(forward)
            .with_state(ProxyState {
                connection,
                port,
                traffic,
                media,
                password,
            });
        let (shutdown, mut stop) = watch::channel(false);
        tokio::spawn(async move {
            let result = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = stop.wait_for(|flag| *flag).await;
                })
                .await;
            if let Err(err) = result {
                tracing::warn!(%err, "guest proxy stopped with an error");
            }
        });
        Ok(Self { port, shutdown })
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }

    pub fn shutdown_handle(&self) -> watch::Sender<bool> {
        self.shutdown.clone()
    }

    pub fn shutdown(self) {
        let _ = self.shutdown.send(true);
    }
}

impl Drop for GuestProxy {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

#[derive(Clone)]
struct ProxyState {
    connection: Connection,
    port: u16,
    traffic: Arc<Traffic>,
    media: MediaHub,
    password: String,
}

async fn forward(State(state): State<ProxyState>, request: Request) -> Response {
    let method = match crate::p2p::Method::from_http(request.method()) {
        Some(method) => method,
        None => return locked(StatusCode::METHOD_NOT_ALLOWED, "这个方法不能转发到摊位"),
    };
    if let Err(response) = check_host(&request, state.port) {
        return response;
    }
    let target = request
        .uri()
        .path_and_query()
        .map(|value| value.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());
    if let Err(err) = validate_target(&target) {
        return locked(StatusCode::BAD_REQUEST, &err.to_string());
    }
    let path_only = target.split(['?', '#']).next().unwrap_or(target.as_str());
    if matches!(method, Method::Get | Method::Head) {
        // Exhibition shell comes from this build — not the remote host — so UI
        // fixes (download, layout) apply when visiting older stalls.
        if path_only == "/" || path_only == "/index.html" {
            if let Some(page) = local_exhibit_shell(&state).await {
                let body = if method == Method::Head {
                    Body::empty()
                } else {
                    Body::from(page.clone())
                };
                return Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
                    .header(header::CONTENT_LENGTH, page.len())
                    .header(header::CACHE_CONTROL, "no-store")
                    .header(header::CONTENT_SECURITY_POLICY, PROXY_CSP)
                    .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
                    .body(body)
                    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
            }
        }
        if let Some((bytes, content_type)) = stall_asset(path_only) {
            let body = if method == Method::Head {
                Body::empty()
            } else {
                Body::from(bytes)
            };
            return Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, content_type)
                .header(header::CONTENT_LENGTH, bytes.len())
                .header(header::CACHE_CONTROL, "private, max-age=86400")
                .header(header::CONTENT_SECURITY_POLICY, PROXY_CSP)
                .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
                .body(body)
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }
    }
    let range = request
        .headers()
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.len() <= 128 && value.to_ascii_lowercase().starts_with("bytes="))
        .map(str::to_string);
    if matches!(method, crate::p2p::Method::Get | crate::p2p::Method::Head) && is_media_target(&target)
    {
        return state.media.serve(method, target, range).await;
    }
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.len() <= 128)
        .map(str::to_string);
    let body = match Limited::new(request.into_body(), MAX_REQUEST_BODY).collect().await {
        Ok(collected) => collected.to_bytes().to_vec(),
        Err(_) => return locked(StatusCode::PAYLOAD_TOO_LARGE, "请求体超过 1 MiB"),
    };
    let carries_body = matches!(
        method,
        crate::p2p::Method::Post | crate::p2p::Method::Put | crate::p2p::Method::Patch
    );
    let frame = match encode_request(&RequestFrame {
        method,
        target: target.clone(),
        range,
        content_type,
        body: if carries_body { body } else { Vec::new() },
        pass: state.password.clone(),
    }) {
        Ok(frame) => frame,
        Err(err) => return locked(StatusCode::BAD_REQUEST, &err.to_string()),
    };
    state.traffic.record_sent(frame.len() as u64);

    let (mut send, mut recv) = match state.connection.open_bi().await {
        Ok(streams) => streams,
        Err(err) => return locked(StatusCode::BAD_GATEWAY, &format!("打开加密流失败: {err:?}")),
    };
    if let Err(err) = send.write_all(&frame).await {
        return locked(StatusCode::BAD_GATEWAY, &format!("发送请求失败: {err}"));
    }
    if let Err(err) = send.finish() {
        return locked(StatusCode::BAD_GATEWAY, &format!("结束请求失败: {err:?}"));
    }

    let head = match read_response_head(&mut recv).await {
        Ok(head) => head,
        Err(err) => return locked(StatusCode::BAD_GATEWAY, &err.to_string()),
    };
    let status = StatusCode::from_u16(head.status).unwrap_or(StatusCode::BAD_GATEWAY);
    let cache_control = if head.content_type.starts_with("audio/") || head.content_type.starts_with("video/") {
        "private, max-age=3600"
    } else {
        "no-store"
    };
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, head.content_type.clone())
        .header(header::CONTENT_SECURITY_POLICY, PROXY_CSP)
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::REFERRER_POLICY, "no-referrer")
        .header(header::CACHE_CONTROL, cache_control);
    if let Some(range) = head.content_range {
        builder = builder.header(header::CONTENT_RANGE, range);
    }
    if head.accept_ranges {
        builder = builder.header(header::ACCEPT_RANGES, "bytes");
    }
    if method == crate::p2p::Method::Head {
        if let Some(length) = head.content_length {
            builder = builder.header(header::CONTENT_LENGTH, length);
        }
        return builder
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    // Buffer text/docs/PDFs fully. Progressive bodies make WebView PDF/novel blank.
    let path_lower = target.to_ascii_lowercase();
    let is_document = head.content_type.contains("pdf")
        || head.content_type.starts_with("text/")
        || path_lower.contains(".pdf")
        || path_lower.contains(".txt")
        || path_lower.contains(".md");
    let known_len = head.content_length.filter(|len| *len > 0);
    let should_buffer = is_document
        && !head.content_type.starts_with("audio/")
        && !head.content_type.starts_with("video/")
        && known_len.map(|len| len <= MAX_RESPONSE_BYTES).unwrap_or(true);
    if should_buffer {
        let want = known_len
            .unwrap_or(MAX_RESPONSE_BYTES)
            .min(MAX_RESPONSE_BYTES) as usize;
        let mut buf = vec![0u8; want.min(64 * 1024)];
        let mut body = Vec::with_capacity(want.min(1024 * 1024));
        loop {
            if body.len() >= want {
                break;
            }
            let max = (want - body.len()).min(buf.len());
            match AsyncReadExt::read(&mut recv, &mut buf[..max]).await {
                Ok(0) => break,
                Ok(size) => {
                    state.traffic.record_received(size as u64);
                    body.extend_from_slice(&buf[..size]);
                }
                Err(err) => {
                    return locked(
                        StatusCode::BAD_GATEWAY,
                        &format!("读正文中断: {err}"),
                    );
                }
            }
        }
        if known_len.is_some_and(|len| (body.len() as u64) < len) {
            return locked(
                StatusCode::BAD_GATEWAY,
                &format!("文档只收到 {}/{} 字节，连接中断了", body.len(), known_len.unwrap_or(0)),
            );
        }
        return builder
            .header(header::CONTENT_LENGTH, body.len())
            .body(Body::from(body))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    if let Some(length) = head.content_length {
        builder = builder.header(header::CONTENT_LENGTH, length);
    }
    builder
        .body(Body::from_stream(body_stream(recv, state.traffic)))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Pull catalog from the host and render the local exhibit.html shell.
async fn local_exhibit_shell(state: &ProxyState) -> Option<String> {
    let body = p2p_get_bytes(state, "/_stall/catalog.json").await.ok()?;
    let catalog: serde_json::Value = serde_json::from_slice(&body).ok()?;
    let theme = Theme::parse(catalog.get("theme")?.as_str()?)?;
    let title = catalog
        .get("title")
        .and_then(|value| value.as_str())
        .unwrap_or("摊位");
    Some(render_exhibit(theme, title))
}

async fn p2p_get_bytes(state: &ProxyState, target: &str) -> anyhow::Result<Vec<u8>> {
    let frame = encode_request(&RequestFrame {
        method: Method::Get,
        target: target.to_string(),
        range: None,
        content_type: None,
        body: Vec::new(),
        pass: state.password.clone(),
    })?;
    state.traffic.record_sent(frame.len() as u64);
    let (mut send, mut recv) = state
        .connection
        .open_bi()
        .await
        .map_err(|err| anyhow::anyhow!("{err:?}"))?;
    send.write_all(&frame).await?;
    send.finish()
        .map_err(|err| anyhow::anyhow!("{err:?}"))?;
    let head = read_response_head(&mut recv).await?;
    if head.status != 200 {
        anyhow::bail!("catalog status {}", head.status);
    }
    let want = head
        .content_length
        .unwrap_or(MAX_RESPONSE_BYTES)
        .min(MAX_RESPONSE_BYTES) as usize;
    let mut body = Vec::with_capacity(want.min(64 * 1024));
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let size = AsyncReadExt::read(&mut recv, &mut buf).await?;
        if size == 0 {
            break;
        }
        state.traffic.record_received(size as u64);
        if body.len() + size > want {
            anyhow::bail!("catalog too large");
        }
        body.extend_from_slice(&buf[..size]);
    }
    Ok(body)
}

fn check_host(request: &Request, port: u16) -> Result<(), Response> {
    let Some(value) = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return Err(locked(StatusCode::BAD_REQUEST, "缺少 Host 头"));
    };
    let expected = format!("127.0.0.1:{port}");
    if value == expected || value == "127.0.0.1" {
        return Ok(());
    }
    let host_only = value.split(':').next().unwrap_or(value);
    if is_restricted_host(host_only) {
        return Err(locked(StatusCode::FORBIDDEN, "已拦截对私网地址的请求"));
    }
    Err(locked(StatusCode::BAD_REQUEST, "代理只接受本机回环地址"))
}

fn locked(status: StatusCode, message: &str) -> Response {
    let mut response = (status, message.to_string()).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(PROXY_CSP),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn body_stream(
    recv: RecvStream,
    traffic: Arc<Traffic>,
) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send {
    unfold((recv, false, traffic), |(mut recv, done, traffic)| async move {
        if done {
            return None;
        }
        let mut buf = vec![0u8; 64 * 1024];
        match AsyncReadExt::read(&mut recv, &mut buf).await {
            Ok(0) => None,
            Ok(size) => {
                traffic.record_received(size as u64);
                buf.truncate(size);
                Some((Ok(Bytes::from(buf)), (recv, false, traffic)))
            }
            Err(err) => Some((Err(std::io::Error::other(err.to_string())), (recv, true, traffic))),
        }
    })
}

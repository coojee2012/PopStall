use std::net::SocketAddr;

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

use crate::p2p::{RequestFrame, encode_request, read_response_head};
use crate::security::{MAX_REQUEST_BODY, is_restricted_host, validate_target};

const PROXY_CSP: &str = "default-src 'self' data: blob:; base-uri 'self'; form-action 'self'; frame-ancestors 'none'; object-src 'none'; img-src 'self' data: blob:; media-src 'self' blob: data:; font-src 'self' data:; style-src 'self' 'unsafe-inline'; script-src 'self' 'unsafe-inline' 'unsafe-eval' blob:; connect-src 'self'; frame-src 'self'; worker-src 'self' blob:";

pub struct GuestProxy {
    pub port: u16,
    shutdown: watch::Sender<bool>,
}

impl GuestProxy {
    pub async fn start(connection: Connection) -> anyhow::Result<Self> {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await?;
        let port = listener.local_addr()?.port();
        let app = Router::new()
            .fallback(forward)
            .with_state(ProxyState { connection, port });
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
    let range = request
        .headers()
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.len() <= 128 && value.to_ascii_lowercase().starts_with("bytes="))
        .map(str::to_string);
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
    }) {
        Ok(frame) => frame,
        Err(err) => return locked(StatusCode::BAD_REQUEST, &err.to_string()),
    };

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
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, head.content_type)
        .header(header::CONTENT_SECURITY_POLICY, PROXY_CSP)
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::REFERRER_POLICY, "no-referrer")
        .header(header::CACHE_CONTROL, "no-store");
    if let Some(length) = head.content_length {
        builder = builder.header(header::CONTENT_LENGTH, length);
    }
    if let Some(range) = head.content_range {
        builder = builder.header(header::CONTENT_RANGE, range);
    }
    if head.accept_ranges {
        builder = builder.header(header::ACCEPT_RANGES, "bytes");
    }
    if method == crate::p2p::Method::Head {
        return builder
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    builder
        .body(Body::from_stream(body_stream(recv)))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
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
) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send {
    unfold((recv, false), |(mut recv, done)| async move {
        if done {
            return None;
        }
        let mut buf = vec![0u8; 16 * 1024];
        match AsyncReadExt::read(&mut recv, &mut buf).await {
            Ok(0) => None,
            Ok(size) => {
                buf.truncate(size);
                Some((Ok(Bytes::from(buf)), (recv, false)))
            }
            Err(err) => Some((Err(std::io::Error::other(err.to_string())), (recv, true))),
        }
    })
}

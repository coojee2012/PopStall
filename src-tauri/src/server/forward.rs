use std::net::Ipv4Addr;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use hyper::upgrade::OnUpgrade;
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;

use crate::security::validate_target;

const HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-connection",
    "transfer-encoding",
    "te",
    "trailer",
    "upgrade",
];

/// Forward one request to a fixed loopback port. WebSocket upgrades are bridged
/// when the incoming request carries Hyper's upgrade handle.
pub async fn proxy(port: u16, mut request: Request) -> Response {
    if request.method() == Method::CONNECT {
        return (StatusCode::METHOD_NOT_ALLOWED, "不转发 CONNECT").into_response();
    }
    let path = request
        .uri()
        .path_and_query()
        .map(|value| value.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());
    if let Err(err) = validate_target(request.uri().path()) {
        return (StatusCode::BAD_REQUEST, err.to_string()).into_response();
    }
    let upgrade = request
        .headers()
        .get(header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
    let client_upgrade = upgrade
        .then(|| request.extensions_mut().remove::<OnUpgrade>())
        .flatten();

    let Ok(stream) = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await else {
        return (
            StatusCode::BAD_GATEWAY,
            format!("127.0.0.1:{port} 没有响应。先在本机把应用跑起来。"),
        )
            .into_response();
    };
    let upstream = match rebuild(request, port, &path, upgrade) {
        Ok(request) => request,
        Err(response) => return response,
    };

    let io = TokioIo::new(stream);
    let (mut sender, connection) = match hyper::client::conn::http1::Builder::new()
        .handshake::<_, Body>(io)
        .await
    {
        Ok(pair) => pair,
        Err(err) => {
            tracing::warn!(%err, "local app handshake failed");
            return (
                StatusCode::BAD_GATEWAY,
                "连上了端口，但没有完成 HTTP 握手。",
            )
                .into_response();
        }
    };
    tokio::spawn(async move {
        if let Err(err) = connection.await {
            tracing::debug!(%err, "local app connection closed");
        }
    });

    let response = match sender.send_request(upstream).await {
        Ok(response) => response,
        Err(err) => {
            tracing::warn!(%err, "local app request failed");
            return (StatusCode::BAD_GATEWAY, "本地应用中断了这次请求。").into_response();
        }
    };

    if response.status() == StatusCode::SWITCHING_PROTOCOLS
        && let Some(client_upgrade) = client_upgrade
    {
        let headers = response.headers().clone();
        tokio::spawn(async move {
            let server = match hyper::upgrade::on(response).await {
                Ok(upgraded) => upgraded,
                Err(err) => {
                    tracing::warn!(%err, "local app upgrade failed");
                    return;
                }
            };
            let client = match client_upgrade.await {
                Ok(upgraded) => upgraded,
                Err(err) => {
                    tracing::warn!(%err, "browser upgrade failed");
                    return;
                }
            };
            let mut client = TokioIo::new(client);
            let mut server = TokioIo::new(server);
            if let Err(err) = tokio::io::copy_bidirectional(&mut client, &mut server).await {
                tracing::debug!(%err, "websocket bridge ended");
            }
        });
        let mut out = Response::builder().status(StatusCode::SWITCHING_PROTOCOLS);
        for (name, value) in headers.iter() {
            if name == header::CONTENT_LENGTH || name == header::TRANSFER_ENCODING {
                continue;
            }
            out = out.header(name, value);
        }
        return out
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
    }

    let (parts, incoming) = response.into_parts();
    let mut out = Response::builder().status(parts.status);
    for (name, value) in parts.headers.iter() {
        if HOP
            .iter()
            .any(|hop| name.as_str().eq_ignore_ascii_case(hop))
        {
            continue;
        }
        out = out.header(name, value);
    }
    out.body(Body::new(incoming))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

fn rebuild(request: Request, port: u16, path: &str, upgrade: bool) -> Result<Request, Response> {
    let uri: Uri = match format!("http://127.0.0.1:{port}{path}").parse() {
        Ok(uri) => uri,
        Err(_) => return Err((StatusCode::BAD_REQUEST, "无法转写本地地址").into_response()),
    };
    let (mut parts, body) = request.into_parts();
    parts.uri = uri;
    if !upgrade {
        for hop in HOP {
            parts.headers.remove(*hop);
        }
    }
    match HeaderValue::from_str(&format!("127.0.0.1:{port}")) {
        Ok(value) => {
            parts.headers.insert(header::HOST, value);
        }
        Err(_) => return Err((StatusCode::BAD_REQUEST, "端口无效").into_response()),
    }
    Ok(Request::from_parts(parts, body))
}

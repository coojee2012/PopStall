use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_util::io::ReaderStream;
use tower_http::trace::TraceLayer;

use super::catalog::{MediaFile, Theme, files_for_theme, scan_folder};
use super::exhibit::render_exhibit;
use super::forward;
use crate::security::{
    MAX_MEDIA_BYTES, MAX_RESPONSE_BYTES, ResolvedPath, SafePathError, resolve_within_root, validate_target,
};

const PATH_SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'&')
    .add(b'/')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'\\')
    .add(b'`');

pub enum ServeSpec {
    Site { root: PathBuf },
    Exhibition { root: PathBuf, theme: Theme },
    App { port: u16 },
}

#[derive(Clone)]
enum Engine {
    Site { root: Arc<PathBuf> },
    Exhibition {
        root: Arc<PathBuf>,
        theme: Theme,
        page: Arc<String>,
        catalog: Arc<String>,
    },
    App { port: u16 },
}

pub struct StaticHost {
    pub root_label: String,
    pub mode_label: String,
    pub local_addr: SocketAddr,
    pub router: Router,
    shutdown: watch::Sender<bool>,
}

impl StaticHost {
    pub async fn bind(root: PathBuf) -> anyhow::Result<Self> {
        Self::open(ServeSpec::Site { root }).await
    }

    pub async fn open(spec: ServeSpec) -> anyhow::Result<Self> {
        let (engine, root_label, mode_label) = match spec {
            ServeSpec::Site { root } => {
                let root = canonical_dir(root)?;
                let label = root.display().to_string();
                (
                    Engine::Site {
                        root: Arc::new(root),
                    },
                    label,
                    "原样网站".to_string(),
                )
            }
            ServeSpec::Exhibition { root, theme } => {
                let root = canonical_dir(root)?;
                let shelf = scan_folder(&root)?;
                let items = files_for_theme(&shelf.files, theme);
                let page = render_exhibit(theme, &shelf.report.folder_name);
                let catalog = serde_json::to_string(&PublicCatalog {
                    theme: theme.as_str(),
                    title: shelf.report.folder_name.clone(),
                    truncated: shelf.report.truncated,
                    items: items.iter().map(public_item).collect(),
                })?;
                let label = root.display().to_string();
                (
                    Engine::Exhibition {
                        root: Arc::new(root),
                        theme,
                        page: Arc::new(page),
                        catalog: Arc::new(catalog),
                    },
                    label,
                    format!("{}展厅", theme.label()),
                )
            }
            ServeSpec::App { port } => {
                if port == 0 {
                    anyhow::bail!("端口无效");
                }
                (
                    Engine::App { port },
                    format!("127.0.0.1:{port}"),
                    format!("本地应用 :{port}"),
                )
            }
        };

        let router = Router::new()
            .fallback(dispatch)
            .layer(TraceLayer::new_for_http())
            .with_state(engine);
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await?;
        let local_addr = listener.local_addr()?;
        let (shutdown, mut stop) = watch::channel(false);
        let served = router.clone();
        tokio::spawn(async move {
            let result = axum::serve(listener, served)
                .with_graceful_shutdown(async move {
                    let _ = stop.wait_for(|flag| *flag).await;
                })
                .await;
            if let Err(err) = result {
                tracing::warn!(%err, "static host stopped with an error");
            }
        });
        tracing::info!(%local_addr, %root_label, %mode_label, "stall is listening");
        Ok(Self {
            root_label,
            mode_label,
            local_addr,
            router,
            shutdown,
        })
    }

    pub fn local_url(&self) -> String {
        format!("http://{}/", self.local_addr)
    }

    pub fn shutdown(self) {
        let _ = self.shutdown.send(true);
    }
}

impl Drop for StaticHost {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

fn canonical_dir(root: PathBuf) -> anyhow::Result<PathBuf> {
    if !root.is_dir() {
        anyhow::bail!("只能托管文件夹");
    }
    Ok(root.canonicalize()?)
}

async fn dispatch(State(engine): State<Engine>, request: Request) -> Response {
    match engine {
        Engine::App { port } => forward::proxy(port, request).await,
        Engine::Site { root } => serve_site(root, request).await,
        Engine::Exhibition {
            root,
            theme,
            page,
            catalog,
        } => serve_exhibition(root, theme, page, catalog, request).await,
    }
}

async fn serve_site(root: Arc<PathBuf>, request: Request) -> Response {
    let method = request.method().clone();
    if method != Method::GET && method != Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let target = request_target(&request);
    let range = header_str(&request, header::RANGE);
    if let Err(err) = validate_target(&target) {
        return path_response(err);
    }
    match resolve_within_root(root.as_path(), &target) {
        Ok(ResolvedPath::File(path)) => serve_file(&method, path, range.as_deref()).await,
        Ok(ResolvedPath::Directory(_)) => {
            let index_target = index_target(&target);
            match resolve_within_root(root.as_path(), &index_target) {
                Ok(ResolvedPath::File(path)) => serve_file(&method, path, range.as_deref()).await,
                _ => html(
                    &method,
                    StatusCode::NOT_FOUND,
                    "<!doctype html><meta charset=\"utf-8\"><title>闪摊</title><p>这个文件夹没有 index.html。回到闪摊，改选图片、音乐、小说或其他主题。</p>",
                ),
            }
        }
        Err(err) => path_response(err),
    }
}

async fn serve_exhibition(
    root: Arc<PathBuf>,
    theme: Theme,
    page: Arc<String>,
    catalog: Arc<String>,
    request: Request,
) -> Response {
    let method = request.method().clone();
    if method != Method::GET && method != Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let target = request_target(&request);
    let path_only = target.split(['?', '#']).next().unwrap_or(target.as_str());
    if let Err(err) = validate_target(path_only) {
        return path_response(err);
    }
    if path_only == "/" || path_only == "/index.html" {
        return html(&method, StatusCode::OK, page.as_str());
    }
    if path_only == "/_stall/catalog.json" {
        return bytes_response(
            &method,
            StatusCode::OK,
            "application/json; charset=utf-8",
            catalog.as_bytes(),
        );
    }
    if let Some((bytes, content_type)) = crate::server::stall_asset(path_only) {
        return bytes_response(&method, StatusCode::OK, content_type, bytes);
    }
    let Some(rest) = path_only.strip_prefix("/media/") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if rest.is_empty() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let range = header_str(&request, header::RANGE);
    match resolve_within_root(root.as_path(), &format!("/{rest}")) {
        Ok(ResolvedPath::File(path)) if theme_allows(theme, &path) => {
            serve_file(&method, path, range.as_deref()).await
        }
        Ok(_) => StatusCode::NOT_FOUND.into_response(),
        Err(err) => path_response(err),
    }
}

fn theme_allows(theme: Theme, path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| theme.matches(ext))
}

async fn serve_file(method: &Method, path: PathBuf, range_header: Option<&str>) -> Response {
    let mut file = match tokio::fs::File::open(&path).await {
        Ok(file) => file,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let total = match file.metadata().await {
        Ok(metadata) => metadata.len(),
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let content_type = content_type_for(&path);
    let media = content_type.starts_with("audio/") || content_type.starts_with("video/");
    let ceiling = if media { MAX_MEDIA_BYTES } else { MAX_RESPONSE_BYTES };
    let (status, start, end) = if let Some(header) = range_header {
        match parse_byte_range(header, total, ceiling) {
            Some((start, end)) => (StatusCode::PARTIAL_CONTENT, start, end),
            None => {
                return Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::CONTENT_RANGE, format!("bytes */{total}"))
                    .header(header::ACCEPT_RANGES, "bytes")
                    .body(Body::empty())
                    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
            }
        }
    } else if total > ceiling {
        let message = if media {
            "这个影音超过 32 GB。"
        } else {
            "这个文件太大，需要分段读取。"
        };
        return Response::builder()
            .status(StatusCode::PAYLOAD_TOO_LARGE)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(Body::from(message))
            .unwrap_or_else(|_| StatusCode::PAYLOAD_TOO_LARGE.into_response());
    } else {
        (StatusCode::OK, 0, total.saturating_sub(1))
    };
    if total == 0 {
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, content_type)
            .header(header::CONTENT_LENGTH, 0)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CACHE_CONTROL, "no-store")
            .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    let length = end - start + 1;
    if let Err(err) = file.seek(std::io::SeekFrom::Start(start)).await {
        tracing::warn!(%err, "failed to seek stall file");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, length)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    if status == StatusCode::PARTIAL_CONTENT {
        builder = builder.header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{total}"),
        );
    }
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        Body::from_stream(ReaderStream::new(file.take(length)))
    };
    builder
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn parse_byte_range(header: &str, total: u64, ceiling: u64) -> Option<(u64, u64)> {
    if total == 0 {
        return None;
    }
    let spec = header
        .trim()
        .strip_prefix("bytes=")?
        .split(',')
        .next()?
        .trim();
    let (start, mut end) = if let Some(suffix) = spec.strip_prefix('-') {
        let n: u64 = suffix.parse().ok()?;
        if n == 0 || n > total {
            return None;
        }
        (total - n, total - 1)
    } else {
        let (start_text, end_text) = spec.split_once('-')?;
        let start = start_text.parse().ok()?;
        if start >= total {
            return None;
        }
        let end = if end_text.is_empty() {
            total - 1
        } else {
            end_text.parse::<u64>().ok()?.min(total - 1)
        };
        if end < start {
            return None;
        }
        (start, end)
    };
    if end - start + 1 > ceiling {
        end = start + ceiling - 1;
    }
    Some((start, end))
}

fn html(method: &Method, status: StatusCode, body: &str) -> Response {
    bytes_response(method, status, "text/html; charset=utf-8", body.as_bytes())
}

fn bytes_response(method: &Method, status: StatusCode, content_type: &str, body: &[u8]) -> Response {
    let payload = if method == Method::HEAD {
        Body::empty()
    } else {
        Body::from(body.to_vec())
    };
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, body.len())
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(payload)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn request_target(request: &Request) -> String {
    request
        .uri()
        .path_and_query()
        .map(|value| value.as_str().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/".to_string())
}

fn header_str(request: &Request, name: header::HeaderName) -> Option<String> {
    request
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn index_target(target: &str) -> String {
    let path = target.split(['?', '#']).next().unwrap_or(target);
    if path == "/" {
        "/index.html".to_string()
    } else {
        format!("{}/index.html", path.trim_end_matches('/'))
    }
}

fn path_response(err: SafePathError) -> Response {
    match err {
        SafePathError::NotFound => StatusCode::NOT_FOUND.into_response(),
        SafePathError::RootUnavailable | SafePathError::Io => {
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
        SafePathError::Traversal | SafePathError::Dotfile | SafePathError::Encoding => {
            StatusCode::BAD_REQUEST.into_response()
        }
    }
}

fn content_type_for(path: &Path) -> String {
    let raw = mime_guess::from_path(path)
        .first_raw()
        .unwrap_or("application/octet-stream");
    if raw.starts_with("text/")
        || raw.contains("javascript")
        || raw.contains("json")
        || raw.contains("xml")
        || raw.contains("svg")
    {
        format!("{raw}; charset=utf-8")
    } else {
        raw.to_string()
    }
}

fn public_item(file: &MediaFile) -> PublicItem {
    PublicItem {
        title: file.title.clone(),
        path: media_path(&file.relative),
        size: file.size,
    }
}

fn media_path(relative: &Path) -> String {
    let mut out = String::from("/media");
    for component in relative.components() {
        let Component::Normal(part) = component else {
            continue;
        };
        out.push('/');
        out.push_str(&utf8_percent_encode(&part.to_string_lossy(), PATH_SEGMENT).to_string());
    }
    out
}

#[derive(Serialize)]
struct PublicCatalog {
    theme: &'static str,
    title: String,
    truncated: bool,
    items: Vec<PublicItem>,
}

#[derive(Serialize)]
struct PublicItem {
    title: String,
    path: String,
    size: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::{get, post};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    async fn raw_http(addr: SocketAddr, request: &str) -> String {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf).into_owned()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn serves_index_and_blocks_traversal() {
        let root = std::env::temp_dir().join(format!("popstall-http-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(root.join("nested")).await.unwrap();
        tokio::fs::write(root.join("index.html"), b"hello-stall")
            .await
            .unwrap();
        tokio::fs::write(root.join("nested/note.txt"), b"nested-note")
            .await
            .unwrap();

        let host = StaticHost::bind(root.clone()).await.unwrap();
        let home = raw_http(
            host.local_addr,
            "GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(home.contains("200"), "{home}");
        assert!(home.contains("hello-stall"));

        let note = raw_http(
            host.local_addr,
            "GET /nested/note.txt HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(note.contains("nested-note"));

        let escaped = raw_http(
            host.local_addr,
            "GET /%2e%2e/%2e%2e/etc/passwd HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(escaped.contains("400"), "{escaped}");

        let hidden = raw_http(
            host.local_addr,
            "GET /.env HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(hidden.contains("400"), "{hidden}");

        let partial = raw_http(
            host.local_addr,
            "GET /nested/note.txt HTTP/1.1\r\nHost: 127.0.0.1\r\nRange: bytes=0-5\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(partial.contains("206"), "{partial}");
        assert!(partial.contains("bytes 0-5/"));
        assert!(partial.contains("nested"));

        host.shutdown();
        let _ = tokio::fs::remove_dir_all(root).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn exhibition_lists_only_the_chosen_theme() {
        let root = std::env::temp_dir().join(format!("popstall-theme-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&root).await.unwrap();
        tokio::fs::write(root.join("cover.png"), b"not-a-real-png")
            .await
            .unwrap();
        tokio::fs::write(root.join("notes.txt"), b"secret-note")
            .await
            .unwrap();

        let host = StaticHost::open(ServeSpec::Exhibition {
            root: root.clone(),
            theme: Theme::Image,
        })
        .await
        .unwrap();
        let page = raw_http(
            host.local_addr,
            "GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(page.contains("200"), "{page}");
        assert!(page.contains("data-theme=\"image\""));
        assert!(!page.contains("secret-note"));

        let catalog = raw_http(
            host.local_addr,
            "GET /_stall/catalog.json HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(catalog.contains("/media/cover.png"), "{catalog}");
        assert!(!catalog.contains("notes.txt"), "{catalog}");

        let image = raw_http(
            host.local_addr,
            "GET /media/cover.png HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(image.contains("not-a-real-png"), "{image}");

        let hidden = raw_http(
            host.local_addr,
            "GET /media/notes.txt HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(hidden.contains("404"), "{hidden}");

        host.shutdown();
        let _ = tokio::fs::remove_dir_all(root).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn local_app_forwards_get_and_post() {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = Router::new()
            .route("/hello", get(|| async { "pong" }))
            .route("/echo", post(|body: String| async move { body }));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let host = StaticHost::open(ServeSpec::App { port }).await.unwrap();
        let hello = raw_http(
            host.local_addr,
            "GET /hello HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(hello.contains("pong"), "{hello}");

        let echo = raw_http(
            host.local_addr,
            "POST /echo HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: text/plain\r\nContent-Length: 4\r\nConnection: close\r\n\r\nping",
        )
        .await;
        assert!(echo.contains("ping"), "{echo}");

        let escaped = raw_http(
            host.local_addr,
            "GET /%2e%2e/secret HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(escaped.contains("400"), "{escaped}");

        host.shutdown();
    }
}

use std::path::{Path, PathBuf};

use tauri::AppHandle;
use tauri_plugin_dialog::{DialogExt, FilePath};
use tokio::io::AsyncWriteExt;
use tokio::sync::oneshot;
use url::Url;

/// Parse `/_stall/save-as?path=/media/...&name=file.ext` on the stage loopback port.
pub fn save_as_request(url: &Url, port: u16) -> Option<(String, String)> {
    if url.scheme() != "http" || url.host_str() != Some("127.0.0.1") || url.port() != Some(port) {
        return None;
    }
    if url.path() != "/_stall/save-as" {
        return None;
    }
    let mut media_path = None;
    let mut name = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "path" => media_path = Some(value.into_owned()),
            "name" => name = Some(value.into_owned()),
            _ => {}
        }
    }
    let media_path = media_path?;
    if !media_path.starts_with("/media/") || media_path.contains("..") {
        return None;
    }
    if media_path.len() > 2048 {
        return None;
    }
    let name = sanitize_file_name(&name.unwrap_or_else(|| "download".into()));
    Some((media_path, name))
}

/// Ask the user where to save, then stream `origin + media_path` to that file.
pub async fn save_from_stage(
    app: AppHandle,
    origin: String,
    media_path: String,
    file_name: String,
) -> Result<(), String> {
    let dest = pick_save_path(&app, &file_name).await?;
    let Some(dest) = dest else {
        return Ok(());
    };
    let url = format!("{}{}", origin.trim_end_matches('/'), media_path);
    download_to_file(&url, &dest).await?;
    tracing::info!(path = %dest.display(), "saved stall file");
    Ok(())
}

async fn pick_save_path(app: &AppHandle, file_name: &str) -> Result<Option<PathBuf>, String> {
    let (tx, rx) = oneshot::channel();
    app.dialog()
        .file()
        .set_title("选择保存位置")
        .set_file_name(file_name)
        .save_file(move |path| {
            let _ = tx.send(path);
        });
    let chosen = rx.await.map_err(|_| "保存对话框已关闭".to_string())?;
    match chosen {
        Some(FilePath::Path(path)) => Ok(Some(path)),
        Some(other) => other
            .into_path()
            .map(Some)
            .map_err(|err| format!("无法解析保存路径: {err}")),
        None => Ok(None),
    }
}

async fn download_to_file(url: &str, dest: &Path) -> Result<(), String> {
    let client = reqwest::Client::new();
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|err| format!("开始下载失败: {err}"))?;
    if !response.status().is_success() {
        return Err(format!("下载失败 ({})", response.status()));
    }
    let tmp = dest.with_extension(format!(
        "{}.part",
        dest.extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("tmp")
    ));
    let mut file = tokio::fs::File::create(&tmp)
        .await
        .map_err(|err| format!("无法创建文件: {err}"))?;
    let mut written: u64 = 0;
    const CEILING: u64 = 32 * 1024 * 1024 * 1024;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| format!("下载中断: {err}"))?
    {
        written += chunk.len() as u64;
        if written > CEILING {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err("文件超过 32 GB".into());
        }
        file.write_all(&chunk)
            .await
            .map_err(|err| format!("写入失败: {err}"))?;
    }
    file.flush()
        .await
        .map_err(|err| format!("写入失败: {err}"))?;
    drop(file);
    tokio::fs::rename(&tmp, dest)
        .await
        .map_err(|err| format!("无法完成保存: {err}"))?;
    Ok(())
}

fn sanitize_file_name(raw: &str) -> String {
    let trimmed = raw.trim().trim_start_matches('.');
    let cleaned: String = trimmed
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim().trim_end_matches('.');
    if cleaned.is_empty() {
        "download".into()
    } else {
        cleaned.chars().take(180).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_save_as() {
        let url =
            Url::parse("http://127.0.0.1:4321/_stall/save-as?path=%2Fmedia%2Fa.jpg&name=a.jpg")
                .unwrap();
        let (path, name) = save_as_request(&url, 4321).unwrap();
        assert_eq!(path, "/media/a.jpg");
        assert_eq!(name, "a.jpg");
        assert!(save_as_request(&url, 9999).is_none());
    }

    #[test]
    fn rejects_traversal() {
        let url = Url::parse(
            "http://127.0.0.1:4321/_stall/save-as?path=%2Fmedia%2F..%2Fetc%2Fpasswd&name=x",
        )
        .unwrap();
        assert!(save_as_request(&url, 4321).is_none());
    }
}

use std::path::{Path, PathBuf};

use serde::Serialize;

const MAX_DEPTH: u32 = 8;
const MAX_SEEN: usize = 5_000;
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "vendor",
    "__pycache__",
    "build",
    "coverage",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    Image,
    Music,
    Video,
    Novel,
    Document,
}

impl Theme {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "image" => Self::Image,
            "music" => Self::Music,
            "video" => Self::Video,
            "novel" => Self::Novel,
            "document" => Self::Document,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Music => "music",
            Self::Video => "video",
            Self::Novel => "novel",
            Self::Document => "document",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Image => "图片",
            Self::Music => "音乐",
            Self::Video => "视频",
            Self::Novel => "小说",
            Self::Document => "文档",
        }
    }

    pub fn matches(self, ext: &str) -> bool {
        let ext = ext.to_ascii_lowercase();
        match self {
            Self::Image => matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "avif" | "bmp"),
            Self::Music => matches!(ext.as_str(), "mp3" | "flac" | "m4a" | "ogg" | "wav" | "aac"),
            Self::Video => matches!(ext.as_str(), "mp4" | "webm" | "mov" | "m4v" | "mkv"),
            Self::Novel => matches!(ext.as_str(), "md" | "markdown" | "txt"),
            Self::Document => ext == "pdf",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeCounts {
    pub image: u32,
    pub music: u32,
    pub video: u32,
    pub novel: u32,
    pub document: u32,
    pub other: u32,
}

impl ThemeCounts {
    pub fn get(&self, theme: Theme) -> u32 {
        match theme {
            Theme::Image => self.image,
            Theme::Music => self.music,
            Theme::Video => self.video,
            Theme::Novel => self.novel,
            Theme::Document => self.document,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub root: String,
    pub folder_name: String,
    pub has_index: bool,
    pub truncated: bool,
    pub counts: ThemeCounts,
}

#[derive(Debug, Clone)]
pub struct MediaFile {
    pub relative: PathBuf,
    pub title: String,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub struct Shelf {
    pub report: ScanReport,
    pub files: Vec<MediaFile>,
}

pub fn scan_folder(root: &Path) -> anyhow::Result<Shelf> {
    if !root.is_dir() {
        anyhow::bail!("这个路径不是文件夹");
    }
    let root = root.canonicalize()?;
    let mut counts = ThemeCounts {
        image: 0,
        music: 0,
        video: 0,
        novel: 0,
        document: 0,
        other: 0,
    };
    let mut files = Vec::new();
    let mut seen = 0usize;
    let mut truncated = false;
    walk(
        &root,
        &root,
        0,
        &mut seen,
        &mut truncated,
        &mut counts,
        &mut files,
    )?;
    files.sort_by(|left, right| left.title.to_lowercase().cmp(&right.title.to_lowercase()));
    let folder_name = root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "摊位".to_string());
    Ok(Shelf {
        report: ScanReport {
            root: root.display().to_string(),
            folder_name,
            has_index: root.join("index.html").is_file(),
            truncated,
            counts,
        },
        files,
    })
}

pub fn files_for_theme(files: &[MediaFile], theme: Theme) -> Vec<MediaFile> {
    files
        .iter()
        .filter(|file| {
            extension_of(&file.relative).is_some_and(|ext| theme.matches(&ext))
        })
        .cloned()
        .collect()
}

fn walk(
    root: &Path,
    dir: &Path,
    depth: u32,
    seen: &mut usize,
    truncated: &mut bool,
    counts: &mut ThemeCounts,
    files: &mut Vec<MediaFile>,
) -> anyhow::Result<()> {
    if depth > MAX_DEPTH || *truncated {
        return Ok(());
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::debug!(%err, path = %dir.display(), "skipped an unreadable directory");
            return Ok(());
        }
    };
    for entry in entries {
        if *seen >= MAX_SEEN {
            *truncated = true;
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name.contains('/') || name.contains('\\') {
            continue;
        }
        if SKIP_DIRS.iter().any(|skip| name.eq_ignore_ascii_case(skip)) {
            continue;
        }
        let path = entry.path();
        let kind = match entry.file_type() {
            Ok(kind) => kind,
            Err(_) => continue,
        };
        let canonical = match path.canonicalize() {
            Ok(path) => path,
            Err(_) => continue,
        };
        if !canonical.starts_with(root) {
            continue;
        }
        if kind.is_symlink() && canonical.is_dir() {
            walk(root, &canonical, depth + 1, seen, truncated, counts, files)?;
            continue;
        }
        if canonical.is_dir() {
            walk(root, &canonical, depth + 1, seen, truncated, counts, files)?;
            continue;
        }
        if !canonical.is_file() {
            continue;
        }
        *seen += 1;
        let Ok(relative) = canonical.strip_prefix(root) else {
            continue;
        };
        let relative = relative.to_path_buf();
        let size = canonical.metadata().map(|meta| meta.len()).unwrap_or(0);
        match theme_of(&relative) {
            Some(Theme::Image) => counts.image += 1,
            Some(Theme::Music) => counts.music += 1,
            Some(Theme::Video) => counts.video += 1,
            Some(Theme::Novel) => counts.novel += 1,
            Some(Theme::Document) => counts.document += 1,
            None => counts.other += 1,
        }
        files.push(MediaFile {
            title: display_title(&relative),
            relative,
            size,
        });
    }
    Ok(())
}

fn theme_of(relative: &Path) -> Option<Theme> {
    let ext = extension_of(relative)?;
    [Theme::Image, Theme::Music, Theme::Video, Theme::Novel, Theme::Document]
        .into_iter()
        .find(|theme| theme.matches(&ext))
}

fn extension_of(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
}

fn display_title(relative: &Path) -> String {
    let text = relative.to_string_lossy().replace('\\', "/");
    match text.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_string(),
        _ => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_a_mixed_folder_and_skips_hidden_and_dependencies() {
        let root = std::env::temp_dir().join(format!("popstall-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        std::fs::create_dir_all(root.join(".secret")).unwrap();
        std::fs::write(root.join("cover.PNG"), b"img").unwrap();
        std::fs::write(root.join("song.mp3"), b"mp3").unwrap();
        std::fs::write(root.join("clip.mp4"), b"mp4").unwrap();
        std::fs::write(root.join("chapter.md"), b"# hi").unwrap();
        std::fs::write(root.join("paper.pdf"), b"%PDF").unwrap();
        std::fs::write(root.join("notes.txt"), b"txt").unwrap();
        std::fs::write(root.join("src.rs"), b"fn").unwrap();
        std::fs::write(root.join(".env"), b"nope").unwrap();
        std::fs::write(root.join("node_modules/pkg/left.png"), b"skip").unwrap();
        std::fs::write(root.join("index.html"), b"<h1>site</h1>").unwrap();

        let shelf = scan_folder(&root).unwrap();
        assert!(shelf.report.has_index);
        assert_eq!(shelf.report.counts.image, 1);
        assert_eq!(shelf.report.counts.music, 1);
        assert_eq!(shelf.report.counts.video, 1);
        assert_eq!(shelf.report.counts.novel, 2);
        assert_eq!(shelf.report.counts.document, 1);
        assert_eq!(shelf.report.counts.other, 2);
        assert_eq!(files_for_theme(&shelf.files, Theme::Image).len(), 1);
        assert!(
            shelf
                .files
                .iter()
                .all(|file| !file.relative.components().any(|part| part.as_os_str() == "node_modules"))
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}

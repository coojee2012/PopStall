use std::path::{Component, Path, PathBuf};

use percent_encoding::percent_decode_str;

use super::MAX_TARGET_LEN;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SafePathError {
    #[error("路径跳出了托管目录")]
    Traversal,
    #[error("拒绝访问隐藏文件")]
    Dotfile,
    #[error("路径编码无效")]
    Encoding,
    #[error("文件不存在")]
    NotFound,
    #[error("托管目录不可用")]
    RootUnavailable,
    #[error("读取路径失败")]
    Io,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedPath {
    File(PathBuf),
    Directory(PathBuf),
}

/// Lexical check for a request target (`/path`, optionally with `?query`).
///
/// Decoding happens once. `..`, hidden components, backslashes, and NULs never
/// reach the filesystem.
pub fn validate_target(target: &str) -> Result<(), SafePathError> {
    if target.is_empty()
        || target.len() > MAX_TARGET_LEN
        || !target.starts_with('/')
        || target.starts_with("//")
    {
        return Err(SafePathError::Traversal);
    }
    if target
        .bytes()
        .any(|byte| matches!(byte, 0 | b'\r' | b'\n' | b'\\'))
    {
        return Err(SafePathError::Traversal);
    }
    let path = target.split(['?', '#']).next().unwrap_or(target);
    if path.contains("://") || path.contains('@') {
        return Err(SafePathError::Traversal);
    }
    normalize_request_path(path)?;
    Ok(())
}

pub fn normalize_request_path(encoded_path: &str) -> Result<PathBuf, SafePathError> {
    if encoded_path.contains('\\') || encoded_path.contains('\0') {
        return Err(SafePathError::Traversal);
    }
    let decoded = percent_decode_str(encoded_path)
        .decode_utf8()
        .map_err(|_| SafePathError::Encoding)?;
    if decoded.contains('\0') || decoded.contains('\\') || decoded.contains(':') {
        return Err(SafePathError::Traversal);
    }

    let mut relative = PathBuf::new();
    let trimmed = decoded.trim_start_matches('/');
    if trimmed.is_empty() {
        return Ok(relative);
    }
    for part in trimmed.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return Err(SafePathError::Traversal);
        }
        if part.starts_with('.') {
            return Err(SafePathError::Dotfile);
        }
        relative.push(part);
    }
    Ok(relative)
}

/// Resolve `request_path` and require every existing component to stay inside `root`.
pub fn resolve_within_root(root: &Path, request_path: &str) -> Result<ResolvedPath, SafePathError> {
    let path_only = request_path
        .split(['?', '#'])
        .next()
        .unwrap_or(request_path);
    let relative = normalize_request_path(path_only)?;
    let root = root
        .canonicalize()
        .map_err(|_| SafePathError::RootUnavailable)?;
    if !root.is_dir() {
        return Err(SafePathError::RootUnavailable);
    }

    let mut current = root.clone();
    let parts: Vec<_> = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_os_string()),
            _ => None,
        })
        .collect();

    for (index, part) in parts.iter().enumerate() {
        current.push(part);
        if !current.exists() {
            return Err(SafePathError::NotFound);
        }
        let canonical = current.canonicalize().map_err(|_| SafePathError::Io)?;
        if !canonical.starts_with(&root) {
            return Err(SafePathError::Traversal);
        }
        let is_last = index + 1 == parts.len();
        if !is_last && !canonical.is_dir() {
            return Err(SafePathError::NotFound);
        }
        current = canonical;
    }

    if current.is_dir() {
        Ok(ResolvedPath::Directory(current))
    } else if current.is_file() {
        Ok(ResolvedPath::File(current))
    } else {
        Err(SafePathError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "popstall-path-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(path.join("nested")).unwrap();
        std::fs::write(path.join("index.html"), b"home").unwrap();
        std::fs::write(path.join("nested/note.txt"), b"note").unwrap();
        path
    }

    #[test]
    fn resolves_files_inside_the_root() {
        let root = temp_root("inside");
        assert!(matches!(
            resolve_within_root(&root, "/index.html"),
            Ok(ResolvedPath::File(_))
        ));
        assert!(matches!(
            resolve_within_root(&root, "/nested/note.txt"),
            Ok(ResolvedPath::File(_))
        ));
        assert!(matches!(
            resolve_within_root(&root, "/"),
            Ok(ResolvedPath::Directory(_))
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_dotdot_and_encoded_traversal() {
        let root = temp_root("traverse");
        assert_eq!(
            resolve_within_root(&root, "/../../etc/passwd").unwrap_err(),
            SafePathError::Traversal
        );
        assert_eq!(
            resolve_within_root(&root, "/%2e%2e/%2e%2e/etc/passwd").unwrap_err(),
            SafePathError::Traversal
        );
        assert_eq!(
            resolve_within_root(&root, "/nested/%2e%2e/index.html").unwrap_err(),
            SafePathError::Traversal
        );
        assert_eq!(
            validate_target("/..%2f..%2fetc/passwd").unwrap_err(),
            SafePathError::Traversal
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_dotfiles_nulls_and_absolute_urls() {
        let root = temp_root("dot");
        assert_eq!(
            resolve_within_root(&root, "/.env").unwrap_err(),
            SafePathError::Dotfile
        );
        assert_eq!(
            resolve_within_root(&root, "/nested/.secret").unwrap_err(),
            SafePathError::Dotfile
        );
        assert!(validate_target("/%00index.html").is_err());
        assert!(validate_target("http://127.0.0.1/index.html").is_err());
        assert!(validate_target("//server/share").is_err());
        assert!(validate_target("/nested/note.txt?x=1").is_ok());
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_that_leaves_the_root() {
        let root = temp_root("symlink");
        std::os::unix::fs::symlink("/etc", root.join("escape")).unwrap();
        assert_eq!(
            resolve_within_root(&root, "/escape").unwrap_err(),
            SafePathError::Traversal
        );
        assert_eq!(
            resolve_within_root(&root, "/escape/passwd").unwrap_err(),
            SafePathError::Traversal
        );
        let _ = std::fs::remove_dir_all(root);
    }
}

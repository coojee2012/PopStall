use std::io::Write;
use std::path::Path;

use anyhow::Context;
use iroh::SecretKey;

pub fn load_or_create_secret_key(data_dir: &Path) -> anyhow::Result<SecretKey> {
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("无法创建数据目录 {}", data_dir.display()))?;
    tighten_dir(data_dir)?;

    let path = data_dir.join("endpoint.key");
    if path.exists() {
        let bytes =
            std::fs::read(&path).with_context(|| format!("无法读取身份密钥 {}", path.display()))?;
        let array: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
            anyhow::anyhow!(
                "身份密钥文件损坏（{}）。为避免悄悄更换节点身份，需要手动处理这个文件。",
                path.display()
            )
        })?;
        return Ok(SecretKey::from_bytes(&array));
    }

    let key = SecretKey::generate();
    write_secret(&path, &key.to_bytes())?;
    tracing::info!(path = %path.display(), "created endpoint identity");
    Ok(key)
}

fn write_secret(path: &Path, bytes: &[u8; 32]) -> anyhow::Result<()> {
    let tmp = path.with_extension("key.tmp");
    {
        let mut options = std::fs::OpenOptions::new();
        options.create(true).write(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&tmp)
            .with_context(|| format!("无法写入 {}", tmp.display()))?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    tighten_file(path)?;
    Ok(())
}

fn tighten_dir(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(path, permissions)?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn tighten_file(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("popstall-id-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let first = load_or_create_secret_key(&dir).unwrap();
        let second = load_or_create_secret_key(&dir).unwrap();
        assert_eq!(first.to_bytes(), second.to_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("endpoint.key"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}

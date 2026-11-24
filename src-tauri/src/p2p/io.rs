use anyhow::Context;
use tokio::io::{AsyncRead, AsyncReadExt};

use super::frame::{MAGIC_RESPONSE, content_type_is_safe};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedResponseHead {
    pub status: u16,
    pub content_type: String,
    pub content_length: Option<u64>,
    pub content_range: Option<String>,
    pub accept_ranges: bool,
}

pub async fn read_limited(
    reader: &mut (impl AsyncRead + Unpin),
    max: usize,
) -> anyhow::Result<Vec<u8>> {
    let mut scratch = vec![0u8; 1024.min(max)];
    let mut out = Vec::new();
    loop {
        let read = reader.read(&mut scratch).await.context("读取 P2P 流失败")?;
        if read == 0 {
            break;
        }
        if out.len() + read > max {
            anyhow::bail!("请求超出长度限制");
        }
        out.extend_from_slice(&scratch[..read]);
    }
    Ok(out)
}

pub async fn read_exact(
    reader: &mut (impl AsyncRead + Unpin),
    len: usize,
) -> anyhow::Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    reader
        .read_exact(&mut buf)
        .await
        .context("P2P 流提前结束")?;
    Ok(buf)
}

pub async fn read_response_head(
    reader: &mut (impl AsyncRead + Unpin),
) -> anyhow::Result<DecodedResponseHead> {
    let magic = read_exact(reader, 4).await?;
    if magic.as_slice() != MAGIC_RESPONSE {
        anyhow::bail!("远端响应头无效");
    }
    let status_bytes = read_exact(reader, 2).await?;
    let status = u16::from_be_bytes([status_bytes[0], status_bytes[1]]);
    let len_bytes = read_exact(reader, 2).await?;
    let len = u16::from_be_bytes([len_bytes[0], len_bytes[1]]) as usize;
    if !(100..600).contains(&status) || len > 256 {
        anyhow::bail!("远端响应头字段无效");
    }
    let content_type =
        String::from_utf8(read_exact(reader, len).await?).context("内容类型不是 UTF-8")?;
    if !content_type_is_safe(&content_type) {
        anyhow::bail!("内容类型包含控制字符");
    }
    let flags = read_exact(reader, 1).await?[0];
    let content_length = if (flags & 1) == 1 {
        let bytes = read_exact(reader, 8).await?;
        Some(u64::from_be_bytes(bytes.try_into().unwrap()))
    } else {
        None
    };
    let content_range = if (flags & 2) == 2 {
        let range_len = u16::from_be_bytes(read_exact(reader, 2).await?.try_into().unwrap()) as usize;
        if range_len > 256 {
            anyhow::bail!("远端范围字段无效");
        }
        let range = String::from_utf8(read_exact(reader, range_len).await?).context("范围字段不是 UTF-8")?;
        if !content_type_is_safe(&range) {
            anyhow::bail!("范围字段包含控制字符");
        }
        Some(range)
    } else {
        None
    };
    Ok(DecodedResponseHead {
        status,
        content_type,
        content_length,
        content_range,
        accept_ranges: (flags & 4) == 4,
    })
}

use std::io::Cursor;
use std::time::Duration;

use anyhow::Context;
use tokio::io::{AsyncRead, AsyncReadExt};

use super::frame::{
    MAGIC_RESPONSE, MAGIC_RESPONSE_PROOF, PSR3_FLAG_MERKLE_ROOT, PSR3_FLAG_RANGE_PROOF,
    ResponseHeadProof, content_type_is_safe, decode_response_head_proof,
};
use super::integrity::{
    DIGEST_LEN, IntegrityDigest, MAX_MERKLE_PROOF_BYTES, MerkleProof, MerkleRoot,
};
use super::traffic::Traffic;

// frame.rs keeps this private; mirror its bound so oversized tokens are rejected
// before io.rs allocates them.
const MAX_HEADER_TOKEN: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedResponseHead {
    pub status: u16,
    pub content_type: String,
    pub content_length: Option<u64>,
    pub content_range: Option<String>,
    pub accept_ranges: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedResponseHeadV3 {
    pub status: u16,
    pub content_type: String,
    pub content_length: Option<u64>,
    pub content_range: Option<String>,
    pub accept_ranges: bool,
    pub digest: IntegrityDigest,
    pub merkle_root: Option<MerkleRoot>,
    pub range_proof: Option<MerkleProof>,
}

pub type DecodedResponseHeadProof = DecodedResponseHeadV3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodedResponseHeadAny {
    Plain(DecodedResponseHead),
    Proof(DecodedResponseHeadV3),
}

impl DecodedResponseHeadAny {
    #[must_use]
    pub fn status(&self) -> u16 {
        match self {
            Self::Plain(head) => head.status,
            Self::Proof(head) => head.status,
        }
    }

    #[must_use]
    pub fn content_type(&self) -> &str {
        match self {
            Self::Plain(head) => &head.content_type,
            Self::Proof(head) => &head.content_type,
        }
    }

    #[must_use]
    pub fn content_length(&self) -> Option<u64> {
        match self {
            Self::Plain(head) => head.content_length,
            Self::Proof(head) => head.content_length,
        }
    }

    #[must_use]
    pub fn content_range(&self) -> Option<&str> {
        match self {
            Self::Plain(head) => head.content_range.as_deref(),
            Self::Proof(head) => head.content_range.as_deref(),
        }
    }

    #[must_use]
    pub fn accept_ranges(&self) -> bool {
        match self {
            Self::Plain(head) => head.accept_ranges,
            Self::Proof(head) => head.accept_ranges,
        }
    }

    #[must_use]
    pub fn digest(&self) -> Option<&IntegrityDigest> {
        match self {
            Self::Plain(_) => None,
            Self::Proof(head) => Some(&head.digest),
        }
    }

    #[must_use]
    pub fn merkle_root(&self) -> Option<&MerkleRoot> {
        match self {
            Self::Plain(_) => None,
            Self::Proof(head) => head.merkle_root.as_ref(),
        }
    }

    #[must_use]
    pub fn range_proof(&self) -> Option<&MerkleProof> {
        match self {
            Self::Plain(_) => None,
            Self::Proof(head) => head.range_proof.as_ref(),
        }
    }
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
    let len = usize::from(u16::from_be_bytes([len_bytes[0], len_bytes[1]]));
    if !(100..600).contains(&status) || len > MAX_HEADER_TOKEN {
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
        Some(u64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    } else {
        None
    };
    let content_range = if (flags & 2) == 2 {
        let range_bytes = read_exact(reader, 2).await?;
        let range_len = usize::from(u16::from_be_bytes([range_bytes[0], range_bytes[1]]));
        if range_len > MAX_HEADER_TOKEN {
            anyhow::bail!("远端范围字段无效");
        }
        let range = String::from_utf8(read_exact(reader, range_len).await?)
            .context("范围字段不是 UTF-8")?;
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

pub async fn read_response_head_with_timeout(
    reader: &mut (impl AsyncRead + Unpin),
    duration: Duration,
) -> anyhow::Result<DecodedResponseHead> {
    tokio::time::timeout(duration, read_response_head(reader))
        .await
        .context("读取 P2P 响应头超时")?
}

pub async fn read_response_head_any(
    reader: &mut (impl AsyncRead + Unpin),
) -> anyhow::Result<DecodedResponseHeadAny> {
    let magic = read_exact(reader, 4).await?;
    let mut framed = Cursor::new(magic.clone()).chain(reader);
    if magic.as_slice() == MAGIC_RESPONSE {
        Ok(DecodedResponseHeadAny::Plain(
            read_response_head(&mut framed).await?,
        ))
    } else if magic.as_slice() == MAGIC_RESPONSE_PROOF {
        Ok(DecodedResponseHeadAny::Proof(
            decode_response_head_v3(&mut framed).await?,
        ))
    } else {
        anyhow::bail!("远端响应头无效")
    }
}

pub fn verify_response_body(
    head: &DecodedResponseHeadAny,
    body: &[u8],
    traffic: &Traffic,
) -> anyhow::Result<()> {
    let Some(digest) = head.digest() else {
        return Ok(());
    };
    traffic.record_digest_check();
    let valid = if let (Some(root), Some(proof)) = (head.merkle_root(), head.range_proof()) {
        root.verify_range(proof, body).is_ok()
    } else {
        digest.verify_bytes(body)
    };
    if !valid {
        traffic.record_digest_failure();
        anyhow::bail!("内容摘要校验失败");
    }
    Ok(())
}

pub async fn decode_response_head_v3(
    reader: &mut (impl AsyncRead + Unpin),
) -> anyhow::Result<DecodedResponseHeadProof> {
    let mut framed = Vec::new();

    let magic = read_exact(reader, 4).await?;
    if magic.as_slice() != MAGIC_RESPONSE_PROOF {
        anyhow::bail!("远端 PSR3 响应头无效");
    }
    framed.extend_from_slice(&magic);

    let status_bytes = read_exact(reader, 2).await?;
    let status = u16::from_be_bytes([status_bytes[0], status_bytes[1]]);
    if !(100..600).contains(&status) {
        anyhow::bail!("远端响应头状态无效");
    }
    framed.extend_from_slice(&status_bytes);

    let len_bytes = read_exact(reader, 2).await?;
    let content_type_len = usize::from(u16::from_be_bytes([len_bytes[0], len_bytes[1]]));
    if content_type_len > MAX_HEADER_TOKEN {
        anyhow::bail!("远端内容类型字段过长");
    }
    framed.extend_from_slice(&len_bytes);
    let content_type = String::from_utf8(read_exact(reader, content_type_len).await?)
        .context("内容类型不是 UTF-8")?;
    if !content_type_is_safe(&content_type) {
        anyhow::bail!("内容类型包含控制字符");
    }
    framed.extend_from_slice(content_type.as_bytes());

    let flags_bytes = read_exact(reader, 1).await?;
    let flags = flags_bytes[0];
    if flags & !7 != 0 {
        anyhow::bail!("远端响应头标志无效");
    }
    framed.extend_from_slice(&flags_bytes);

    if flags & 1 == 1 {
        let bytes = read_exact(reader, 8).await?;
        framed.extend_from_slice(&bytes);
    }

    if flags & 2 == 2 {
        let range_len_bytes = read_exact(reader, 2).await?;
        let range_len = usize::from(u16::from_be_bytes([range_len_bytes[0], range_len_bytes[1]]));
        if range_len > MAX_HEADER_TOKEN {
            anyhow::bail!("远端范围字段过长");
        }
        framed.extend_from_slice(&range_len_bytes);
        let content_range = String::from_utf8(read_exact(reader, range_len).await?)
            .context("范围字段不是 UTF-8")?;
        if !content_type_is_safe(&content_range) {
            anyhow::bail!("范围字段包含控制字符");
        }
        framed.extend_from_slice(content_range.as_bytes());
    }

    let digest_bytes = read_exact(reader, DIGEST_LEN).await?;
    framed.extend_from_slice(&digest_bytes);

    let proof_flags_bytes = read_exact(reader, 1).await?;
    let proof_flags = proof_flags_bytes[0];
    if proof_flags & !(PSR3_FLAG_MERKLE_ROOT | PSR3_FLAG_RANGE_PROOF) != 0 {
        anyhow::bail!("远端证明标志无效");
    }
    framed.extend_from_slice(&proof_flags_bytes);

    if proof_flags & PSR3_FLAG_MERKLE_ROOT != 0 {
        let root_bytes = read_exact(reader, DIGEST_LEN).await?;
        framed.extend_from_slice(&root_bytes);
    }

    if proof_flags & PSR3_FLAG_RANGE_PROOF != 0 {
        if proof_flags & PSR3_FLAG_MERKLE_ROOT == 0 {
            anyhow::bail!("范围证明缺少 Merkle 根");
        }
        let proof_len_bytes = read_exact(reader, 4).await?;
        let proof_len = usize::try_from(u32::from_be_bytes([
            proof_len_bytes[0],
            proof_len_bytes[1],
            proof_len_bytes[2],
            proof_len_bytes[3],
        ]))
        .context("证明长度无效")?;
        if proof_len > MAX_MERKLE_PROOF_BYTES {
            anyhow::bail!("Merkle 证明过长");
        }
        framed.extend_from_slice(&proof_len_bytes);
        let proof_bytes = read_exact(reader, proof_len).await?;
        framed.extend_from_slice(&proof_bytes);
    }

    let ResponseHeadProof {
        head,
        digest,
        merkle_root,
        range_proof,
    } = decode_response_head_proof(&framed).context("PSR3 响应头校验失败")?;

    Ok(DecodedResponseHeadV3 {
        status: head.status,
        content_type: head.content_type,
        content_length: head.content_length,
        content_range: head.content_range,
        accept_ranges: head.accept_ranges,
        digest,
        merkle_root,
        range_proof,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::p2p::frame::{ResponseHead, encode_response_head, encode_response_head_proof};
    use tokio::io::{AsyncWriteExt, duplex};

    fn psr3_fixture() -> ResponseHeadProof {
        let head = ResponseHead {
            status: 206,
            content_type: "video/mp4".into(),
            content_length: Some(4),
            content_range: Some("bytes 0-3/10".into()),
            accept_ranges: true,
        };
        let proof = MerkleProof {
            total_len: 10,
            start: 1,
            end: 4,
            prefix: vec![0u8],
            suffix: vec![0u8; 6],
            siblings: Vec::new(),
        };
        ResponseHeadProof::new(head, IntegrityDigest::hash_bytes(b"full file")).with_merkle(
            MerkleRoot::from_slice(&[7u8; DIGEST_LEN]).unwrap(),
            Some(proof),
        )
    }

    async fn assert_psr3_rejected(bytes: &[u8]) {
        let (mut writer, mut reader) = duplex(bytes.len().max(64));
        writer.write_all(bytes).await.unwrap();
        drop(writer);
        assert!(decode_response_head_v3(&mut reader).await.is_err());
    }

    #[tokio::test]
    async fn psr3_decodes_incrementally_and_leaves_body_unread() {
        let framed = psr3_fixture();
        let head_bytes = encode_response_head_proof(&framed).unwrap();
        let body = b"body-after-head";
        let (mut writer, mut reader) = duplex(1024);
        writer.write_all(&head_bytes).await.unwrap();
        writer.write_all(body).await.unwrap();

        let decoded = decode_response_head_v3(&mut reader).await.unwrap();
        let expected = DecodedResponseHeadV3 {
            status: framed.head.status,
            content_type: framed.head.content_type.clone(),
            content_length: framed.head.content_length,
            content_range: framed.head.content_range.clone(),
            accept_ranges: framed.head.accept_ranges,
            digest: framed.digest,
            merkle_root: framed.merkle_root.clone(),
            range_proof: framed.range_proof.clone(),
        };
        assert_eq!(decoded, expected);

        let mut remaining = vec![0u8; body.len()];
        reader.read_exact(&mut remaining).await.unwrap();
        assert_eq!(remaining.as_slice(), &body[..]);
    }

    #[tokio::test]
    async fn psr3_rejects_psr2_truncated_oversized_and_tampered_frames() {
        let framed = psr3_fixture();
        let head = framed.head.clone();
        let bytes = encode_response_head_proof(&framed).unwrap();

        let psr2 = encode_response_head(&head).unwrap();
        assert_psr3_rejected(&psr2).await;
        assert_psr3_rejected(&bytes[..bytes.len() - 1]).await;

        let mut oversized_token = Vec::new();
        oversized_token.extend_from_slice(MAGIC_RESPONSE_PROOF);
        oversized_token.extend_from_slice(&200u16.to_be_bytes());
        oversized_token.extend_from_slice(&257u16.to_be_bytes());
        assert_psr3_rejected(&oversized_token).await;

        let proof_len = framed
            .range_proof
            .as_ref()
            .unwrap()
            .to_bytes()
            .unwrap()
            .len();
        let proof_len_at = bytes.len() - proof_len - 4;
        let mut oversized_proof = bytes.clone();
        oversized_proof[proof_len_at..proof_len_at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_psr3_rejected(&oversized_proof).await;

        let digest_at = encode_response_head(&head).unwrap().len();
        let mut tampered_digest = bytes.clone();
        tampered_digest.remove(digest_at + DIGEST_LEN - 1);
        assert_psr3_rejected(&tampered_digest).await;

        let proof_at = bytes.len() - proof_len;
        let mut tampered_proof = bytes;
        tampered_proof[proof_at] ^= 0xff;
        assert_psr3_rejected(&tampered_proof).await;
    }

    #[tokio::test]
    async fn read_response_head_with_timeout_decodes_psr2() {
        let head = ResponseHead::new(200, "text/plain");
        let bytes = encode_response_head(&head).unwrap();
        let (mut writer, mut reader) = duplex(128);
        writer.write_all(&bytes).await.unwrap();

        let decoded = read_response_head_with_timeout(&mut reader, Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(
            decoded,
            DecodedResponseHead {
                status: head.status,
                content_type: head.content_type,
                content_length: None,
                content_range: None,
                accept_ranges: false,
            }
        );
    }

    #[tokio::test]
    async fn read_response_head_with_timeout_errors_on_stalled_stream() {
        let (_writer, mut reader) = duplex(64);
        let started = tokio::time::Instant::now();
        let err = read_response_head_with_timeout(&mut reader, Duration::from_millis(20))
            .await
            .expect_err("stalled head read must time out");
        assert!(format!("{err:#}").contains("超时"));
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}

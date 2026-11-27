use crate::p2p::integrity::{
    DIGEST_LEN, IntegrityDigest, IntegrityError, MAX_MERKLE_PROOF_BYTES, MerkleProof, MerkleRoot,
};
use crate::security::{MAX_REQUEST_BODY, MAX_TARGET_LEN, SafePathError, validate_target};

// allow: SIZE_OK — PSR2 and PSR3 codecs stay together to keep the wire format auditable.
pub const MAGIC_REQUEST: &[u8; 4] = b"PSH2";
pub const MAGIC_REQUEST_PASS: &[u8; 4] = b"PSH3";
pub const MAGIC_RESPONSE: &[u8; 4] = b"PSR2";
pub const MAGIC_RESPONSE_PROOF: &[u8; 4] = b"PSR3";
pub const PSR3_FLAG_MERKLE_ROOT: u8 = 1;
pub const PSR3_FLAG_RANGE_PROOF: u8 = 2;
const MAX_HEADER_TOKEN: usize = 256;
const MAX_PASS: usize = 192;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Head,
    Post,
    Put,
    Patch,
    Delete,
    Options,
}

impl Method {
    pub fn from_http(method: &http::Method) -> Option<Self> {
        if method == http::Method::GET {
            Some(Self::Get)
        } else if method == http::Method::HEAD {
            Some(Self::Head)
        } else if method == http::Method::POST {
            Some(Self::Post)
        } else if method == http::Method::PUT {
            Some(Self::Put)
        } else if method == http::Method::PATCH {
            Some(Self::Patch)
        } else if method == http::Method::DELETE {
            Some(Self::Delete)
        } else if method == http::Method::OPTIONS {
            Some(Self::Options)
        } else {
            None
        }
    }

    pub fn to_http(self) -> http::Method {
        match self {
            Self::Get => http::Method::GET,
            Self::Head => http::Method::HEAD,
            Self::Post => http::Method::POST,
            Self::Put => http::Method::PUT,
            Self::Patch => http::Method::PATCH,
            Self::Delete => http::Method::DELETE,
            Self::Options => http::Method::OPTIONS,
        }
    }

    fn code(self) -> u8 {
        match self {
            Self::Get => 1,
            Self::Head => 2,
            Self::Post => 3,
            Self::Put => 4,
            Self::Patch => 5,
            Self::Delete => 6,
            Self::Options => 7,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            1 => Self::Get,
            2 => Self::Head,
            3 => Self::Post,
            4 => Self::Put,
            5 => Self::Patch,
            6 => Self::Delete,
            7 => Self::Options,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestFrame {
    pub method: Method,
    pub target: String,
    pub range: Option<String>,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
    pub pass: String,
}

impl RequestFrame {
    pub fn get(target: impl Into<String>) -> Self {
        Self {
            method: Method::Get,
            target: target.into(),
            range: None,
            content_type: None,
            body: Vec::new(),
            pass: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseHead {
    pub status: u16,
    pub content_type: String,
    pub content_length: Option<u64>,
    pub content_range: Option<String>,
    pub accept_ranges: bool,
}

impl ResponseHead {
    pub fn new(status: u16, content_type: impl Into<String>) -> Self {
        Self {
            status,
            content_type: content_type.into(),
            content_length: None,
            content_range: None,
            accept_ranges: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseHeadProof {
    pub head: ResponseHead,
    pub digest: IntegrityDigest,
    pub merkle_root: Option<MerkleRoot>,
    pub range_proof: Option<MerkleProof>,
}

impl ResponseHeadProof {
    pub fn new(head: ResponseHead, digest: IntegrityDigest) -> Self {
        Self {
            head,
            digest,
            merkle_root: None,
            range_proof: None,
        }
    }

    pub fn with_merkle(mut self, root: MerkleRoot, proof: Option<MerkleProof>) -> Self {
        self.merkle_root = Some(root);
        self.range_proof = proof;
        self
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error(transparent)]
    Target(#[from] SafePathError),
    #[error("帧格式无效")]
    Malformed,
    #[error("帧超出长度限制")]
    TooLong,
    #[error(transparent)]
    Proof(#[from] IntegrityError),
}

pub fn encode_request(frame: &RequestFrame) -> Result<Vec<u8>, FrameError> {
    validate_target(&frame.target)?;
    let target = frame.target.as_bytes();
    if target.len() > MAX_TARGET_LEN || frame.body.len() > MAX_REQUEST_BODY {
        return Err(FrameError::TooLong);
    }
    let range = checked_token(frame.range.as_deref())?;
    let content_type = checked_token(frame.content_type.as_deref())?;
    let pass = frame.pass.trim();
    if !pass.is_empty() && (!token_is_safe(pass) || pass.len() > MAX_PASS) {
        return Err(FrameError::Malformed);
    }
    let mut out = Vec::with_capacity(
        13 + target.len() + range.len() + content_type.len() + pass.len() + frame.body.len(),
    );
    out.extend_from_slice(if pass.is_empty() {
        MAGIC_REQUEST
    } else {
        MAGIC_REQUEST_PASS
    });
    out.push(frame.method.code());
    push_u16_bytes(&mut out, target);
    push_u16_bytes(&mut out, range);
    push_u16_bytes(&mut out, content_type);
    if !pass.is_empty() {
        push_u16_bytes(&mut out, pass.as_bytes());
    }
    out.extend_from_slice(&(frame.body.len() as u32).to_be_bytes());
    out.extend_from_slice(&frame.body);
    Ok(out)
}

pub fn decode_request(buf: &[u8]) -> Result<RequestFrame, FrameError> {
    if buf.len() < 13 {
        return Err(FrameError::Malformed);
    }
    let with_pass = if &buf[0..4] == MAGIC_REQUEST {
        false
    } else if &buf[0..4] == MAGIC_REQUEST_PASS {
        true
    } else {
        return Err(FrameError::Malformed);
    };
    let mut cursor = 4;
    let method = Method::from_code(read_u8(buf, &mut cursor)?).ok_or(FrameError::Malformed)?;
    let target = read_str(buf, &mut cursor, MAX_TARGET_LEN)?;
    validate_target(&target)?;
    let range = optional_token(read_str(buf, &mut cursor, MAX_HEADER_TOKEN)?)?;
    let content_type = optional_token(read_str(buf, &mut cursor, MAX_HEADER_TOKEN)?)?;
    let pass = if with_pass {
        read_str(buf, &mut cursor, MAX_PASS)?
    } else {
        String::new()
    };
    if !pass.is_empty() && !token_is_safe(&pass) {
        return Err(FrameError::Malformed);
    }
    if cursor + 4 > buf.len() {
        return Err(FrameError::Malformed);
    }
    let body_len = u32::from_be_bytes(buf[cursor..cursor + 4].try_into().unwrap()) as usize;
    cursor += 4;
    if body_len > MAX_REQUEST_BODY || cursor + body_len != buf.len() {
        return Err(FrameError::Malformed);
    }
    Ok(RequestFrame {
        method,
        target,
        range,
        content_type,
        body: buf[cursor..].to_vec(),
        pass,
    })
}

pub fn encode_response_head(head: &ResponseHead) -> Result<Vec<u8>, FrameError> {
    if !(100..600).contains(&head.status) || !token_is_safe(&head.content_type) {
        return Err(FrameError::Malformed);
    }
    let content_type = head.content_type.as_bytes();
    if content_type.len() > MAX_HEADER_TOKEN {
        return Err(FrameError::TooLong);
    }
    let range = match head.content_range.as_deref() {
        Some(value) if token_is_safe(value) && value.len() <= MAX_HEADER_TOKEN => value.as_bytes(),
        Some(_) => return Err(FrameError::Malformed),
        None => &[],
    };
    let mut flags = 0u8;
    if head.content_length.is_some() {
        flags |= 1;
    }
    if !range.is_empty() {
        flags |= 2;
    }
    if head.accept_ranges {
        flags |= 4;
    }
    let mut out = Vec::with_capacity(16 + content_type.len() + range.len());
    out.extend_from_slice(MAGIC_RESPONSE);
    out.extend_from_slice(&head.status.to_be_bytes());
    push_u16_bytes(&mut out, content_type);
    out.push(flags);
    if let Some(length) = head.content_length {
        out.extend_from_slice(&length.to_be_bytes());
    }
    if !range.is_empty() {
        push_u16_bytes(&mut out, range);
    }
    Ok(out)
}

pub fn encode_response_head_proof(head: &ResponseHeadProof) -> Result<Vec<u8>, FrameError> {
    let mut out = encode_response_head(&head.head)?;
    out[0..4].copy_from_slice(MAGIC_RESPONSE_PROOF);
    out.extend_from_slice(head.digest.as_bytes());

    let mut proof_flags = 0u8;
    if head.merkle_root.is_some() {
        proof_flags |= PSR3_FLAG_MERKLE_ROOT;
    }
    if head.range_proof.is_some() {
        proof_flags |= PSR3_FLAG_RANGE_PROOF;
    }
    if proof_flags & PSR3_FLAG_RANGE_PROOF != 0 && proof_flags & PSR3_FLAG_MERKLE_ROOT == 0 {
        return Err(FrameError::Malformed);
    }
    out.push(proof_flags);

    if let Some(root) = head.merkle_root.as_ref() {
        out.extend_from_slice(root.as_bytes());
    }
    if let Some(proof) = head.range_proof.as_ref() {
        let encoded = proof.to_bytes()?;
        if encoded.len() > MAX_MERKLE_PROOF_BYTES {
            return Err(FrameError::TooLong);
        }
        let len = u32::try_from(encoded.len()).map_err(|_| FrameError::TooLong)?;
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&encoded);
    }
    Ok(out)
}

pub fn decode_response_head_proof(buf: &[u8]) -> Result<ResponseHeadProof, FrameError> {
    if buf.len() < 4 || &buf[0..4] != MAGIC_RESPONSE_PROOF {
        return Err(FrameError::Malformed);
    }
    let mut cursor = 4;
    let status = read_u16(buf, &mut cursor)?;
    if !(100..600).contains(&status) {
        return Err(FrameError::Malformed);
    }
    let content_type = read_str(buf, &mut cursor, MAX_HEADER_TOKEN)?;
    if !token_is_safe(&content_type) {
        return Err(FrameError::Malformed);
    }
    let flags = read_u8(buf, &mut cursor)?;
    if flags & !7 != 0 {
        return Err(FrameError::Malformed);
    }
    let content_length = if flags & 1 == 1 {
        let bytes: [u8; 8] = buf
            .get(cursor..cursor + 8)
            .ok_or(FrameError::Malformed)?
            .try_into()
            .map_err(|_| FrameError::Malformed)?;
        cursor += 8;
        Some(u64::from_be_bytes(bytes))
    } else {
        None
    };
    let content_range = if flags & 2 == 2 {
        optional_token(read_str(buf, &mut cursor, MAX_HEADER_TOKEN)?)?
    } else {
        None
    };
    let head = ResponseHead {
        status,
        content_type,
        content_length,
        content_range,
        accept_ranges: flags & 4 == 4,
    };

    let digest_bytes: [u8; DIGEST_LEN] = buf
        .get(cursor..cursor + DIGEST_LEN)
        .ok_or(FrameError::Malformed)?
        .try_into()
        .map_err(|_| FrameError::Malformed)?;
    cursor += DIGEST_LEN;
    let digest = IntegrityDigest::from_slice(&digest_bytes)?;

    let proof_flags = read_u8(buf, &mut cursor)?;
    if proof_flags & !(PSR3_FLAG_MERKLE_ROOT | PSR3_FLAG_RANGE_PROOF) != 0 {
        return Err(FrameError::Malformed);
    }
    let merkle_root = if proof_flags & PSR3_FLAG_MERKLE_ROOT != 0 {
        let root_bytes: [u8; DIGEST_LEN] = buf
            .get(cursor..cursor + DIGEST_LEN)
            .ok_or(FrameError::Malformed)?
            .try_into()
            .map_err(|_| FrameError::Malformed)?;
        cursor += DIGEST_LEN;
        Some(MerkleRoot::from_slice(&root_bytes)?)
    } else {
        None
    };
    let range_proof = if proof_flags & PSR3_FLAG_RANGE_PROOF != 0 {
        if merkle_root.is_none() {
            return Err(FrameError::Malformed);
        }
        let len = usize::try_from(read_u32(buf, &mut cursor)?).map_err(|_| FrameError::TooLong)?;
        if len > MAX_MERKLE_PROOF_BYTES {
            return Err(FrameError::TooLong);
        }
        let end = cursor.checked_add(len).ok_or(FrameError::TooLong)?;
        if end > buf.len() {
            return Err(FrameError::Malformed);
        }
        let proof = MerkleProof::from_bytes(&buf[cursor..end])?;
        cursor = end;
        Some(proof)
    } else {
        None
    };
    if cursor != buf.len() {
        return Err(FrameError::Malformed);
    }
    Ok(ResponseHeadProof {
        head,
        digest,
        merkle_root,
        range_proof,
    })
}

pub fn content_type_is_safe(value: &str) -> bool {
    token_is_safe(value)
}

fn checked_token(value: Option<&str>) -> Result<&[u8], FrameError> {
    match value {
        None => Ok(&[]),
        Some(value) if token_is_safe(value) && value.len() <= MAX_HEADER_TOKEN => {
            Ok(value.as_bytes())
        }
        Some(_) => Err(FrameError::Malformed),
    }
}

fn optional_token(value: String) -> Result<Option<String>, FrameError> {
    if value.is_empty() {
        return Ok(None);
    }
    if token_is_safe(&value) {
        Ok(Some(value))
    } else {
        Err(FrameError::Malformed)
    }
}

fn token_is_safe(value: &str) -> bool {
    !value.is_empty() && !value.chars().any(|ch| ch.is_control() || ch == '\u{007f}')
}

fn push_u16_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn read_u8(buf: &[u8], cursor: &mut usize) -> Result<u8, FrameError> {
    let byte = *buf.get(*cursor).ok_or(FrameError::Malformed)?;
    *cursor += 1;
    Ok(byte)
}

fn read_u16(buf: &[u8], cursor: &mut usize) -> Result<u16, FrameError> {
    let bytes: [u8; 2] = buf
        .get(*cursor..*cursor + 2)
        .ok_or(FrameError::Malformed)?
        .try_into()
        .map_err(|_| FrameError::Malformed)?;
    *cursor += 2;
    Ok(u16::from_be_bytes(bytes))
}

fn read_u32(buf: &[u8], cursor: &mut usize) -> Result<u32, FrameError> {
    let bytes: [u8; 4] = buf
        .get(*cursor..*cursor + 4)
        .ok_or(FrameError::Malformed)?
        .try_into()
        .map_err(|_| FrameError::Malformed)?;
    *cursor += 4;
    Ok(u32::from_be_bytes(bytes))
}

fn read_str(buf: &[u8], cursor: &mut usize, max: usize) -> Result<String, FrameError> {
    if *cursor + 2 > buf.len() {
        return Err(FrameError::Malformed);
    }
    let len = u16::from_be_bytes([buf[*cursor], buf[*cursor + 1]]) as usize;
    *cursor += 2;
    if len > max || *cursor + len > buf.len() {
        return Err(FrameError::Malformed);
    }
    let text =
        std::str::from_utf8(&buf[*cursor..*cursor + len]).map_err(|_| FrameError::Malformed)?;
    *cursor += len;
    Ok(text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::p2p::integrity::MERKLE_CHUNK_SIZE;

    #[test]
    fn request_frames_round_trip() {
        let frame = RequestFrame {
            method: Method::Post,
            target: "/nested/note.txt?x=1".into(),
            range: Some("bytes=0-3".into()),
            content_type: Some("text/plain".into()),
            body: b"ping".to_vec(),
            pass: String::new(),
        };
        let bytes = encode_request(&frame).unwrap();
        assert_eq!(&bytes[0..4], MAGIC_REQUEST);
        assert_eq!(decode_request(&bytes).unwrap(), frame);

        let mut locked = frame.clone();
        locked.pass = "夜里的口令".into();
        let bytes = encode_request(&locked).unwrap();
        assert_eq!(&bytes[0..4], MAGIC_REQUEST_PASS);
        assert_eq!(decode_request(&bytes).unwrap(), locked);
    }

    #[test]
    fn request_frames_reject_traversal() {
        let frame = RequestFrame::get("/%2e%2e/secret");
        assert!(encode_request(&frame).is_err());
    }

    #[test]
    fn response_head_keeps_status_and_range() {
        let head = ResponseHead {
            status: 206,
            content_type: "video/mp4".into(),
            content_length: Some(4),
            content_range: Some("bytes 0-3/9".into()),
            accept_ranges: true,
        };
        let bytes = encode_response_head(&head).unwrap();
        assert_eq!(&bytes[0..4], MAGIC_RESPONSE);
        assert_eq!(u16::from_be_bytes([bytes[4], bytes[5]]), 206);
    }

    fn psr3_fixture() -> ResponseHeadProof {
        let head = ResponseHead {
            status: 206,
            content_type: "video/mp4".into(),
            content_length: Some(4),
            content_range: Some("bytes 0-3/9".into()),
            accept_ranges: true,
        };
        let chunk = u64::try_from(MERKLE_CHUNK_SIZE).unwrap();
        let proof = MerkleProof {
            total_len: chunk + 10,
            start: 1,
            end: chunk - 1,
            prefix: vec![1u8],
            suffix: vec![2u8],
            siblings: vec![[9u8; DIGEST_LEN]],
        };
        ResponseHeadProof::new(head, IntegrityDigest::hash_bytes(b"full file")).with_merkle(
            MerkleRoot::from_slice(&[7u8; DIGEST_LEN]).unwrap(),
            Some(proof),
        )
    }

    #[test]
    fn psr3_round_trip_preserves_digest_root_and_proof() {
        let framed = psr3_fixture();
        let bytes = encode_response_head_proof(&framed).unwrap();

        assert_eq!(&bytes[0..4], MAGIC_RESPONSE_PROOF);
        assert_eq!(decode_response_head_proof(&bytes).unwrap(), framed);
    }

    #[test]
    fn psr2_encoding_is_unchanged_and_psr3_decoder_rejects_it() {
        let head = ResponseHead {
            status: 206,
            content_type: "video/mp4".into(),
            content_length: Some(4),
            content_range: Some("bytes 0-3/9".into()),
            accept_ranges: true,
        };
        let mut expected = b"PSR2".to_vec();
        expected.extend_from_slice(&206u16.to_be_bytes());
        expected.extend_from_slice(&9u16.to_be_bytes());
        expected.extend_from_slice(b"video/mp4");
        expected.push(7);
        expected.extend_from_slice(&4u64.to_be_bytes());
        expected.extend_from_slice(&11u16.to_be_bytes());
        expected.extend_from_slice(b"bytes 0-3/9");

        assert_eq!(encode_response_head(&head).unwrap(), expected);
        assert!(decode_response_head_proof(&expected).is_err());
    }

    #[test]
    fn psr3_rejects_oversized_and_truncated_proof() {
        let framed = psr3_fixture();
        let bytes = encode_response_head_proof(&framed).unwrap();
        let proof_len = framed
            .range_proof
            .as_ref()
            .unwrap()
            .to_bytes()
            .unwrap()
            .len();
        let length_at = bytes.len() - proof_len - 4;

        let mut oversized = bytes.clone();
        oversized[length_at..length_at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(matches!(
            decode_response_head_proof(&oversized),
            Err(FrameError::TooLong)
        ));

        let mut truncated = bytes;
        truncated.pop();
        assert!(decode_response_head_proof(&truncated).is_err());
    }

    #[test]
    fn psr3_rejects_proof_without_merkle_root() {
        let mut framed = psr3_fixture();
        framed.merkle_root = None;
        assert!(encode_response_head_proof(&framed).is_err());
    }
}

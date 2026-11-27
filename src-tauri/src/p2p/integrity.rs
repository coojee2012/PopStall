use std::collections::{HashMap, VecDeque};
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use blake3::Hasher;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::security::{ResolvedPath, SafePathError, resolve_within_root};

// allow: SIZE_OK — U12 keeps the full-file digest cache and the chunked Merkle primitives in one integrity module per plan.
pub const DIGEST_LEN: usize = 32;
const HASH_CHUNK: usize = 64 * 1024;
pub const MERKLE_CHUNK_SIZE: usize = 1024 * 1024;
const MERKLE_CHUNK_SIZE_U64: u64 = MERKLE_CHUNK_SIZE as u64;
pub const MAX_MERKLE_PROOF_CHUNKS: usize = 8;
pub const MAX_MERKLE_DEPTH: usize = 64;
pub const MAX_MERKLE_EDGE_BYTES: usize = 2 * MERKLE_CHUNK_SIZE;
pub const MAX_MERKLE_SIBLINGS: usize = MAX_MERKLE_PROOF_CHUNKS * MAX_MERKLE_DEPTH;
pub const MAX_MERKLE_PROOF_BYTES: usize =
    40 + MAX_MERKLE_EDGE_BYTES + MAX_MERKLE_SIBLINGS * DIGEST_LEN;
const MAX_CACHED_MERKLE_TREES: usize = 8;
const MERKLE_PROOF_MAGIC: &[u8; 4] = b"MPR1";

#[derive(Debug, thiserror::Error)]
pub enum IntegrityError {
    #[error(transparent)]
    Path(#[from] SafePathError),
    #[error("读取文件失败: {0}")]
    Io(#[from] std::io::Error),
    #[error("摘要长度无效")]
    InvalidDigestLength,
    #[error("Merkle 根长度无效")]
    InvalidMerkleRootLength,
    #[error("Merkle 范围无效")]
    InvalidMerkleRange,
    #[error("Merkle 证明格式无效")]
    MalformedMerkleProof,
    #[error("Merkle 证明过长")]
    MerkleProofTooLong,
    #[error("Merkle 证明截断")]
    MerkleProofTruncated,
    #[error("Merkle 证明包含过多分块")]
    TooManyMerkleChunks,
    #[error("Merkle 边缘数据过长")]
    MerkleEdgeTooLong,
    #[error("Merkle 证明哈希数量无效")]
    InvalidMerkleSiblings,
    #[error("Merkle 证明校验失败")]
    MerkleMismatch,
}

/// A full-file BLAKE3 digest. Range proofs stay in U12; this is deliberately
/// only a whole-resource hash.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct IntegrityDigest([u8; DIGEST_LEN]);

impl IntegrityDigest {
    pub fn from_slice(bytes: &[u8]) -> Result<Self, IntegrityError> {
        let bytes: [u8; DIGEST_LEN] = bytes
            .try_into()
            .map_err(|_| IntegrityError::InvalidDigestLength)?;
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8; DIGEST_LEN] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(DIGEST_LEN * 2);
        for byte in self.0 {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    }

    pub fn hash_bytes(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    /// Streams the file in bounded chunks so a movie is never loaded into
    /// memory just to hash it.
    pub async fn hash_file(path: &Path) -> Result<Self, IntegrityError> {
        let mut file = tokio::fs::File::open(path).await?;
        let mut hasher = Hasher::new();
        let mut buf = vec![0u8; HASH_CHUNK];
        loop {
            let read = file.read(&mut buf).await?;
            if read == 0 {
                break;
            }
            hasher.update(&buf[..read]);
        }
        Ok(Self(*hasher.finalize().as_bytes()))
    }

    pub fn verify_bytes(&self, bytes: &[u8]) -> bool {
        Self::hash_bytes(bytes) == *self
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct MerkleRoot([u8; DIGEST_LEN]);

impl MerkleRoot {
    pub fn from_slice(bytes: &[u8]) -> Result<Self, IntegrityError> {
        let bytes: [u8; DIGEST_LEN] = bytes
            .try_into()
            .map_err(|_| IntegrityError::InvalidMerkleRootLength)?;
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8; DIGEST_LEN] {
        &self.0
    }

    /// Verifies one `Content-Range` slice using only the slice, the edge bytes
    /// in the proof, and the sibling hashes; the file is never reopened here.
    pub fn verify_range(&self, proof: &MerkleProof, slice: &[u8]) -> Result<(), IntegrityError> {
        let shape = proof_shape(proof.total_len, proof.start, proof.end)?;
        if proof.prefix.len() != shape.prefix_len || proof.suffix.len() != shape.suffix_len {
            return Err(IntegrityError::MalformedMerkleProof);
        }
        if proof.siblings.len() != shape.chunk_count * shape.depth {
            return Err(IntegrityError::InvalidMerkleSiblings);
        }
        let expected_len = usize::try_from(proof.end - proof.start)
            .map_err(|_| IntegrityError::MerkleProofTooLong)?;
        if slice.len() != expected_len {
            return Err(IntegrityError::MerkleMismatch);
        }

        let mut cursor = 0usize;
        for index in shape.first..=shape.last {
            let chunk_start = index.saturating_mul(MERKLE_CHUNK_SIZE_U64);
            let chunk_end = index
                .saturating_add(1)
                .saturating_mul(MERKLE_CHUNK_SIZE_U64)
                .min(proof.total_len);
            let covered_start = chunk_start.max(proof.start);
            let covered_end = chunk_end.min(proof.end);
            let take = usize::try_from(covered_end.saturating_sub(covered_start))
                .map_err(|_| IntegrityError::MerkleProofTooLong)?;
            let next = cursor
                .checked_add(take)
                .ok_or(IntegrityError::MerkleProofTooLong)?;
            if next > slice.len() {
                return Err(IntegrityError::MerkleMismatch);
            }

            let capacity = usize::try_from(chunk_end - chunk_start)
                .map_err(|_| IntegrityError::MerkleProofTooLong)?;
            let mut chunk = Vec::with_capacity(capacity);
            if index == shape.first {
                chunk.extend_from_slice(&proof.prefix);
            }
            chunk.extend_from_slice(&slice[cursor..next]);
            if index == shape.last {
                chunk.extend_from_slice(&proof.suffix);
            }
            cursor = next;

            let mut current = *blake3::hash(&chunk).as_bytes();
            let base = usize::try_from(index - shape.first)
                .map_err(|_| IntegrityError::MerkleProofTooLong)?
                * shape.depth;
            for level in 0..shape.depth {
                let sibling = proof.siblings[base + level];
                current = if ((index >> level) & 1) == 0 {
                    node_hash(&current, &sibling)
                } else {
                    node_hash(&sibling, &current)
                };
            }
            if current != self.0 {
                return Err(IntegrityError::MerkleMismatch);
            }
        }
        if cursor != slice.len() {
            return Err(IntegrityError::MerkleMismatch);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MerkleProof {
    pub total_len: u64,
    pub start: u64,
    pub end: u64,
    /// Bytes from the start of the first chunk up to `start`.
    pub prefix: Vec<u8>,
    /// Bytes from `end` to the end of the last chunk.
    pub suffix: Vec<u8>,
    /// Sibling hashes in leaf order, bottom-up, for every covered chunk.
    pub siblings: Vec<[u8; DIGEST_LEN]>,
}

impl MerkleProof {
    pub fn to_bytes(&self) -> Result<Vec<u8>, IntegrityError> {
        let shape = proof_shape(self.total_len, self.start, self.end)?;
        if self.prefix.len() != shape.prefix_len || self.suffix.len() != shape.suffix_len {
            return Err(IntegrityError::MalformedMerkleProof);
        }
        if self.siblings.len() != shape.chunk_count * shape.depth {
            return Err(IntegrityError::InvalidMerkleSiblings);
        }
        let prefix_len =
            u32::try_from(self.prefix.len()).map_err(|_| IntegrityError::MerkleEdgeTooLong)?;
        let suffix_len =
            u32::try_from(self.suffix.len()).map_err(|_| IntegrityError::MerkleEdgeTooLong)?;
        let sibling_count = u32::try_from(self.siblings.len())
            .map_err(|_| IntegrityError::InvalidMerkleSiblings)?;

        let mut out = Vec::with_capacity(
            40 + self.prefix.len() + self.suffix.len() + self.siblings.len() * DIGEST_LEN,
        );
        out.extend_from_slice(MERKLE_PROOF_MAGIC);
        out.extend_from_slice(&self.total_len.to_be_bytes());
        out.extend_from_slice(&self.start.to_be_bytes());
        out.extend_from_slice(&self.end.to_be_bytes());
        out.extend_from_slice(&prefix_len.to_be_bytes());
        out.extend_from_slice(&suffix_len.to_be_bytes());
        out.extend_from_slice(&sibling_count.to_be_bytes());
        out.extend_from_slice(&self.prefix);
        out.extend_from_slice(&self.suffix);
        for sibling in &self.siblings {
            out.extend_from_slice(sibling);
        }
        Ok(out)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IntegrityError> {
        if bytes.len() < 40 {
            return Err(IntegrityError::MerkleProofTruncated);
        }
        if &bytes[0..4] != MERKLE_PROOF_MAGIC {
            return Err(IntegrityError::MalformedMerkleProof);
        }
        let total_len = read_u64(bytes, 4)?;
        let start = read_u64(bytes, 12)?;
        let end = read_u64(bytes, 20)?;
        let prefix_len =
            usize::try_from(read_u32(bytes, 28)?).map_err(|_| IntegrityError::MerkleEdgeTooLong)?;
        let suffix_len =
            usize::try_from(read_u32(bytes, 32)?).map_err(|_| IntegrityError::MerkleEdgeTooLong)?;
        let sibling_count = usize::try_from(read_u32(bytes, 36)?)
            .map_err(|_| IntegrityError::InvalidMerkleSiblings)?;
        let edge_len = prefix_len
            .checked_add(suffix_len)
            .ok_or(IntegrityError::MerkleEdgeTooLong)?;
        if prefix_len > MAX_MERKLE_EDGE_BYTES
            || suffix_len > MAX_MERKLE_EDGE_BYTES
            || edge_len > MAX_MERKLE_EDGE_BYTES
        {
            return Err(IntegrityError::MerkleEdgeTooLong);
        }
        if sibling_count > MAX_MERKLE_SIBLINGS {
            return Err(IntegrityError::InvalidMerkleSiblings);
        }
        let siblings_bytes = sibling_count
            .checked_mul(DIGEST_LEN)
            .ok_or(IntegrityError::MerkleProofTooLong)?;
        let expected = 40usize
            .checked_add(prefix_len)
            .and_then(|len| len.checked_add(suffix_len))
            .and_then(|len| len.checked_add(siblings_bytes))
            .ok_or(IntegrityError::MerkleProofTooLong)?;
        if bytes.len() < expected {
            return Err(IntegrityError::MerkleProofTruncated);
        }
        if bytes.len() > expected {
            return Err(IntegrityError::MerkleProofTooLong);
        }

        let shape = proof_shape(total_len, start, end)?;
        if prefix_len != shape.prefix_len || suffix_len != shape.suffix_len {
            return Err(IntegrityError::MalformedMerkleProof);
        }
        if sibling_count != shape.chunk_count * shape.depth {
            return Err(IntegrityError::InvalidMerkleSiblings);
        }
        let prefix_end = 40 + prefix_len;
        let suffix_end = prefix_end + suffix_len;
        let prefix = bytes[40..prefix_end].to_vec();
        let suffix = bytes[prefix_end..suffix_end].to_vec();
        let mut siblings = Vec::with_capacity(sibling_count);
        let mut cursor = suffix_end;
        for _ in 0..sibling_count {
            let hash: [u8; DIGEST_LEN] = bytes[cursor..cursor + DIGEST_LEN]
                .try_into()
                .map_err(|_| IntegrityError::MerkleProofTruncated)?;
            siblings.push(hash);
            cursor += DIGEST_LEN;
        }
        Ok(Self {
            total_len,
            start,
            end,
            prefix,
            suffix,
            siblings,
        })
    }
}

struct ProofShape {
    first: u64,
    last: u64,
    prefix_len: usize,
    suffix_len: usize,
    chunk_count: usize,
    depth: usize,
}

fn proof_shape(total_len: u64, start: u64, end: u64) -> Result<ProofShape, IntegrityError> {
    if total_len == 0 || start >= end || end > total_len {
        return Err(IntegrityError::InvalidMerkleRange);
    }
    let first = start / MERKLE_CHUNK_SIZE_U64;
    let last = (end - 1) / MERKLE_CHUNK_SIZE_U64;
    let chunk_count_u64 = last
        .checked_sub(first)
        .and_then(|count| count.checked_add(1))
        .ok_or(IntegrityError::InvalidMerkleRange)?;
    let chunk_count =
        usize::try_from(chunk_count_u64).map_err(|_| IntegrityError::TooManyMerkleChunks)?;
    if chunk_count > MAX_MERKLE_PROOF_CHUNKS {
        return Err(IntegrityError::TooManyMerkleChunks);
    }
    let chunk_start = first.saturating_mul(MERKLE_CHUNK_SIZE_U64);
    let chunk_end = last
        .saturating_add(1)
        .saturating_mul(MERKLE_CHUNK_SIZE_U64)
        .min(total_len);
    let prefix_len =
        usize::try_from(start - chunk_start).map_err(|_| IntegrityError::MerkleEdgeTooLong)?;
    let suffix_len =
        usize::try_from(chunk_end - end).map_err(|_| IntegrityError::MerkleEdgeTooLong)?;
    let edge_len = prefix_len
        .checked_add(suffix_len)
        .ok_or(IntegrityError::MerkleEdgeTooLong)?;
    if prefix_len > MERKLE_CHUNK_SIZE
        || suffix_len > MERKLE_CHUNK_SIZE
        || edge_len > MAX_MERKLE_EDGE_BYTES
    {
        return Err(IntegrityError::MerkleEdgeTooLong);
    }

    let leaf_count = total_len.div_ceil(MERKLE_CHUNK_SIZE_U64);
    let size = leaf_count.next_power_of_two();
    let depth = usize::try_from(size.trailing_zeros())
        .map_err(|_| IntegrityError::InvalidMerkleSiblings)?;
    if depth > MAX_MERKLE_DEPTH {
        return Err(IntegrityError::InvalidMerkleSiblings);
    }
    Ok(ProofShape {
        first,
        last,
        prefix_len,
        suffix_len,
        chunk_count,
        depth,
    })
}

fn node_hash(left: &[u8; DIGEST_LEN], right: &[u8; DIGEST_LEN]) -> [u8; DIGEST_LEN] {
    let mut hasher = Hasher::new();
    hasher.update(b"popstall/merkle/node/v1");
    hasher.update(left);
    hasher.update(right);
    *hasher.finalize().as_bytes()
}

fn empty_leaf_hash() -> [u8; DIGEST_LEN] {
    *blake3::hash(b"popstall/merkle/empty-leaf/v1").as_bytes()
}

fn empty_tree_hash() -> [u8; DIGEST_LEN] {
    *blake3::hash(b"popstall/merkle/empty-tree/v1").as_bytes()
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, IntegrityError> {
    let slice = bytes
        .get(offset..offset + 8)
        .ok_or(IntegrityError::MerkleProofTruncated)?;
    let array: [u8; 8] = slice
        .try_into()
        .map_err(|_| IntegrityError::MerkleProofTruncated)?;
    Ok(u64::from_be_bytes(array))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, IntegrityError> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or(IntegrityError::MerkleProofTruncated)?;
    let array: [u8; 4] = slice
        .try_into()
        .map_err(|_| IntegrityError::MerkleProofTruncated)?;
    Ok(u32::from_be_bytes(array))
}

/// A cached 1 MiB-chunk Merkle tree for one file. Build once, then issue many
/// bounded range proofs without rereading the whole file.
#[derive(Debug, Clone)]
pub struct MerkleTree {
    path: PathBuf,
    total_len: u64,
    levels: Vec<Vec<[u8; DIGEST_LEN]>>,
    root: MerkleRoot,
}

impl MerkleTree {
    pub async fn from_file(path: &Path) -> Result<Self, IntegrityError> {
        let mut file = tokio::fs::File::open(path).await?;
        let mut buf = vec![0u8; MERKLE_CHUNK_SIZE];
        let mut leaves = Vec::new();
        let mut total_len = 0u64;
        loop {
            let read = read_full_chunk(&mut file, &mut buf).await?;
            if read == 0 {
                break;
            }
            let read_u64 = u64::try_from(read).map_err(|_| IntegrityError::MerkleProofTooLong)?;
            total_len = total_len
                .checked_add(read_u64)
                .ok_or(IntegrityError::MerkleProofTooLong)?;
            leaves.push(*blake3::hash(&buf[..read]).as_bytes());
        }
        Self::from_leaves(path.to_path_buf(), total_len, leaves)
    }

    pub fn root(&self) -> MerkleRoot {
        self.root
    }

    pub async fn proof(&self, start: u64, end: u64) -> Result<MerkleProof, IntegrityError> {
        let shape = proof_shape(self.total_len, start, end)?;
        let metadata = tokio::fs::metadata(&self.path).await?;
        if metadata.len() != self.total_len {
            return Err(IntegrityError::MerkleMismatch);
        }
        let prefix = read_exact_range(
            &self.path,
            shape.first * MERKLE_CHUNK_SIZE_U64,
            shape.prefix_len,
        )
        .await?;
        let suffix = read_exact_range(&self.path, end, shape.suffix_len).await?;
        let mut siblings = Vec::with_capacity(shape.chunk_count * shape.depth);
        for index in shape.first..=shape.last {
            for level in 0..shape.depth {
                let node_index = (index >> level) ^ 1;
                let node_index = usize::try_from(node_index)
                    .map_err(|_| IntegrityError::InvalidMerkleSiblings)?;
                let level_nodes = self
                    .levels
                    .get(level)
                    .ok_or(IntegrityError::MerkleMismatch)?;
                let sibling = level_nodes
                    .get(node_index)
                    .copied()
                    .ok_or(IntegrityError::MerkleMismatch)?;
                siblings.push(sibling);
            }
        }
        Ok(MerkleProof {
            total_len: self.total_len,
            start,
            end,
            prefix,
            suffix,
            siblings,
        })
    }

    fn from_leaves(
        path: PathBuf,
        total_len: u64,
        mut leaves: Vec<[u8; DIGEST_LEN]>,
    ) -> Result<Self, IntegrityError> {
        let leaf_count = total_len.div_ceil(MERKLE_CHUNK_SIZE_U64);
        let size = if leaf_count == 0 {
            1
        } else {
            leaf_count.next_power_of_two()
        };
        let size = usize::try_from(size).map_err(|_| IntegrityError::MerkleProofTooLong)?;
        leaves.resize(size, empty_leaf_hash());
        let mut levels = vec![leaves];
        while levels.last().is_some_and(|level| level.len() > 1) {
            let previous = levels.last().ok_or(IntegrityError::MerkleMismatch)?;
            let mut next = Vec::with_capacity(previous.len() / 2);
            for pair in previous.chunks_exact(2) {
                match pair {
                    [left, right] => next.push(node_hash(left, right)),
                    _ => return Err(IntegrityError::MerkleMismatch),
                }
            }
            levels.push(next);
        }
        let root = if total_len == 0 {
            MerkleRoot(empty_tree_hash())
        } else {
            let top = levels.last().ok_or(IntegrityError::MerkleMismatch)?;
            let hash = top.first().copied().ok_or(IntegrityError::MerkleMismatch)?;
            MerkleRoot(hash)
        };
        Ok(Self {
            path,
            total_len,
            levels,
            root,
        })
    }
}

async fn read_full_chunk(
    file: &mut tokio::fs::File,
    buf: &mut [u8],
) -> Result<usize, IntegrityError> {
    let mut filled = 0usize;
    while filled < buf.len() {
        let read = file.read(&mut buf[filled..]).await?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    Ok(filled)
}

async fn read_exact_range(path: &Path, offset: u64, len: usize) -> Result<Vec<u8>, IntegrityError> {
    if len == 0 {
        return Ok(Vec::new());
    }
    let mut file = tokio::fs::File::open(path).await?;
    file.seek(SeekFrom::Start(offset)).await?;
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf).await?;
    Ok(buf)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct IntegrityKey {
    path: PathBuf,
    len: u64,
    mtime: SystemTime,
}

struct MerkleTreeCache {
    entries: HashMap<IntegrityKey, Arc<MerkleTree>>,
    order: VecDeque<IntegrityKey>,
}

impl MerkleTreeCache {
    fn get(&self, key: &IntegrityKey) -> Option<Arc<MerkleTree>> {
        self.entries.get(key).cloned()
    }

    fn insert(&mut self, key: IntegrityKey, tree: Arc<MerkleTree>) {
        if self.entries.contains_key(&key) {
            self.order.retain(|existing| existing != &key);
        } else if self.entries.len() >= MAX_CACHED_MERKLE_TREES {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(key.clone(), Arc::clone(&tree));
        self.order.push_back(key);
    }
}

/// Caches full-file digests by the exact metadata tuple the host can put on
/// the wire later. Every public lookup resolves through `resolve_within_root`
/// before touching the filesystem.
#[derive(Clone)]
pub struct IntegrityIndex {
    root: Arc<PathBuf>,
    cache: Arc<Mutex<HashMap<IntegrityKey, IntegrityDigest>>>,
    merkle_cache: Arc<Mutex<MerkleTreeCache>>,
}

impl IntegrityIndex {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root: Arc::new(root),
            cache: Arc::new(Mutex::new(HashMap::new())),
            merkle_cache: Arc::new(Mutex::new(MerkleTreeCache {
                entries: HashMap::new(),
                order: VecDeque::new(),
            })),
        }
    }

    pub async fn digest(&self, target: &str) -> Result<IntegrityDigest, IntegrityError> {
        let path = match resolve_within_root(self.root.as_path(), target)? {
            ResolvedPath::File(path) => path,
            ResolvedPath::Directory(_) => return Err(SafePathError::NotFound.into()),
        };
        let metadata = tokio::fs::metadata(&path).await?;
        self.digest_for(&path, metadata.len(), metadata.modified()?)
            .await
    }

    /// Lookup/compute for an already resolved path. Callers must have applied
    /// the repo path resolver; `digest` does that for request targets.
    pub async fn digest_for(
        &self,
        path: &Path,
        len: u64,
        mtime: SystemTime,
    ) -> Result<IntegrityDigest, IntegrityError> {
        let key = IntegrityKey {
            path: path.to_path_buf(),
            len,
            mtime,
        };
        if let Some(found) = self
            .cache
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .get(&key)
        {
            return Ok(*found);
        }
        let digest = IntegrityDigest::hash_file(path).await?;
        self.cache
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .insert(key, digest);
        Ok(digest)
    }

    /// Resolves a request target and returns its cached 1 MiB-chunk Merkle tree.
    ///
    /// # Errors
    ///
    /// Returns [`IntegrityError`] when the target cannot be resolved or the
    /// file cannot be read.
    pub async fn merkle_tree(&self, target: &str) -> Result<Arc<MerkleTree>, IntegrityError> {
        let path = match resolve_within_root(self.root.as_path(), target)? {
            ResolvedPath::File(path) => path,
            ResolvedPath::Directory(_) => return Err(SafePathError::NotFound.into()),
        };
        let metadata = tokio::fs::metadata(&path).await?;
        self.merkle_tree_for(&path, metadata.len(), metadata.modified()?)
            .await
    }

    /// Lookup/compute for an already resolved path; `merkle_tree` does that for request targets.
    async fn merkle_tree_for(
        &self,
        path: &Path,
        len: u64,
        mtime: SystemTime,
    ) -> Result<Arc<MerkleTree>, IntegrityError> {
        let key = IntegrityKey {
            path: path.to_path_buf(),
            len,
            mtime,
        };
        if let Some(found) = self
            .merkle_cache
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .get(&key)
        {
            return Ok(found);
        }
        let tree = Arc::new(MerkleTree::from_file(path).await?);
        self.merkle_cache
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .insert(key, Arc::clone(&tree));
        Ok(tree)
    }

    /// Resolves a request target and returns a bounded Merkle proof for its range.
    ///
    /// # Errors
    ///
    /// Returns [`IntegrityError`] when resolution, tree construction, or proof
    /// generation fails.
    pub async fn merkle_proof(
        &self,
        target: &str,
        start: u64,
        end: u64,
    ) -> Result<MerkleProof, IntegrityError> {
        let tree = self.merkle_tree(target).await?;
        tree.proof(start, end).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static TEMP_SEQ: AtomicU64 = AtomicU64::new(1);

    const ABC_DIGEST: &str = "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85";

    fn temp_root(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "popstall-integrity-{name}-{}-{}",
            std::process::id(),
            TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn known_vector_and_byte_verification() {
        let digest = IntegrityDigest::hash_bytes(b"abc");
        assert_eq!(digest.to_hex(), ABC_DIGEST);
        assert_eq!(
            IntegrityDigest::from_slice(digest.as_bytes()).unwrap(),
            digest
        );
        assert!(digest.verify_bytes(b"abc"));
        assert!(!digest.verify_bytes(b"abd"));
        assert!(IntegrityDigest::from_slice(b"short").is_err());
    }

    #[tokio::test]
    async fn resolves_and_hashes_files_inside_root() {
        let root = temp_root("resolve");
        std::fs::write(root.join("note.txt"), b"abc").unwrap();
        let index = IntegrityIndex::new(root.clone());
        assert_eq!(
            index.digest("/note.txt").await.unwrap().to_hex(),
            ABC_DIGEST
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn rejects_traversal_through_the_existing_resolver() {
        let root = temp_root("traversal");
        std::fs::write(root.join("note.txt"), b"abc").unwrap();
        let index = IntegrityIndex::new(root.clone());
        assert!(matches!(
            index.digest("/../outside.txt").await,
            Err(IntegrityError::Path(SafePathError::Traversal))
        ));
        assert!(matches!(
            index.digest("/%2e%2e/outside.txt").await,
            Err(IntegrityError::Path(SafePathError::Traversal))
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn detects_tampering_when_metadata_changes() {
        let root = temp_root("tamper");
        let path = root.join("note.txt");
        std::fs::write(&path, b"aaaa").unwrap();
        let index = IntegrityIndex::new(root.clone());
        let first = index
            .digest_for(&path, 4, SystemTime::UNIX_EPOCH)
            .await
            .unwrap();

        std::fs::write(&path, b"bbbb").unwrap();
        let second = index
            .digest_for(
                &path,
                4,
                SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1),
            )
            .await
            .unwrap();

        assert_ne!(first, second);
        assert!(first.verify_bytes(b"aaaa"));
        assert!(second.verify_bytes(b"bbbb"));
        assert!(!first.verify_bytes(b"bbbb"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn reuses_cached_digest_for_the_same_metadata() {
        let root = temp_root("cache");
        let path = root.join("note.txt");
        std::fs::write(&path, b"aaaa").unwrap();
        let index = IntegrityIndex::new(root.clone());
        let first = index
            .digest_for(&path, 4, SystemTime::UNIX_EPOCH)
            .await
            .unwrap();

        // Same key, different bytes: a cache hit must return the original digest.
        std::fs::write(&path, b"bbbb").unwrap();
        let cached = index
            .digest_for(&path, 4, SystemTime::UNIX_EPOCH)
            .await
            .unwrap();

        assert_eq!(first, cached);
        let _ = std::fs::remove_dir_all(root);
    }

    fn deterministic_bytes(len: usize) -> Vec<u8> {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            out.push((state & 0xff).to_le_bytes()[0]);
        }
        out
    }

    async fn merkle_fixture(len: usize) -> (PathBuf, PathBuf, MerkleTree, Vec<u8>) {
        let root = temp_root("merkle");
        let path = root.join("clip.bin");
        let bytes = deterministic_bytes(len);
        std::fs::write(&path, &bytes).unwrap();
        let tree = MerkleTree::from_file(&path).await.unwrap();
        (root, path, tree, bytes)
    }

    #[tokio::test]
    async fn merkle_root_is_deterministic_and_detects_changed_bytes() {
        let (root, path, tree, mut bytes) = merkle_fixture(2 * MERKLE_CHUNK_SIZE + 31).await;
        let again = MerkleTree::from_file(&path).await.unwrap();
        assert_eq!(tree.root(), again.root());

        bytes[0] ^= 1;
        std::fs::write(&path, &bytes).unwrap();
        let changed = MerkleTree::from_file(&path).await.unwrap();
        assert_ne!(tree.root(), changed.root());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn merkle_random_ranges_verify_with_bounded_proofs() {
        let (root, _path, tree, bytes) = merkle_fixture(3 * MERKLE_CHUNK_SIZE + 1234).await;
        let total = u64::try_from(bytes.len()).unwrap();
        let chunk = u64::try_from(MERKLE_CHUNK_SIZE).unwrap();
        let mut state = 0x1234_5678_9abc_defu64;

        for _ in 0..8 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let start = state % (total - chunk - 1);
            let len = (chunk / 2 + (state >> 17) % (chunk / 2)).min(total - start);
            let end = start + len;
            let proof = tree.proof(start, end).await.unwrap();
            let slice = &bytes[usize::try_from(start).unwrap()..usize::try_from(end).unwrap()];

            tree.root().verify_range(&proof, slice).unwrap();
            assert!(proof.siblings.len() <= MAX_MERKLE_SIBLINGS);
            assert!(proof.prefix.len() + proof.suffix.len() <= MAX_MERKLE_EDGE_BYTES);

            let mut tampered = slice.to_vec();
            tampered[0] ^= 1;
            assert!(tree.root().verify_range(&proof, &tampered).is_err());
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn merkle_partial_first_and_last_chunks_verify() {
        let (root, _path, tree, bytes) = merkle_fixture(2 * MERKLE_CHUNK_SIZE + 77).await;
        let total = u64::try_from(bytes.len()).unwrap();
        let chunk = u64::try_from(MERKLE_CHUNK_SIZE).unwrap();
        let ranges = [
            (1u64, chunk - 1),
            (chunk - 3, chunk + 5),
            (total - 41, total),
        ];

        for (start, end) in ranges {
            let proof = tree.proof(start, end).await.unwrap();
            let slice = &bytes[usize::try_from(start).unwrap()..usize::try_from(end).unwrap()];
            tree.root().verify_range(&proof, slice).unwrap();
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn merkle_tampered_sibling_hash_fails() {
        let (root, _path, tree, bytes) = merkle_fixture(2 * MERKLE_CHUNK_SIZE + 9).await;
        let chunk = u64::try_from(MERKLE_CHUNK_SIZE).unwrap();
        let start = chunk - 7;
        let end = chunk + 11;
        let mut proof = tree.proof(start, end).await.unwrap();
        let slice = &bytes[usize::try_from(start).unwrap()..usize::try_from(end).unwrap()];
        proof.siblings[0][0] ^= 1;

        assert!(tree.root().verify_range(&proof, slice).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn merkle_wrong_root_fails() {
        let (root, _path, tree, bytes) = merkle_fixture(MERKLE_CHUNK_SIZE + 13).await;
        let proof = tree.proof(3, 19).await.unwrap();
        let wrong = MerkleRoot::from_slice(&[0u8; DIGEST_LEN]).unwrap();

        assert!(wrong.verify_range(&proof, &bytes[3..19]).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn merkle_proof_bytes_round_trip_and_reject_bad_lengths() {
        let (root, _path, tree, _bytes) = merkle_fixture(MERKLE_CHUNK_SIZE + 13).await;
        let proof = tree.proof(3, 19).await.unwrap();
        let encoded = proof.to_bytes().unwrap();
        assert_eq!(MerkleProof::from_bytes(&encoded).unwrap(), proof);

        let mut truncated = encoded.clone();
        truncated.pop();
        assert!(MerkleProof::from_bytes(&truncated).is_err());

        let mut oversized = encoded;
        oversized[36..40].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(MerkleProof::from_bytes(&oversized).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn merkle_index_resolves_targets_and_rejects_traversal() {
        let (root, _path, direct, _bytes) = merkle_fixture(MERKLE_CHUNK_SIZE + 17).await;
        let index = IntegrityIndex::new(root.clone());

        let tree = index.merkle_tree("/clip.bin").await.unwrap();
        assert_eq!(tree.root(), direct.root());
        assert!(matches!(
            index.merkle_tree("/../outside.bin").await,
            Err(IntegrityError::Path(SafePathError::Traversal))
        ));
        assert!(matches!(
            index.merkle_proof("/%2e%2e/outside.bin", 0, 1).await,
            Err(IntegrityError::Path(SafePathError::Traversal))
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn merkle_index_reuses_cached_tree_for_the_same_metadata() {
        let (root, path, direct, bytes) = merkle_fixture(2 * MERKLE_CHUNK_SIZE + 31).await;
        let index = IntegrityIndex::new(root.clone());
        let len = u64::try_from(bytes.len()).unwrap();
        let mtime = SystemTime::UNIX_EPOCH;

        let first = index.merkle_tree_for(&path, len, mtime).await.unwrap();
        let cached = index.merkle_tree_for(&path, len, mtime).await.unwrap();
        assert!(Arc::ptr_eq(&first, &cached));
        assert_eq!(first.root(), direct.root());

        let mut changed = bytes.clone();
        changed[0] ^= 1;
        std::fs::write(&path, &changed).unwrap();
        let stale = index.merkle_tree_for(&path, len, mtime).await.unwrap();
        assert!(Arc::ptr_eq(&first, &stale));
        assert_eq!(stale.root(), direct.root());

        let rebuilt = index
            .merkle_tree_for(&path, len, mtime + std::time::Duration::from_secs(1))
            .await
            .unwrap();
        assert!(!Arc::ptr_eq(&first, &rebuilt));
        assert_ne!(rebuilt.root(), direct.root());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn merkle_index_cross_chunk_proof_verifies_against_cached_root() {
        let (root, _path, _direct, bytes) = merkle_fixture(2 * MERKLE_CHUNK_SIZE + 31).await;
        let index = IntegrityIndex::new(root.clone());
        let tree = index.merkle_tree("/clip.bin").await.unwrap();
        let chunk = u64::try_from(MERKLE_CHUNK_SIZE).unwrap();
        let start = chunk - 7;
        let end = chunk + 11;

        let proof = index.merkle_proof("/clip.bin", start, end).await.unwrap();
        let slice = &bytes[usize::try_from(start).unwrap()..usize::try_from(end).unwrap()];
        tree.root().verify_range(&proof, slice).unwrap();

        assert_eq!(proof.total_len, u64::try_from(bytes.len()).unwrap());
        assert_eq!(proof.start, start);
        assert_eq!(proof.end, end);
        assert!(!proof.prefix.is_empty());
        assert!(!proof.suffix.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }
}

mod codec;
mod framing;
mod handshake;
mod record;
mod stream;

pub use framing::{
    helper_handshake_accept, helper_handshake_accept_peer, helper_handshake_request,
};
pub use stream::{GuestReader, GuestWriter, SealedStream, open_guest_stream};

#[cfg(test)]
pub use framing::{HANDSHAKE_TIMEOUT, read_handshake_message, write_handshake_message};

use iroh::{PublicKey, SecretKey, Signature};
use thiserror::Error;
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};
use zeroize::Zeroize;

const DOMAIN: &[u8] = b"popstall/helper-e2e/v1";
const KEY_INFO: &[u8] = b"popstall/helper-e2e/keys/v1";
const HANDSHAKE_MAGIC: &[u8; 4] = b"PSH1";
const ZERO_PUB: [u8; KEY_LEN] = [0u8; KEY_LEN];
const SESSION_LEN: usize = 16;
const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;
const SIGNATURE_LEN: usize = 64;
const RECORD_HEADER_LEN: usize = 9;
const DIR_GUEST_TO_HOST: u8 = 1;
const DIR_HOST_TO_GUEST: u8 = 2;

/// AEAD record type carrying one opaque stream chunk.
pub const RECORD_DATA: u8 = 1;
/// AEAD record type marking the end of a sealed stream.
pub const RECORD_CLOSE: u8 = 2;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SecureError {
    #[error("handshake message is malformed")]
    InvalidMessage,
    #[error("handshake identity does not match the trusted path")]
    IdentityMismatch,
    #[error("handshake signature is invalid")]
    Signature,
    #[error("session key derivation failed")]
    KeyDerivation,
    #[error("AEAD encryption failed")]
    Encrypt,
    #[error("AEAD decryption failed")]
    Decrypt,
    #[error("record counter is exhausted")]
    CounterExhausted,
    #[error("record counter {actual} does not match expected {expected}")]
    Counter { expected: u64, actual: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    EndToEnd,
    HelperBridge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionId([u8; SESSION_LEN]);

impl SessionId {
    /// Creates a random 16-byte session identifier from the node identity RNG.
    #[must_use]
    pub fn generate() -> Self {
        let mut id = [0u8; SESSION_LEN];
        id.copy_from_slice(&SecretKey::generate().to_bytes()[..SESSION_LEN]);
        Self(id)
    }

    #[must_use]
    pub fn from_bytes(bytes: [u8; SESSION_LEN]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8; SESSION_LEN] {
        &self.0
    }

    #[must_use]
    pub fn to_hex(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(SESSION_LEN * 2);
        for byte in self.0 {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathTrust {
    EndToEnd {
        session: SessionId,
        guest: PublicKey,
        host: PublicKey,
    },
    HelperBridge {
        session: SessionId,
        guest: PublicKey,
        host: PublicKey,
        helper: PublicKey,
    },
}

impl PathTrust {
    #[must_use]
    pub fn kind(&self) -> PathKind {
        match self {
            Self::EndToEnd { .. } => PathKind::EndToEnd,
            Self::HelperBridge { .. } => PathKind::HelperBridge,
        }
    }

    #[must_use]
    pub fn session(&self) -> &SessionId {
        match self {
            Self::EndToEnd { session, .. } | Self::HelperBridge { session, .. } => session,
        }
    }

    #[must_use]
    pub fn guest(&self) -> &PublicKey {
        match self {
            Self::EndToEnd { guest, .. } | Self::HelperBridge { guest, .. } => guest,
        }
    }

    #[must_use]
    pub fn host(&self) -> &PublicKey {
        match self {
            Self::EndToEnd { host, .. } | Self::HelperBridge { host, .. } => host,
        }
    }

    #[must_use]
    pub fn helper(&self) -> Option<&PublicKey> {
        match self {
            Self::EndToEnd { .. } => None,
            Self::HelperBridge { helper, .. } => Some(helper),
        }
    }
}

#[derive(Clone)]
pub struct EphemeralSecret(StaticSecret);

impl EphemeralSecret {
    /// Creates a fresh X25519 ephemeral key from the node identity RNG.
    #[must_use]
    pub fn generate() -> Self {
        Self(StaticSecret::from(SecretKey::generate().to_bytes()))
    }

    fn public_bytes(&self) -> [u8; KEY_LEN] {
        X25519PublicKey::from(&self.0).to_bytes()
    }

    fn shared_with(&self, peer: &[u8; KEY_LEN]) -> [u8; KEY_LEN] {
        let peer = X25519PublicKey::from(*peer);
        self.0.diffie_hellman(&peer).to_bytes()
    }
}

impl Drop for EphemeralSecret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

pub struct HandshakeIdentity {
    secret: SecretKey,
    ephemeral: EphemeralSecret,
}

impl HandshakeIdentity {
    /// Pairs a persistent iroh identity with a fresh X25519 ephemeral key.
    #[must_use]
    pub fn new(secret: SecretKey) -> Self {
        Self {
            secret,
            ephemeral: EphemeralSecret::generate(),
        }
    }

    #[must_use]
    pub fn public(&self) -> PublicKey {
        self.secret.public()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HandshakeKind {
    Proposal,
    Response,
    Confirm,
}

impl HandshakeKind {
    fn to_byte(self) -> u8 {
        match self {
            Self::Proposal => 1,
            Self::Response => 2,
            Self::Confirm => 3,
        }
    }

    fn from_byte(byte: u8) -> Result<Self, SecureError> {
        match byte {
            1 => Ok(Self::Proposal),
            2 => Ok(Self::Response),
            3 => Ok(Self::Confirm),
            _ => Err(SecureError::InvalidMessage),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandshakeMsg {
    kind: HandshakeKind,
    session: SessionId,
    guest: PublicKey,
    host: PublicKey,
    helper: Option<PublicKey>,
    guest_pub: [u8; KEY_LEN],
    host_pub: [u8; KEY_LEN],
    guest_sig: Option<Signature>,
    host_sig: Option<Signature>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Guest,
    Host,
}

pub struct SessionKeys {
    session: SessionId,
    role: Role,
    guest_to_host: CipherState,
    host_to_guest: CipherState,
}

struct CipherState {
    cipher: aes_gcm::Aes256Gcm,
    nonce_base: [u8; NONCE_LEN],
    counter: u64,
}

fn is_zero_pub(pub_key: &[u8; KEY_LEN]) -> bool {
    pub_key == &ZERO_PUB
}

fn nonce_for(base: [u8; NONCE_LEN], counter: u64) -> [u8; NONCE_LEN] {
    let mut nonce = base;
    let counter_bytes = counter.to_be_bytes();
    for (index, byte) in counter_bytes.iter().enumerate() {
        nonce[NONCE_LEN - 8 + index] ^= byte;
    }
    nonce
}

fn take_u8(rest: &mut &[u8]) -> Result<u8, SecureError> {
    Ok(take_array::<1>(rest)?[0])
}

fn take_array<const N: usize>(rest: &mut &[u8]) -> Result<[u8; N], SecureError> {
    if rest.len() < N {
        return Err(SecureError::InvalidMessage);
    }
    let (head, tail) = rest.split_at(N);
    let mut out = [0u8; N];
    out.copy_from_slice(head);
    *rest = tail;
    Ok(out)
}

#[cfg(test)]
mod tests;

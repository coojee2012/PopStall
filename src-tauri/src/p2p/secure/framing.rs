use std::time::Duration;

use anyhow::{Context, bail};
use iroh::PublicKey;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::{
    HandshakeIdentity, HandshakeKind, HandshakeMsg, PathKind, PathTrust, SecureError, SessionKeys,
    ZERO_PUB, is_zero_pub,
};

/// Bounded lifetime of a helper-bridge handshake.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

const MAX_HANDSHAKE_BYTES: usize = 512;

/// Writes one length-prefixed handshake message to an untrusted transport.
///
/// # Errors
///
/// Returns an error when the message cannot be encoded or the transport fails.
pub async fn write_handshake_message<W>(
    writer: &mut W,
    message: &HandshakeMsg,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let bytes = message.encode();
    let len = u32::try_from(bytes.len()).context("handshake message too large")?;
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

/// Reads one bounded, length-prefixed handshake message from an untrusted transport.
///
/// # Errors
///
/// Returns an error for oversized, malformed, or truncated messages.
pub async fn read_handshake_message<R>(reader: &mut R) -> anyhow::Result<HandshakeMsg>
where
    R: AsyncRead + Unpin,
{
    let mut len_bytes = [0u8; 4];
    reader
        .read_exact(&mut len_bytes)
        .await
        .context("P2P 握手流提前结束")?;
    let len = usize::try_from(u32::from_be_bytes(len_bytes)).context("握手长度无效")?;
    if !(1..=MAX_HANDSHAKE_BYTES).contains(&len) {
        bail!("握手消息长度超出限制");
    }
    let mut bytes = vec![0u8; len];
    reader
        .read_exact(&mut bytes)
        .await
        .context("P2P 握手消息提前结束")?;
    Ok(HandshakeMsg::decode(&bytes)?)
}

/// Runs the guest side of a helper-bridge handshake.
///
/// # Errors
///
/// Returns an error for a wrong path identity, invalid peer message, timeout, or transport failure.
pub async fn helper_handshake_request<R, W>(
    reader: &mut R,
    writer: &mut W,
    trust: &PathTrust,
    identity: &HandshakeIdentity,
) -> anyhow::Result<SessionKeys>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    if trust.kind() != PathKind::HelperBridge || identity.public() != *trust.guest() {
        return Err(SecureError::IdentityMismatch.into());
    }
    let proposal = HandshakeMsg::guest_proposal(trust, identity);
    tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        write_handshake_message(writer, &proposal).await?;
        let response = read_handshake_message(reader).await?;
        let (confirm, keys) = HandshakeMsg::guest_complete(identity, trust, &response)?;
        write_handshake_message(writer, &confirm).await?;
        Ok::<_, anyhow::Error>(keys)
    })
    .await
    .context("helper 握手超时")?
}

/// Runs the host side of a helper-bridge handshake.
///
/// # Errors
///
/// Returns an error for a wrong path identity, invalid peer message, timeout, or transport failure.
pub async fn helper_handshake_accept<R, W>(
    reader: &mut R,
    writer: &mut W,
    trust: &PathTrust,
    identity: &HandshakeIdentity,
) -> anyhow::Result<SessionKeys>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    if trust.kind() != PathKind::HelperBridge || identity.public() != *trust.host() {
        return Err(SecureError::IdentityMismatch.into());
    }
    tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        let proposal = read_handshake_message(reader).await?;
        let response = HandshakeMsg::host_response(identity, trust, &proposal)?;
        write_handshake_message(writer, &response).await?;
        let confirm = read_handshake_message(reader).await?;
        let keys = HandshakeMsg::host_complete(identity, trust, &confirm)?;
        Ok::<_, anyhow::Error>(keys)
    })
    .await
    .context("helper 握手超时")?
}

pub async fn helper_handshake_accept_peer<R, W>(
    reader: &mut R,
    writer: &mut W,
    helper: PublicKey,
    identity: &HandshakeIdentity,
) -> anyhow::Result<(PathTrust, SessionKeys)>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        let proposal = read_handshake_message(reader).await?;
        if proposal.kind != HandshakeKind::Proposal
            || proposal.host != identity.public()
            || proposal.helper != Some(helper)
            || is_zero_pub(&proposal.guest_pub)
            || proposal.host_pub != ZERO_PUB
            || proposal.guest_sig.is_some()
            || proposal.host_sig.is_some()
        {
            return Err(SecureError::InvalidMessage.into());
        }
        let trust = PathTrust::HelperBridge {
            session: proposal.session,
            guest: proposal.guest,
            host: proposal.host,
            helper,
        };
        let response = HandshakeMsg::host_response(identity, &trust, &proposal)?;
        write_handshake_message(writer, &response).await?;
        let confirm = read_handshake_message(reader).await?;
        let keys = HandshakeMsg::host_complete(identity, &trust, &confirm)?;
        Ok::<_, anyhow::Error>((trust, keys))
    })
    .await
    .context("helper 握手超时")?
}

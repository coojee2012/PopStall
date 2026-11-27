use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::{Zeroize, Zeroizing};

use super::*;

impl HandshakeMsg {
    /// Builds the unauthenticated guest proposal for a trusted path.
    #[must_use]
    pub fn guest_proposal(trust: &PathTrust, identity: &HandshakeIdentity) -> Self {
        Self {
            kind: HandshakeKind::Proposal,
            session: *trust.session(),
            guest: *trust.guest(),
            host: *trust.host(),
            helper: trust.helper().copied(),
            guest_pub: identity.ephemeral.public_bytes(),
            host_pub: ZERO_PUB,
            guest_sig: None,
            host_sig: None,
        }
    }

    /// Authenticates a host proposal with the persistent iroh identity.
    ///
    /// # Errors
    ///
    /// Returns an error when the proposal identity, shape, or guest key is invalid.
    pub fn host_response(
        identity: &HandshakeIdentity,
        trust: &PathTrust,
        proposal: &Self,
    ) -> Result<Self, SecureError> {
        validate_identity(proposal, trust)?;
        if proposal.kind != HandshakeKind::Proposal
            || is_zero_pub(&proposal.guest_pub)
            || proposal.host_pub != ZERO_PUB
            || proposal.guest_sig.is_some()
            || proposal.host_sig.is_some()
        {
            return Err(SecureError::InvalidMessage);
        }
        let host_pub = identity.ephemeral.public_bytes();
        let signed = transcript(trust, &proposal.guest_pub, &host_pub);
        let host_sig = identity.secret.sign(&signed);
        Ok(Self {
            kind: HandshakeKind::Response,
            session: proposal.session,
            guest: proposal.guest,
            host: proposal.host,
            helper: proposal.helper,
            guest_pub: proposal.guest_pub,
            host_pub,
            guest_sig: None,
            host_sig: Some(host_sig),
        })
    }

    /// Authenticates the guest side and derives guest session keys.
    ///
    /// # Errors
    ///
    /// Returns an error when the response identity, signature, or key material is invalid.
    pub fn guest_complete(
        identity: &HandshakeIdentity,
        trust: &PathTrust,
        response: &Self,
    ) -> Result<(Self, SessionKeys), SecureError> {
        validate_identity(response, trust)?;
        if response.kind != HandshakeKind::Response
            || is_zero_pub(&response.guest_pub)
            || is_zero_pub(&response.host_pub)
            || response.guest_sig.is_some()
            || response.host_sig.is_none()
        {
            return Err(SecureError::InvalidMessage);
        }
        let Some(host_sig) = &response.host_sig else {
            return Err(SecureError::InvalidMessage);
        };
        let signed = transcript(
            trust,
            &identity.ephemeral.public_bytes(),
            &response.host_pub,
        );
        verify_signer(trust.host(), &signed, host_sig)?;
        let guest_sig = identity.secret.sign(&signed);
        let confirm = Self {
            kind: HandshakeKind::Confirm,
            session: response.session,
            guest: response.guest,
            host: response.host,
            helper: response.helper,
            guest_pub: response.guest_pub,
            host_pub: response.host_pub,
            guest_sig: Some(guest_sig),
            host_sig: Some(*host_sig),
        };
        let keys = derive_session(trust, &identity.ephemeral, &response.host_pub, Role::Guest)?;
        Ok((confirm, keys))
    }

    /// Verifies a guest confirmation and derives host session keys.
    ///
    /// # Errors
    ///
    /// Returns an error when the confirmation identity, signatures, or key material is invalid.
    pub fn host_complete(
        identity: &HandshakeIdentity,
        trust: &PathTrust,
        confirm: &Self,
    ) -> Result<SessionKeys, SecureError> {
        validate_identity(confirm, trust)?;
        if confirm.kind != HandshakeKind::Confirm
            || is_zero_pub(&confirm.guest_pub)
            || confirm.host_pub != identity.ephemeral.public_bytes()
            || confirm.guest_sig.is_none()
            || confirm.host_sig.is_none()
        {
            return Err(SecureError::InvalidMessage);
        }
        let (Some(guest_sig), Some(host_sig)) = (&confirm.guest_sig, &confirm.host_sig) else {
            return Err(SecureError::InvalidMessage);
        };
        let signed = transcript(
            trust,
            &confirm.guest_pub,
            &identity.ephemeral.public_bytes(),
        );
        verify_signer(trust.guest(), &signed, guest_sig)?;
        verify_signer(trust.host(), &signed, host_sig)?;
        derive_session(trust, &identity.ephemeral, &confirm.guest_pub, Role::Host)
    }
}

fn validate_identity(message: &HandshakeMsg, trust: &PathTrust) -> Result<(), SecureError> {
    let helper_matches = match (trust.helper(), message.helper.as_ref()) {
        (None, None) => true,
        (Some(expected), Some(actual)) => expected == actual,
        (None, Some(_)) | (Some(_), None) => false,
    };
    if message.session != *trust.session()
        || message.guest != *trust.guest()
        || message.host != *trust.host()
        || !helper_matches
    {
        return Err(SecureError::IdentityMismatch);
    }
    Ok(())
}

fn transcript(trust: &PathTrust, guest_pub: &[u8; KEY_LEN], host_pub: &[u8; KEY_LEN]) -> Vec<u8> {
    let mut signed = Vec::with_capacity(DOMAIN.len() + SESSION_LEN + KEY_LEN * 5);
    signed.extend_from_slice(DOMAIN);
    signed.extend_from_slice(trust.session().as_bytes());
    signed.extend_from_slice(trust.guest().as_bytes());
    signed.extend_from_slice(trust.host().as_bytes());
    match trust.helper() {
        Some(helper) => signed.extend_from_slice(helper.as_bytes()),
        None => signed.extend_from_slice(&ZERO_PUB),
    }
    signed.extend_from_slice(guest_pub);
    signed.extend_from_slice(host_pub);
    signed
}

fn verify_signer(id: &PublicKey, signed: &[u8], signature: &Signature) -> Result<(), SecureError> {
    id.verify(signed, signature)
        .map_err(|_| SecureError::Signature)
}

fn derive_session(
    trust: &PathTrust,
    ephemeral: &EphemeralSecret,
    peer_pub: &[u8; KEY_LEN],
    role: Role,
) -> Result<SessionKeys, SecureError> {
    if is_zero_pub(peer_pub) {
        return Err(SecureError::KeyDerivation);
    }
    let mut shared = Zeroizing::new(ephemeral.shared_with(peer_pub));
    let hkdf = Hkdf::<Sha256>::new(Some(trust.session().as_bytes()), &shared[..]);
    let mut material = Zeroizing::new([0u8; KEY_LEN * 2 + NONCE_LEN * 2]);
    hkdf.expand(KEY_INFO, &mut material[..])
        .map_err(|_| SecureError::KeyDerivation)?;
    let mut guest_key = Zeroizing::new([0u8; KEY_LEN]);
    let mut host_key = Zeroizing::new([0u8; KEY_LEN]);
    let mut guest_nonce = Zeroizing::new([0u8; NONCE_LEN]);
    let mut host_nonce = Zeroizing::new([0u8; NONCE_LEN]);
    guest_key.copy_from_slice(&material[..KEY_LEN]);
    host_key.copy_from_slice(&material[KEY_LEN..KEY_LEN * 2]);
    guest_nonce.copy_from_slice(&material[KEY_LEN * 2..KEY_LEN * 2 + NONCE_LEN]);
    host_nonce.copy_from_slice(&material[KEY_LEN * 2 + NONCE_LEN..]);
    shared.zeroize();
    material.zeroize();
    Ok(SessionKeys {
        session: *trust.session(),
        role,
        guest_to_host: CipherState::new(&guest_key, &guest_nonce)?,
        host_to_guest: CipherState::new(&host_key, &host_nonce)?,
    })
}

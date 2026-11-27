use super::*;

impl HandshakeMsg {
    /// Encodes a handshake message for transport over an untrusted helper.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(369);
        out.extend_from_slice(HANDSHAKE_MAGIC);
        out.push(self.kind.to_byte());
        out.extend_from_slice(self.session.as_bytes());
        out.extend_from_slice(self.guest.as_bytes());
        out.extend_from_slice(self.host.as_bytes());
        out.push(u8::from(self.helper.is_some()));
        if let Some(helper) = &self.helper {
            out.extend_from_slice(helper.as_bytes());
        }
        out.extend_from_slice(&self.guest_pub);
        out.extend_from_slice(&self.host_pub);
        push_signature(&mut out, self.guest_sig.as_ref());
        push_signature(&mut out, self.host_sig.as_ref());
        out
    }

    /// Decodes a handshake message received from an untrusted helper.
    ///
    /// # Errors
    ///
    /// Returns [`SecureError::InvalidMessage`] for malformed bytes or identities.
    pub fn decode(bytes: &[u8]) -> Result<Self, SecureError> {
        let mut rest = bytes;
        if &take_array::<4>(&mut rest)? != HANDSHAKE_MAGIC {
            return Err(SecureError::InvalidMessage);
        }
        let kind = HandshakeKind::from_byte(take_u8(&mut rest)?)?;
        let session = SessionId(take_array::<SESSION_LEN>(&mut rest)?);
        let guest = decode_pub(take_array::<KEY_LEN>(&mut rest)?)?;
        let host = decode_pub(take_array::<KEY_LEN>(&mut rest)?)?;
        let helper = if take_u8(&mut rest)? == 1 {
            Some(decode_pub(take_array::<KEY_LEN>(&mut rest)?)?)
        } else {
            None
        };
        let guest_pub = take_array::<KEY_LEN>(&mut rest)?;
        let host_pub = take_array::<KEY_LEN>(&mut rest)?;
        let guest_sig = take_signature(&mut rest)?;
        let host_sig = take_signature(&mut rest)?;
        if !rest.is_empty() {
            return Err(SecureError::InvalidMessage);
        }
        Ok(Self {
            kind,
            session,
            guest,
            host,
            helper,
            guest_pub,
            host_pub,
            guest_sig,
            host_sig,
        })
    }
}

fn decode_pub(bytes: [u8; KEY_LEN]) -> Result<PublicKey, SecureError> {
    PublicKey::from_bytes(&bytes).map_err(|_| SecureError::InvalidMessage)
}

fn push_signature(out: &mut Vec<u8>, signature: Option<&Signature>) {
    match signature {
        Some(signature) => {
            out.push(1);
            out.extend_from_slice(&signature.to_bytes());
        }
        None => out.push(0),
    }
}

fn take_signature(rest: &mut &[u8]) -> Result<Option<Signature>, SecureError> {
    match take_u8(rest)? {
        0 => Ok(None),
        1 => Ok(Some(Signature::from_bytes(&take_array::<SIGNATURE_LEN>(
            rest,
        )?))),
        _ => Err(SecureError::InvalidMessage),
    }
}

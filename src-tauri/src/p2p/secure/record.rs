use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};

use super::*;

impl SessionKeys {
    /// Encrypts one opaque record for the peer on this trusted path.
    ///
    /// # Errors
    ///
    /// Returns an error when encryption fails or the per-direction counter is exhausted.
    pub fn seal(&mut self, record_type: u8, plaintext: &[u8]) -> Result<Vec<u8>, SecureError> {
        match self.role {
            Role::Guest => {
                self.guest_to_host
                    .seal(&self.session, DIR_GUEST_TO_HOST, record_type, plaintext)
            }
            Role::Host => {
                self.host_to_guest
                    .seal(&self.session, DIR_HOST_TO_GUEST, record_type, plaintext)
            }
        }
    }

    /// Decrypts one opaque record received from the peer on this trusted path.
    ///
    /// Returns the record type and plaintext.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed records, replayed counters, or failed authentication.
    pub fn open(&mut self, record: &[u8]) -> Result<(u8, Vec<u8>), SecureError> {
        match self.role {
            Role::Guest => self
                .host_to_guest
                .open(&self.session, DIR_HOST_TO_GUEST, record),
            Role::Host => self
                .guest_to_host
                .open(&self.session, DIR_GUEST_TO_HOST, record),
        }
    }
}

impl CipherState {
    pub(super) fn new(
        key: &[u8; KEY_LEN],
        nonce_base: &[u8; NONCE_LEN],
    ) -> Result<Self, SecureError> {
        let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| SecureError::KeyDerivation)?;
        Ok(Self {
            cipher,
            nonce_base: *nonce_base,
            counter: 0,
        })
    }

    fn seal(
        &mut self,
        session: &SessionId,
        direction: u8,
        record_type: u8,
        plaintext: &[u8],
    ) -> Result<Vec<u8>, SecureError> {
        let counter = self
            .counter
            .checked_add(1)
            .ok_or(SecureError::CounterExhausted)?;
        let nonce = nonce_for(self.nonce_base, counter);
        let aad = record_aad(session, direction, counter, record_type);
        let ciphertext = self
            .cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| SecureError::Encrypt)?;
        let mut record = Vec::with_capacity(RECORD_HEADER_LEN + ciphertext.len());
        record.extend_from_slice(&counter.to_be_bytes());
        record.push(record_type);
        record.extend_from_slice(&ciphertext);
        self.counter = counter;
        Ok(record)
    }

    fn open(
        &mut self,
        session: &SessionId,
        direction: u8,
        record: &[u8],
    ) -> Result<(u8, Vec<u8>), SecureError> {
        if record.len() < RECORD_HEADER_LEN {
            return Err(SecureError::InvalidMessage);
        }
        let mut counter_bytes = [0u8; 8];
        counter_bytes.copy_from_slice(&record[..8]);
        let counter = u64::from_be_bytes(counter_bytes);
        let record_type = record[8];
        let expected = self
            .counter
            .checked_add(1)
            .ok_or(SecureError::CounterExhausted)?;
        if counter != expected {
            return Err(SecureError::Counter {
                expected,
                actual: counter,
            });
        }
        let nonce = nonce_for(self.nonce_base, counter);
        let aad = record_aad(session, direction, counter, record_type);
        let plaintext = self
            .cipher
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &record[RECORD_HEADER_LEN..],
                    aad: &aad,
                },
            )
            .map_err(|_| SecureError::Decrypt)?;
        self.counter = counter;
        Ok((record_type, plaintext))
    }
}

fn record_aad(session: &SessionId, direction: u8, counter: u64, record_type: u8) -> Vec<u8> {
    let mut aad = Vec::with_capacity(SESSION_LEN + 10);
    aad.extend_from_slice(session.as_bytes());
    aad.push(direction);
    aad.extend_from_slice(&counter.to_be_bytes());
    aad.push(record_type);
    aad
}

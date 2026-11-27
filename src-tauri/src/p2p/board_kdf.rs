//! HKDF-SHA256 used for the plaza publish key.

use hkdf::Hkdf;
use sha2::Sha256;

/// Derives one 32-byte publish key. HKDF output length is fixed, so `expand`
/// cannot fail for this function's contract.
pub fn hkdf_sha256(ikm: &[u8], salt: &[u8], info: &[u8], out: &mut [u8; 32]) {
    Hkdf::<Sha256>::new(Some(salt), ikm)
        .expand(info, out)
        .expect("32-byte HKDF output is valid");
}

#[cfg(test)]
mod tests {
    use super::hkdf_sha256;

    #[test]
    fn hkdf_matches_rfc5869_test_case_1() {
        let ikm = [0x0b; 22];
        let salt = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
        ];
        let info = [0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9];
        let mut out = [0u8; 32];
        hkdf_sha256(&ikm, &salt, &info, &mut out);
        assert_eq!(
            out,
            [
                0x3c, 0xb2, 0x5f, 0x25, 0xfa, 0xac, 0xd5, 0x7a, 0x90, 0x43, 0x4f, 0x64, 0xd0, 0x36,
                0x2f, 0x2a, 0x2d, 0x2d, 0x0a, 0x90, 0xcf, 0x1a, 0x5a, 0x4c, 0x5d, 0xb0, 0x2d, 0x56,
                0xec, 0xc4, 0xc5, 0xbf,
            ]
        );
    }
}

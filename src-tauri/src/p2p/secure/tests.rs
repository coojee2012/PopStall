use std::collections::HashSet;
use std::io::ErrorKind;

use iroh::SecretKey;
use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex, split};

use super::stream::MAX_PLAINTEXT_CHUNK;
use super::*;

struct Fixture {
    trust: PathTrust,
    guest_identity: HandshakeIdentity,
    host_identity: HandshakeIdentity,
}

impl Fixture {
    fn helper_path() -> Self {
        let guest_secret = SecretKey::generate();
        let host_secret = SecretKey::generate();
        let helper_secret = SecretKey::generate();
        Self {
            trust: PathTrust::HelperBridge {
                session: SessionId::generate(),
                guest: guest_secret.public(),
                host: host_secret.public(),
                helper: helper_secret.public(),
            },
            guest_identity: HandshakeIdentity::new(guest_secret),
            host_identity: HandshakeIdentity::new(host_secret),
        }
    }

    fn end_to_end_path() -> Self {
        let guest_secret = SecretKey::generate();
        let host_secret = SecretKey::generate();
        Self {
            trust: PathTrust::EndToEnd {
                session: SessionId::generate(),
                guest: guest_secret.public(),
                host: host_secret.public(),
            },
            guest_identity: HandshakeIdentity::new(guest_secret),
            host_identity: HandshakeIdentity::new(host_secret),
        }
    }

    fn proposal(&self) -> HandshakeMsg {
        HandshakeMsg::guest_proposal(&self.trust, &self.guest_identity)
    }

    fn handshake(&self) -> (HandshakeMsg, SessionKeys, SessionKeys) {
        let proposal = self.proposal();
        let response = HandshakeMsg::host_response(&self.host_identity, &self.trust, &proposal)
            .expect("host_response should succeed");
        let (confirm, guest_keys) =
            HandshakeMsg::guest_complete(&self.guest_identity, &self.trust, &response)
                .expect("guest_complete should succeed");
        let host_keys = HandshakeMsg::host_complete(&self.host_identity, &self.trust, &confirm)
            .expect("host_complete should succeed");
        (confirm, guest_keys, host_keys)
    }
}

#[test]
fn helper_path_handshake_seals_records_in_both_directions() {
    let fixture = Fixture::helper_path();
    let (_, mut guest_keys, mut host_keys) = fixture.handshake();

    let guest_record = guest_keys
        .seal(RECORD_DATA, b"guest-to-host")
        .expect("guest seal should succeed");
    assert_eq!(
        host_keys
            .open(&guest_record)
            .expect("host open should succeed"),
        (RECORD_DATA, b"guest-to-host".to_vec())
    );

    let host_record = host_keys
        .seal(RECORD_CLOSE, b"host-to-guest")
        .expect("host seal should succeed");
    assert_eq!(
        guest_keys
            .open(&host_record)
            .expect("guest open should succeed"),
        (RECORD_CLOSE, b"host-to-guest".to_vec())
    );
}

#[test]
fn end_to_end_path_handshake_works_without_helper() {
    let fixture = Fixture::end_to_end_path();
    let (_, mut guest_keys, mut host_keys) = fixture.handshake();

    let record = guest_keys.seal(RECORD_DATA, b"direct").expect("seal");
    assert_eq!(
        host_keys.open(&record).expect("open"),
        (RECORD_DATA, b"direct".to_vec())
    );
}

#[test]
fn host_response_rejects_wrong_path_identities() {
    let fixture = Fixture::helper_path();
    let proposal = fixture.proposal();
    let wrong_key = SecretKey::generate().public();

    let mut wrong_helper = proposal.clone();
    wrong_helper.helper = Some(wrong_key);
    assert_eq!(
        HandshakeMsg::host_response(&fixture.host_identity, &fixture.trust, &wrong_helper),
        Err(SecureError::IdentityMismatch)
    );

    let mut wrong_guest = proposal.clone();
    wrong_guest.guest = wrong_key;
    assert_eq!(
        HandshakeMsg::host_response(&fixture.host_identity, &fixture.trust, &wrong_guest),
        Err(SecureError::IdentityMismatch)
    );

    let mut wrong_host = proposal;
    wrong_host.host = wrong_key;
    assert_eq!(
        HandshakeMsg::host_response(&fixture.host_identity, &fixture.trust, &wrong_host),
        Err(SecureError::IdentityMismatch)
    );
}

#[test]
fn host_complete_rejects_wrong_path_identities() {
    let fixture = Fixture::helper_path();
    let (confirm, _, _) = fixture.handshake();
    let wrong_key = SecretKey::generate().public();

    let mut wrong_helper = confirm.clone();
    wrong_helper.helper = Some(wrong_key);
    assert_eq!(
        HandshakeMsg::host_complete(&fixture.host_identity, &fixture.trust, &wrong_helper)
            .map(|_| ()),
        Err(SecureError::IdentityMismatch)
    );

    let mut wrong_guest = confirm.clone();
    wrong_guest.guest = wrong_key;
    assert_eq!(
        HandshakeMsg::host_complete(&fixture.host_identity, &fixture.trust, &wrong_guest)
            .map(|_| ()),
        Err(SecureError::IdentityMismatch)
    );

    let mut wrong_host = confirm;
    wrong_host.host = wrong_key;
    assert_eq!(
        HandshakeMsg::host_complete(&fixture.host_identity, &fixture.trust, &wrong_host)
            .map(|_| ()),
        Err(SecureError::IdentityMismatch)
    );
}

#[test]
fn guest_complete_rejects_forged_host_signature() {
    let fixture = Fixture::helper_path();
    let response =
        HandshakeMsg::host_response(&fixture.host_identity, &fixture.trust, &fixture.proposal())
            .expect("host_response should succeed");
    let mut forged = response;
    forged.host_sig = Some(SecretKey::generate().sign(b"forged-host"));

    assert_eq!(
        HandshakeMsg::guest_complete(&fixture.guest_identity, &fixture.trust, &forged).map(|_| ()),
        Err(SecureError::Signature)
    );
}

#[test]
fn host_complete_rejects_forged_guest_signature() {
    let fixture = Fixture::helper_path();
    let (mut confirm, _, _) = fixture.handshake();
    confirm.guest_sig = Some(SecretKey::generate().sign(b"forged-guest"));

    assert_eq!(
        HandshakeMsg::host_complete(&fixture.host_identity, &fixture.trust, &confirm).map(|_| ()),
        Err(SecureError::Signature)
    );
}

#[test]
fn replayed_record_is_rejected() {
    let fixture = Fixture::helper_path();
    let (_, mut guest_keys, mut host_keys) = fixture.handshake();
    let record = guest_keys.seal(RECORD_DATA, b"once").expect("seal");

    assert!(host_keys.open(&record).is_ok());
    assert_eq!(
        host_keys.open(&record),
        Err(SecureError::Counter {
            expected: 2,
            actual: 1
        })
    );
}

#[test]
fn record_with_tampered_aad_is_rejected() {
    let fixture = Fixture::helper_path();
    let (_, mut guest_keys, mut host_keys) = fixture.handshake();
    let mut record = guest_keys.seal(RECORD_DATA, b"aad").expect("seal");
    record[8] = RECORD_CLOSE;

    assert_eq!(host_keys.open(&record), Err(SecureError::Decrypt));
}

#[test]
fn record_with_tampered_ciphertext_is_rejected() {
    let fixture = Fixture::helper_path();
    let (_, mut guest_keys, mut host_keys) = fixture.handshake();
    let mut record = guest_keys.seal(RECORD_DATA, b"ciphertext").expect("seal");
    let last = record.len() - 1;
    record[last] ^= 0x01;

    assert_eq!(host_keys.open(&record), Err(SecureError::Decrypt));
}

#[test]
fn ten_thousand_records_have_unique_nonces() {
    let fixture = Fixture::helper_path();
    let (_, mut guest_keys, _) = fixture.handshake();
    let base = guest_keys.guest_to_host.nonce_base;
    let mut seen = HashSet::with_capacity(10_000);

    for expected in 1..=10_000u64 {
        let record = guest_keys.seal(RECORD_DATA, b"nonce").expect("seal");
        let counter = u64::from_be_bytes(record[..8].try_into().expect("counter bytes"));
        assert_eq!(counter, expected);
        assert!(
            seen.insert(nonce_for(base, counter)),
            "duplicate nonce at counter {counter}"
        );
    }

    assert_eq!(seen.len(), 10_000);
}

#[test]
fn handshake_messages_round_trip_through_wire_codec() {
    let fixture = Fixture::helper_path();
    let proposal = fixture.proposal();
    let response = HandshakeMsg::host_response(&fixture.host_identity, &fixture.trust, &proposal)
        .expect("host_response should succeed");
    let (confirm, _, _) = fixture.handshake();

    for message in [proposal, response, confirm] {
        let encoded = message.encode();
        assert_eq!(
            HandshakeMsg::decode(&encoded).expect("decode should succeed"),
            message
        );
    }
}

#[test]
fn wire_codec_rejects_malformed_messages() {
    let fixture = Fixture::helper_path();
    let encoded = fixture.proposal().encode();

    let mut bad_magic = encoded.clone();
    bad_magic[0] ^= 0x01;
    assert_eq!(
        HandshakeMsg::decode(&bad_magic),
        Err(SecureError::InvalidMessage)
    );

    assert_eq!(
        HandshakeMsg::decode(&encoded[..encoded.len() - 1]),
        Err(SecureError::InvalidMessage)
    );
}

#[tokio::test]
async fn handshake_framing_round_trips_message() {
    let fixture = Fixture::helper_path();
    let (mut writer, mut reader) = duplex(4096);
    let proposal = fixture.proposal();

    write_handshake_message(&mut writer, &proposal)
        .await
        .expect("handshake write should succeed");
    let decoded = tokio::time::timeout(HANDSHAKE_TIMEOUT, read_handshake_message(&mut reader))
        .await
        .expect("framing should finish before timeout")
        .expect("handshake read should succeed");
    assert_eq!(decoded, proposal);
}

#[tokio::test]
async fn handshake_framing_rejects_oversized_length() {
    let (_writer_io, mut reader) = duplex(64);
    let mut writer_io = _writer_io;
    writer_io
        .write_all(&u32::MAX.to_be_bytes())
        .await
        .expect("length write should succeed");

    let err = tokio::time::timeout(HANDSHAKE_TIMEOUT, read_handshake_message(&mut reader))
        .await
        .expect("framing should finish before timeout")
        .expect_err("oversized handshake should fail");
    assert!(err.to_string().contains("握手消息长度超出限制"));
}

#[tokio::test]
async fn helper_handshake_helpers_negotiate_matching_keys() {
    let fixture = Fixture::helper_path();
    let (guest_io, host_io) = duplex(4096);
    let (mut guest_recv, mut guest_send) = split(guest_io);
    let (mut host_recv, mut host_send) = split(host_io);

    let (guest_result, host_result) = tokio::join!(
        helper_handshake_request(
            &mut guest_recv,
            &mut guest_send,
            &fixture.trust,
            &fixture.guest_identity,
        ),
        helper_handshake_accept(
            &mut host_recv,
            &mut host_send,
            &fixture.trust,
            &fixture.host_identity,
        ),
    );
    let mut guest_keys = guest_result.expect("guest handshake should succeed");
    let mut host_keys = host_result.expect("host handshake should succeed");

    let record = guest_keys
        .seal(RECORD_DATA, b"over-duplex")
        .expect("seal should succeed");
    assert_eq!(
        host_keys.open(&record).expect("open should succeed"),
        (RECORD_DATA, b"over-duplex".to_vec())
    );
}

#[tokio::test]
async fn helper_handshake_request_rejects_wrong_identity() {
    let fixture = Fixture::helper_path();
    let (guest_io, _host_io) = duplex(64);
    let (mut guest_recv, mut guest_send) = split(guest_io);
    let attacker = HandshakeIdentity::new(SecretKey::generate());

    let err = helper_handshake_request(&mut guest_recv, &mut guest_send, &fixture.trust, &attacker)
        .await
        .err()
        .expect("wrong guest identity should fail");
    assert_eq!(
        err.downcast_ref::<SecureError>(),
        Some(&SecureError::IdentityMismatch)
    );
}

#[tokio::test]
async fn sealed_stream_round_trips_chunked_payload_and_close() {
    let fixture = Fixture::helper_path();
    let (_, guest_keys, host_keys) = fixture.handshake();
    let (guest_io, host_io) = duplex(256 * 1024);
    let (guest_recv, guest_send) = split(guest_io);
    let (host_recv, host_send) = split(host_io);
    let mut guest = SealedStream::new(guest_recv, guest_send, guest_keys);
    let mut host = SealedStream::new(host_recv, host_send, host_keys);
    let payload = vec![0x5a; MAX_PLAINTEXT_CHUNK * 2 + 17];

    guest
        .write_all(&payload)
        .await
        .expect("sealed writes should succeed");
    guest.shutdown().await.expect("sealed close should succeed");

    let mut received = Vec::new();
    host.read_to_end(&mut received)
        .await
        .expect("sealed reads should succeed");
    assert_eq!(received, payload);
}

#[tokio::test]
async fn sealed_stream_rejects_oversized_length() {
    let fixture = Fixture::helper_path();
    let (_, _, host_keys) = fixture.handshake();
    let (writer_io, host_io) = duplex(64);
    let (_raw_recv, mut raw_send) = split(writer_io);
    let (host_recv, host_send) = split(host_io);
    let mut host = SealedStream::new(host_recv, host_send, host_keys);

    raw_send
        .write_all(&u32::MAX.to_be_bytes())
        .await
        .expect("length write should succeed");
    raw_send.shutdown().await.expect("raw close should succeed");

    let err = host
        .read_to_end(&mut Vec::new())
        .await
        .expect_err("oversized sealed frame should fail");
    assert_eq!(err.kind(), ErrorKind::InvalidData);
}

#[tokio::test]
async fn sealed_stream_rejects_tampered_record() {
    let fixture = Fixture::helper_path();
    let (_, mut guest_keys, host_keys) = fixture.handshake();
    let mut record = guest_keys
        .seal(RECORD_DATA, b"tampered")
        .expect("seal should succeed");
    let last = record.len() - 1;
    record[last] ^= 0x01;

    let (writer_io, host_io) = duplex(256);
    let (_raw_recv, mut raw_send) = split(writer_io);
    let (host_recv, host_send) = split(host_io);
    let mut host = SealedStream::new(host_recv, host_send, host_keys);

    raw_send
        .write_all(&(record.len() as u32).to_be_bytes())
        .await
        .expect("length write should succeed");
    raw_send
        .write_all(&record)
        .await
        .expect("record write should succeed");
    raw_send.shutdown().await.expect("raw close should succeed");

    let err = host
        .read_to_end(&mut Vec::new())
        .await
        .expect_err("tampered sealed record should fail");
    assert_eq!(err.kind(), ErrorKind::InvalidData);
}

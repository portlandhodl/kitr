// SPDX-License-Identifier: GPL-3.0-or-later
//! The encryption handshake state machine used by remote control.
//!
//! Kitty's encrypted command exchange (`CommandEncrypter` in
//! `kitty/remote_control.py`, `parse_cmd()` in the same file and
//! `parse_cmd()`/`peer_message_received()` in `kitty/boss.py`) works like this:
//!
//! 1. The kitty process owns a long-lived X25519 key. Its raw public key is
//!    published as `KITTY_PUBLIC_KEY=<protocol>:<base85(public)>`.
//! 2. For every command the client generates a fresh X25519 key, derives
//!    `SHA256(X25519(client_private, server_public))`, and encrypts a JSON
//!    payload with AES-256-GCM using a fresh random 12-byte IV.
//! 3. The JSON envelope contains `version`, `iv`, `tag`, `pubkey`,
//!    `encrypted`, and optionally `enc_proto`, all base85-encoded. The server
//!    derives the same secret and decrypts it. The inner JSON also carries a
//!    nanosecond timestamp, rejected if it differs from the local clock by
//!    more than five minutes.
//!
//! [`CryptoState`] implements steps 1-3 at the byte level. It intentionally
//! does **not** implement the JSON envelope or the DCS transport; combine it
//! with [`crate::framing::Framer`] and a JSON layer at the integration
//! boundary. Each `seal()` uses a new random IV, exactly like kitty; kitty does
//! not use a nonce counter. [`NonceCounter`] is provided only for protocols
//! that want a long-lived counter-based AES-GCM stream and is not used by
//! `CryptoState`.

use crate::base64::encode_b85;
use crate::crypto::{
    Aes256GcmDecrypt, Aes256GcmEncrypt, EllipticCurveKey, Secret, GCM_IV_LEN, GCM_TAG_LEN,
};
use crate::error::{Error, Result};
use crate::framing::DEFAULT_MAX_MESSAGE_SIZE;

/// Which side of the handshake this state represents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The kitty process holding the long-lived key.
    Server,
    /// A remote-control client generating an ephemeral key.
    Client,
}

/// A sealed remote-control payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedMessage {
    /// 12-byte random AES-GCM IV.
    pub iv: [u8; GCM_IV_LEN],
    /// 16-byte AES-GCM authentication tag.
    pub tag: [u8; GCM_TAG_LEN],
    /// Ciphertext (same length as the plaintext).
    pub ciphertext: Vec<u8>,
}

impl SealedMessage {
    /// Reserve a message with empty ciphertext, useful when decoding an
    /// envelope field by field.
    pub fn new(iv: [u8; GCM_IV_LEN], tag: [u8; GCM_TAG_LEN], ciphertext: Vec<u8>) -> Self {
        Self {
            iv,
            tag,
            ciphertext,
        }
    }

    /// Base85 encoding of the IV, as used in the JSON envelope.
    pub fn iv_base85(&self) -> String {
        String::from_utf8(encode_b85(&self.iv)).expect("base85 is ASCII")
    }

    /// Base85 encoding of the tag, as used in the JSON envelope.
    pub fn tag_base85(&self) -> String {
        String::from_utf8(encode_b85(&self.tag)).expect("base85 is ASCII")
    }

    /// Base85 encoding of the ciphertext, as used in the JSON envelope.
    pub fn ciphertext_base85(&self) -> String {
        String::from_utf8(encode_b85(&self.ciphertext)).expect("base85 is ASCII")
    }
}

/// X25519 + AES-256-GCM state for one remote-control peer.
///
/// The server side calls [`CryptoState::new_server`] once and
/// [`CryptoState::establish`] with the client's ephemeral public key from each
/// envelope. The client calls [`CryptoState::new_client`] with the server's
/// published key, then [`CryptoState::seal`].
pub struct CryptoState {
    key: EllipticCurveKey,
    secret: Option<Secret>,
    role: Role,
    max_message_size: usize,
}

impl CryptoState {
    /// Create the server (kitty) side with a fresh long-lived key.
    pub fn new_server() -> Result<Self> {
        Ok(Self {
            key: EllipticCurveKey::generate()?,
            secret: None,
            role: Role::Server,
            max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
        })
    }

    /// Create the server side from an existing key (deterministic tests).
    pub fn new_server_with_key(key: EllipticCurveKey) -> Self {
        Self {
            key,
            secret: None,
            role: Role::Server,
            max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
        }
    }

    /// Create a client with a fresh ephemeral key and derive the shared secret
    /// from the server's published public key.
    pub fn new_client(server_public: &[u8]) -> Result<Self> {
        Self::new_client_with_key(EllipticCurveKey::generate()?, server_public)
    }

    /// Create a client from an existing key (deterministic tests).
    pub fn new_client_with_key(key: EllipticCurveKey, server_public: &[u8]) -> Result<Self> {
        let secret = key.derive_secret(server_public)?;
        Ok(Self {
            key,
            secret: Some(secret),
            role: Role::Client,
            max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
        })
    }

    /// Set the maximum plaintext/ciphertext length accepted by this state.
    /// Kitty has no such limit; the default is the same 16 MiB as the framer.
    pub fn with_max_message_size(mut self, max_message_size: usize) -> Self {
        self.max_message_size = max_message_size;
        self
    }

    /// Which side of the handshake this is.
    pub fn role(&self) -> Role {
        self.role
    }

    /// Raw public key to publish (or put in an envelope).
    pub fn public_key(&self) -> Result<Vec<u8>> {
        self.key.public_key()
    }

    /// Base85 public key, matching the `pubkey` envelope field.
    pub fn public_key_base85(&self) -> Result<String> {
        Ok(String::from_utf8(encode_b85(&self.public_key()?)).expect("base85 is ASCII"))
    }

    /// Whether the shared secret has been derived.
    pub fn is_established(&self) -> bool {
        self.secret.is_some()
    }

    /// Derive the shared secret from the peer's raw public key. On the server
    /// this is called once per incoming envelope; on the client it is done by
    /// the constructor. Calling it again replaces the previous secret.
    pub fn establish(&mut self, peer_public: &[u8]) -> Result<&mut Self> {
        self.secret = Some(self.key.derive_secret(peer_public)?);
        Ok(self)
    }

    /// Borrow the derived secret, if any. Tests use this to verify both sides
    /// agreed; production callers should not need it.
    pub fn shared_secret(&self) -> Option<&Secret> {
        self.secret.as_ref()
    }

    fn secret(&self) -> Result<&Secret> {
        self.secret.as_ref().ok_or(Error::InvalidState(
            "handshake is not established: call establish() or use new_client()",
        ))
    }

    /// Encrypt `plaintext` with a fresh random IV and no AAD.
    pub fn seal(&self, plaintext: &[u8]) -> Result<SealedMessage> {
        self.seal_with_aad(plaintext, &[])
    }

    /// Encrypt `plaintext` with a fresh random IV and optional AAD.
    pub fn seal_with_aad(&self, plaintext: &[u8], aad: &[u8]) -> Result<SealedMessage> {
        let secret = self.secret()?;
        if plaintext.len() > self.max_message_size {
            return Err(Error::MessageTooLarge {
                size: plaintext.len(),
                max: self.max_message_size,
            });
        }
        let mut enc = Aes256GcmEncrypt::new(secret)?;
        enc.add_aad(aad)?;
        let ciphertext = enc.add_data(plaintext, true)?;
        let mut iv = [0u8; GCM_IV_LEN];
        iv.copy_from_slice(enc.iv());
        let mut tag = [0u8; GCM_TAG_LEN];
        tag.copy_from_slice(enc.tag());
        Ok(SealedMessage {
            iv,
            tag,
            ciphertext,
        })
    }

    /// Decrypt and authenticate `message` with no AAD.
    pub fn open(&self, message: &SealedMessage) -> Result<Vec<u8>> {
        self.open_with_aad(message, &[])
    }

    /// Decrypt and authenticate `message` with optional AAD.
    pub fn open_with_aad(&self, message: &SealedMessage, aad: &[u8]) -> Result<Vec<u8>> {
        let secret = self.secret()?;
        if message.ciphertext.len() > self.max_message_size {
            return Err(Error::MessageTooLarge {
                size: message.ciphertext.len(),
                max: self.max_message_size,
            });
        }
        let mut dec = Aes256GcmDecrypt::new(secret, &message.iv, &message.tag)?;
        dec.add_aad(aad)?;
        dec.add_data(&message.ciphertext, true)
    }
}

impl std::fmt::Debug for CryptoState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CryptoState")
            .field("role", &self.role)
            .field("established", &self.is_established())
            .field("max_message_size", &self.max_message_size)
            .finish()
    }
}

/// Counter-based GCM nonces for stream protocols.
///
/// Kitty does not use this: every remote-control message gets a fresh random
/// IV. The counter is a 4-byte prefix followed by a big-endian 64-bit counter,
/// which cannot wrap.
#[derive(Clone, Debug)]
pub struct NonceCounter {
    prefix: [u8; 4],
    counter: u64,
}

impl NonceCounter {
    /// Start at zero with the given prefix.
    pub fn new(prefix: [u8; 4]) -> Self {
        Self { prefix, counter: 0 }
    }

    /// Current counter value (the value that the next [`Self::next`] will use).
    pub fn counter(&self) -> u64 {
        self.counter
    }

    /// Produce the next 12-byte nonce.
    pub fn next_nonce(&mut self) -> Result<[u8; GCM_IV_LEN]> {
        let counter = self
            .counter
            .checked_add(1)
            .ok_or(Error::InvalidState("GCM nonce counter exhausted"))?;
        let mut nonce = [0u8; GCM_IV_LEN];
        nonce[..4].copy_from_slice(&self.prefix);
        nonce[4..].copy_from_slice(&self.counter.to_be_bytes());
        self.counter = counter;
        Ok(nonce)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::EllipticCurveKey;

    fn fixed_pair() -> (CryptoState, CryptoState) {
        let server_key = EllipticCurveKey::from_private_bytes(&[
            0x77, 0x07, 0x6d, 0x0a, 0x73, 0x18, 0xa5, 0x7d, 0x3c, 0x16, 0xc1, 0x72, 0x51, 0xb2,
            0x66, 0x45, 0xdf, 0x4c, 0x2f, 0x87, 0xeb, 0xc0, 0x99, 0x2a, 0xb1, 0x77, 0xfb, 0xa5,
            0x1d, 0xb9, 0x2c, 0x2a,
        ])
        .unwrap();
        let client_key = EllipticCurveKey::from_private_bytes(&[
            0x5d, 0xab, 0x08, 0x7e, 0x62, 0x4a, 0x8a, 0x4b, 0x79, 0xe1, 0x7f, 0x8b, 0x83, 0x80,
            0x0e, 0xe6, 0x6f, 0x3b, 0xb1, 0x29, 0x26, 0x18, 0xb6, 0xfd, 0x1c, 0x2f, 0x8b, 0x27,
            0xff, 0x88, 0xe0, 0xeb,
        ])
        .unwrap();
        let server_pub = server_key.public_key().unwrap();
        let server = CryptoState::new_server_with_key(server_key);
        let client = CryptoState::new_client_with_key(client_key, &server_pub).unwrap();
        (server, client)
    }

    #[test]
    fn deterministic_handshake_and_roundtrip() {
        let (mut server, client) = fixed_pair();
        assert!(client.is_established());
        assert!(!server.is_established());
        assert_eq!(client.role(), Role::Client);
        assert_eq!(server.role(), Role::Server);
        server.establish(&client.public_key().unwrap()).unwrap();
        assert_eq!(
            server.shared_secret().unwrap(),
            client.shared_secret().unwrap()
        );
        let message = client.seal(br#"{"cmd":"ls"}"#).unwrap();
        assert_eq!(message.iv.len(), 12);
        assert_eq!(message.tag.len(), 16);
        assert_eq!(server.open(&message).unwrap(), br#"{"cmd":"ls"}"#);
    }

    #[test]
    fn client_and_server_public_keys_are_published_as_base85() {
        let (_server, client) = fixed_pair();
        let text = client.public_key_base85().unwrap();
        let decoded = crate::base64::decode_b85(text.as_bytes()).unwrap();
        assert_eq!(decoded, client.public_key().unwrap());
        assert!(!text.contains('='));
    }

    #[test]
    fn random_handshake_roundtrip() {
        let server = CryptoState::new_server().unwrap();
        let client = CryptoState::new_client(&server.public_key().unwrap()).unwrap();
        let mut server = server;
        server.establish(&client.public_key().unwrap()).unwrap();
        let payload = vec![0x42; 4096];
        let sealed = client.seal(&payload).unwrap();
        assert_eq!(server.open(&sealed).unwrap(), payload);
        // Server responses use the same state.
        let response = server.seal(b"response").unwrap();
        assert_eq!(client.open(&response).unwrap(), b"response");
    }

    #[test]
    fn aad_roundtrip_and_tamper() {
        let (mut server, client) = fixed_pair();
        server.establish(&client.public_key().unwrap()).unwrap();
        let sealed = client.seal_with_aad(b"payload", b"header").unwrap();
        assert_eq!(
            server.open_with_aad(&sealed, b"header").unwrap(),
            b"payload"
        );
        assert!(matches!(
            server.open_with_aad(&sealed, b"other"),
            Err(Error::AuthenticationFailed)
        ));
        assert!(matches!(
            server.open(&sealed),
            Err(Error::AuthenticationFailed)
        ));
    }

    #[test]
    fn tampered_ciphertext_and_tag_fail() {
        let (mut server, client) = fixed_pair();
        server.establish(&client.public_key().unwrap()).unwrap();
        let mut sealed = client.seal(b"payload").unwrap();
        sealed.ciphertext[0] ^= 1;
        assert!(matches!(
            server.open(&sealed),
            Err(Error::AuthenticationFailed)
        ));
        let mut sealed = client.seal(b"payload").unwrap();
        sealed.tag[0] ^= 1;
        assert!(matches!(
            server.open(&sealed),
            Err(Error::AuthenticationFailed)
        ));
    }

    #[test]
    fn sealing_before_establish_fails() {
        let server = CryptoState::new_server().unwrap();
        assert!(matches!(server.seal(b"x"), Err(Error::InvalidState(_))));
        assert!(matches!(
            server.open(&SealedMessage::new([0; 12], [0; 16], vec![])),
            Err(Error::InvalidState(_))
        ));
    }

    #[test]
    fn message_size_limit() {
        let (mut server, client) = fixed_pair();
        server.establish(&client.public_key().unwrap()).unwrap();
        let client = client.with_max_message_size(4);
        assert!(matches!(
            client.seal(b"12345"),
            Err(Error::MessageTooLarge { size: 5, max: 4 })
        ));
        let sealed = client.seal(b"1234").unwrap();
        assert_eq!(server.open(&sealed).unwrap(), b"1234");
    }

    #[test]
    fn fresh_iv_per_message() {
        let (_server, client) = fixed_pair();
        let a = client.seal(b"same").unwrap();
        let b = client.seal(b"same").unwrap();
        assert_ne!(a.iv, b.iv);
        assert_ne!(a.ciphertext, b.ciphertext);
    }

    #[test]
    fn nonce_counter() {
        let mut counter = NonceCounter::new([1, 2, 3, 4]);
        let a = counter.next_nonce().unwrap();
        let b = counter.next_nonce().unwrap();
        assert_eq!(&a[..4], &[1, 2, 3, 4]);
        assert_eq!(&a[4..], &0u64.to_be_bytes());
        assert_eq!(&b[4..], &1u64.to_be_bytes());
        assert_eq!(counter.counter(), 2);

        let mut exhausted = NonceCounter {
            prefix: [0; 4],
            counter: u64::MAX,
        };
        assert!(exhausted.next_nonce().is_err());
    }
}

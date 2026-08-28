use std::io;

use ctr::cipher::StreamCipher;
use sha2::{Digest, Sha256};

use crate::crypto::{CryptoContext, DownstreamCrypto, UpstreamCrypto, new_aes_ctr};

pub const HANDSHAKE_LEN: usize = 64;
const SKIP_LEN: usize = 8;
const PREKEY_LEN: usize = 32;
const IV_LEN: usize = 16;
const PROTO_TAG_POS: usize = 56;
const DC_POS: usize = 60;
const ZERO_64: [u8; 64] = [0; 64];

pub const PROTO_ABRIDGED: [u8; 4] = [0xef; 4];
pub const PROTO_INTERMEDIATE: [u8; 4] = [0xee; 4];
pub const PROTO_PADDED_INTERMEDIATE: [u8; 4] = [0xdd; 4];

const RESERVED_FIRST_BYTES: &[u8] = &[0xef];
const RESERVED_STARTS: &[[u8; 4]] = &[
    *b"HEAD",
    *b"POST",
    *b"OPTI",
    *b"GET ",
    PROTO_INTERMEDIATE,
    PROTO_PADDED_INTERMEDIATE,
    [0x16, 0x03, 0x01, 0x02],
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Transport {
    Abridged,
    Intermediate,
    PaddedIntermediate,
}

impl Transport {
    #[must_use]
    pub fn tag(self) -> [u8; 4] {
        match self {
            Self::Abridged => PROTO_ABRIDGED,
            Self::Intermediate => PROTO_INTERMEDIATE,
            Self::PaddedIntermediate => PROTO_PADDED_INTERMEDIATE,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientInit {
    pub dc: i16,
    pub media: bool,
    pub transport: Transport,
    pub prekey_iv: [u8; PREKEY_LEN + IV_LEN],
}

#[must_use]
pub fn parse_client_init(handshake: &[u8; HANDSHAKE_LEN], secret: &[u8; 16]) -> Option<ClientInit> {
    let prekey_iv: [u8; PREKEY_LEN + IV_LEN] = handshake[SKIP_LEN..SKIP_LEN + PREKEY_LEN + IV_LEN]
        .try_into()
        .ok()?;
    let prekey: &[u8; PREKEY_LEN] = prekey_iv[..PREKEY_LEN].try_into().ok()?;
    let iv: &[u8; IV_LEN] = prekey_iv[PREKEY_LEN..].try_into().ok()?;

    let mut hasher = Sha256::new();
    hasher.update(prekey);
    hasher.update(secret);
    let key: [u8; 32] = hasher.finalize().into();

    let mut decrypted = *handshake;
    new_aes_ctr(&key, iv).apply_keystream(&mut decrypted);

    let tag: [u8; 4] = decrypted[PROTO_TAG_POS..PROTO_TAG_POS + 4]
        .try_into()
        .ok()?;
    let transport = match tag {
        PROTO_ABRIDGED => Transport::Abridged,
        PROTO_INTERMEDIATE => Transport::Intermediate,
        PROTO_PADDED_INTERMEDIATE => Transport::PaddedIntermediate,
        _ => return None,
    };

    let dc_index = i16::from_le_bytes(decrypted[DC_POS..DC_POS + 2].try_into().ok()?);
    let dc = dc_index.checked_abs()?;
    if dc == 0 {
        return None;
    }

    Some(ClientInit {
        dc,
        media: dc_index < 0,
        transport,
        prekey_iv,
    })
}

pub fn generate_relay_init(transport: Transport, dc: i16, media: bool) -> io::Result<[u8; 64]> {
    let dc_index = if media {
        dc.checked_neg()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid DC number"))?
    } else {
        dc
    };

    let mut init = [0_u8; HANDSHAKE_LEN];
    loop {
        getrandom::fill(&mut init).map_err(io::Error::other)?;
        let start: [u8; 4] = init[..4].try_into().expect("fixed slice");
        if !RESERVED_FIRST_BYTES.contains(&init[0])
            && !RESERVED_STARTS.contains(&start)
            && init[4..8] != [0, 0, 0, 0]
        {
            break;
        }
    }

    let key: [u8; 32] = init[8..40].try_into().expect("fixed slice");
    let iv: [u8; 16] = init[40..56].try_into().expect("fixed slice");
    let mut encrypted = init;
    new_aes_ctr(&key, &iv).apply_keystream(&mut encrypted);

    let mut tail = [0_u8; 8];
    tail[..4].copy_from_slice(&transport.tag());
    tail[4..6].copy_from_slice(&dc_index.to_le_bytes());
    getrandom::fill(&mut tail[6..]).map_err(io::Error::other)?;
    for index in 0..tail.len() {
        let keystream = encrypted[56 + index] ^ init[56 + index];
        init[56 + index] = tail[index] ^ keystream;
    }
    Ok(init)
}

#[must_use]
pub fn build_crypto_context(
    client_prekey_iv: &[u8; 48],
    secret: &[u8; 16],
    relay_init: &[u8; 64],
) -> CryptoContext {
    let client_prekey: &[u8; 32] = client_prekey_iv[..32].try_into().expect("fixed slice");
    let client_iv: &[u8; 16] = client_prekey_iv[32..].try_into().expect("fixed slice");

    let mut hasher = Sha256::new();
    hasher.update(client_prekey);
    hasher.update(secret);
    let client_decrypt_key: [u8; 32] = hasher.finalize().into();

    let mut reversed = *client_prekey_iv;
    reversed.reverse();
    let reversed_prekey: &[u8; 32] = reversed[..32].try_into().expect("fixed slice");
    let reversed_iv: &[u8; 16] = reversed[32..].try_into().expect("fixed slice");
    let mut hasher = Sha256::new();
    hasher.update(reversed_prekey);
    hasher.update(secret);
    let client_encrypt_key: [u8; 32] = hasher.finalize().into();

    let relay_encrypt_key: [u8; 32] = relay_init[8..40].try_into().expect("fixed slice");
    let relay_encrypt_iv: [u8; 16] = relay_init[40..56].try_into().expect("fixed slice");

    let mut relay_reverse: [u8; 48] = relay_init[8..56].try_into().expect("fixed slice");
    relay_reverse.reverse();
    let relay_decrypt_key: [u8; 32] = relay_reverse[..32].try_into().expect("fixed slice");
    let relay_decrypt_iv: [u8; 16] = relay_reverse[32..].try_into().expect("fixed slice");

    let mut client_decrypt = new_aes_ctr(&client_decrypt_key, client_iv);
    let client_encrypt = new_aes_ctr(&client_encrypt_key, reversed_iv);
    let mut telegram_encrypt = new_aes_ctr(&relay_encrypt_key, &relay_encrypt_iv);
    let telegram_decrypt = new_aes_ctr(&relay_decrypt_key, &relay_decrypt_iv);

    client_decrypt.apply_keystream(&mut ZERO_64.clone());
    telegram_encrypt.apply_keystream(&mut ZERO_64.clone());

    CryptoContext {
        upstream: UpstreamCrypto {
            client_decrypt,
            telegram_encrypt,
        },
        downstream: DownstreamCrypto {
            telegram_decrypt,
            client_encrypt,
        },
    }
}

pub struct MessageSplitter {
    decrypt: crate::crypto::Aes256Ctr,
    transport: Transport,
    cipher_buffer: Vec<u8>,
    plain_buffer: Vec<u8>,
    disabled: bool,
    max_buffer: usize,
}

impl MessageSplitter {
    #[must_use]
    pub fn new(relay_init: &[u8; 64], transport: Transport, max_buffer: usize) -> Self {
        let key: [u8; 32] = relay_init[8..40].try_into().expect("fixed slice");
        let iv: [u8; 16] = relay_init[40..56].try_into().expect("fixed slice");
        let mut decrypt = new_aes_ctr(&key, &iv);
        decrypt.apply_keystream(&mut ZERO_64.clone());
        Self {
            decrypt,
            transport,
            cipher_buffer: Vec::new(),
            plain_buffer: Vec::new(),
            disabled: false,
            max_buffer,
        }
    }

    pub fn split(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        if chunk.is_empty() {
            return Vec::new();
        }
        if self.disabled {
            return vec![chunk.to_vec()];
        }
        let mut plain = chunk.to_vec();
        self.decrypt.apply_keystream(&mut plain);
        self.split_plain_and_cipher(&plain, chunk)
    }

    /// Splits bytes that have already been decrypted from the client and
    /// re-encrypted for Telegram, avoiding a second AES-CTR pass.
    pub fn split_reencrypted(&mut self, plain_chunk: &[u8], cipher_chunk: &[u8]) -> Vec<Vec<u8>> {
        assert_eq!(
            plain_chunk.len(),
            cipher_chunk.len(),
            "plain and encrypted MTProto chunks must have equal lengths"
        );
        if cipher_chunk.is_empty() {
            return Vec::new();
        }
        if self.disabled {
            return vec![cipher_chunk.to_vec()];
        }
        self.split_plain_and_cipher(plain_chunk, cipher_chunk)
    }

    fn split_plain_and_cipher(&mut self, plain_chunk: &[u8], cipher_chunk: &[u8]) -> Vec<Vec<u8>> {
        self.cipher_buffer.extend_from_slice(cipher_chunk);
        self.plain_buffer.extend_from_slice(plain_chunk);

        if self.cipher_buffer.len() > self.max_buffer {
            self.disabled = true;
            return self.flush();
        }

        let mut parts = Vec::new();
        let mut offset = 0;
        while offset < self.cipher_buffer.len() {
            let Some(packet_len) = self.next_packet_len(offset) else {
                break;
            };
            if packet_len == 0 || packet_len > self.max_buffer {
                self.disabled = true;
                parts.push(self.cipher_buffer[offset..].to_vec());
                offset = self.cipher_buffer.len();
                break;
            }
            if self.cipher_buffer.len() - offset < packet_len {
                break;
            }
            parts.push(self.cipher_buffer[offset..offset + packet_len].to_vec());
            offset += packet_len;
        }
        if offset > 0 {
            self.cipher_buffer.drain(..offset);
            self.plain_buffer.drain(..offset);
        }
        parts
    }

    pub fn flush(&mut self) -> Vec<Vec<u8>> {
        if self.cipher_buffer.is_empty() {
            return Vec::new();
        }
        self.plain_buffer.clear();
        vec![std::mem::take(&mut self.cipher_buffer)]
    }

    fn next_packet_len(&self, offset: usize) -> Option<usize> {
        let available = self.plain_buffer.len().checked_sub(offset)?;
        match self.transport {
            Transport::Abridged => {
                let first = *self.plain_buffer.get(offset)?;
                let (header, payload) = if first == 0x7f || first == 0xff {
                    if available < 4 {
                        return None;
                    }
                    let words = u32::from_le_bytes([
                        self.plain_buffer[offset + 1],
                        self.plain_buffer[offset + 2],
                        self.plain_buffer[offset + 3],
                        0,
                    ]);
                    (4_usize, usize::try_from(words).ok()?.checked_mul(4)?)
                } else {
                    (1_usize, usize::from(first & 0x7f).checked_mul(4)?)
                };
                if payload == 0 {
                    return Some(0);
                }
                header.checked_add(payload)
            }
            Transport::Intermediate | Transport::PaddedIntermediate => {
                if available < 4 {
                    return None;
                }
                let payload = u32::from_le_bytes(
                    self.plain_buffer[offset..offset + 4]
                        .try_into()
                        .expect("checked slice"),
                ) & 0x7fff_ffff;
                let payload = usize::try_from(payload).ok()?;
                if payload == 0 {
                    return Some(0);
                }
                4_usize.checked_add(payload)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_python_compatible_handshake_vector() {
        let secret = hex::decode("00112233445566778899aabbccddeeff").unwrap();
        let secret: [u8; 16] = secret.try_into().unwrap();
        let handshake = hex::decode(
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f\
             202122232425262728292a2b2c2d2e2f3031323334353637bc0e7366c174d0db",
        )
        .unwrap();
        let handshake: [u8; 64] = handshake.try_into().unwrap();
        let parsed = parse_client_init(&handshake, &secret).unwrap();
        assert_eq!(parsed.dc, 4);
        assert!(parsed.media);
        assert_eq!(parsed.transport, Transport::Intermediate);
        assert_eq!(
            hex::encode(parsed.prekey_iv),
            "08090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f2021222324252627\
             28292a2b2c2d2e2f3031323334353637"
        );
    }

    #[test]
    fn rejects_wrong_secret_and_zero_dc() {
        let secret = [0_u8; 16];
        let handshake = [0_u8; 64];
        assert!(parse_client_init(&handshake, &secret).is_none());
    }

    #[test]
    fn crypto_context_matches_python_golden_vector() {
        let secret: [u8; 16] = hex::decode("00112233445566778899aabbccddeeff")
            .unwrap()
            .try_into()
            .unwrap();
        let handshake: [u8; 64] = hex::decode(
            "010102034142434408090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f\
             202122232425262728292a2b2c2d2e2f30313233343536378f3d4055c174d035",
        )
        .unwrap()
        .try_into()
        .unwrap();
        let parsed = parse_client_init(&handshake, &secret).unwrap();
        assert_eq!(parsed.dc, 4);
        assert!(parsed.media);
        assert_eq!(parsed.transport, Transport::PaddedIntermediate);

        let relay_init: [u8; 64] = hex::decode(
            "010102034142434408090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f\
             202122232425262728292a2b2c2d2e2f3031323334353637752b166de2ff69d9",
        )
        .unwrap()
        .try_into()
        .unwrap();
        let mut context = build_crypto_context(&parsed.prekey_iv, &secret, &relay_init);

        let mut upload = hex::decode("884e6246ef8d821c09c17679acd4c613").unwrap();
        context.upstream.client_decrypt.apply_keystream(&mut upload);
        assert_eq!(hex::encode(&upload), "112233445566778899aabbccddeeff00");
        context
            .upstream
            .telegram_encrypt
            .apply_keystream(&mut upload);
        assert_eq!(hex::encode(upload), "238bf26c6d9712c9bb1b2b71c04c4d0b");

        let mut download = hex::decode("d0733ebc65669fc8e2bf64a739046a69").unwrap();
        context
            .downstream
            .telegram_decrypt
            .apply_keystream(&mut download);
        assert_eq!(hex::encode(&download), "ffeeddccbbaa99887766554433221100");
        context
            .downstream
            .client_encrypt
            .apply_keystream(&mut download);
        assert_eq!(hex::encode(download), "f70f3013b1d97504f27da6683e4ed144");
    }

    #[test]
    fn generated_relay_init_encodes_transport_dc_and_media_sign() {
        for transport in [
            Transport::Abridged,
            Transport::Intermediate,
            Transport::PaddedIntermediate,
        ] {
            for media in [false, true] {
                let init = generate_relay_init(transport, 4, media).unwrap();
                let key: [u8; 32] = init[8..40].try_into().unwrap();
                let iv: [u8; 16] = init[40..56].try_into().unwrap();
                let mut decrypted = init;
                new_aes_ctr(&key, &iv).apply_keystream(&mut decrypted);

                assert_eq!(&decrypted[56..60], &transport.tag());
                let encoded_dc = i16::from_le_bytes(decrypted[60..62].try_into().unwrap());
                assert_eq!(encoded_dc, if media { -4 } else { 4 });
                assert!(!RESERVED_FIRST_BYTES.contains(&init[0]));
                assert!(!RESERVED_STARTS.contains(&init[..4].try_into().unwrap()));
                assert_ne!(&init[4..8], &[0, 0, 0, 0]);
            }
        }
    }
}

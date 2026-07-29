use aes::Aes256;
use ctr::cipher::{KeyIvInit, StreamCipher};

pub type Aes256Ctr = ctr::Ctr128BE<Aes256>;

#[must_use]
pub fn new_aes_ctr(key: &[u8; 32], iv: &[u8; 16]) -> Aes256Ctr {
    Aes256Ctr::new(key.into(), iv.into())
}

pub fn apply(cipher: &mut Aes256Ctr, data: &mut [u8]) {
    cipher.apply_keystream(data);
}

pub struct UpstreamCrypto {
    pub client_decrypt: Aes256Ctr,
    pub telegram_encrypt: Aes256Ctr,
}

pub struct DownstreamCrypto {
    pub telegram_decrypt: Aes256Ctr,
    pub client_encrypt: Aes256Ctr,
}

pub struct CryptoContext {
    pub upstream: UpstreamCrypto,
    pub downstream: DownstreamCrypto,
}

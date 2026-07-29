use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const TLS_RECORD_HANDSHAKE: u8 = 0x16;
const TLS_RECORD_CHANGE_CIPHER_SPEC: u8 = 0x14;
const TLS_RECORD_APPLICATION_DATA: u8 = 0x17;
const MAX_TLS_RECORD: usize = 18_432;
const TLS_APP_DATA_MAX: usize = 16_384;
const TIMESTAMP_TOLERANCE_SECONDS: i64 = 120;

const CLIENT_RANDOM_OFFSET: usize = 11;
const CLIENT_RANDOM_LENGTH: usize = 32;
const SESSION_ID_OFFSET: usize = 44;
const SESSION_ID_LENGTH: usize = 32;

const SERVER_RANDOM_OFFSET: usize = 11;
const SERVER_SESSION_ID_OFFSET: usize = 44;
const SERVER_PUBLIC_KEY_OFFSET: usize = 89;

const SERVER_HELLO_PREFIX: &[u8] = &[
    0x16, 0x03, 0x03, 0x00, 0x7a, 0x02, 0x00, 0x00, 0x76, 0x03, 0x03,
];
const CHANGE_CIPHER_SPEC: &[u8] = &[0x14, 0x03, 0x03, 0x00, 0x01, 0x01];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedClientHello {
    pub client_random: [u8; CLIENT_RANDOM_LENGTH],
    pub session_id: [u8; SESSION_ID_LENGTH],
    pub timestamp: u32,
}

#[must_use]
pub fn verify_client_hello(
    data: &[u8],
    secret: &[u8; 16],
    now: SystemTime,
) -> Option<VerifiedClientHello> {
    if data.len() < 43 || data[0] != TLS_RECORD_HANDSHAKE || data[5] != 0x01 {
        return None;
    }
    let declared_record = usize::from(u16::from_be_bytes([data[3], data[4]]));
    if declared_record.checked_add(5)? != data.len() || declared_record > MAX_TLS_RECORD {
        return None;
    }
    let declared_handshake =
        (usize::from(data[6]) << 16) | (usize::from(data[7]) << 8) | usize::from(data[8]);
    if declared_handshake.checked_add(4)? != declared_record {
        return None;
    }

    let client_random: [u8; 32] = data
        [CLIENT_RANDOM_OFFSET..CLIENT_RANDOM_OFFSET + CLIENT_RANDOM_LENGTH]
        .try_into()
        .ok()?;
    let mut authenticated = data.to_vec();
    authenticated[CLIENT_RANDOM_OFFSET..CLIENT_RANDOM_OFFSET + CLIENT_RANDOM_LENGTH].fill(0);

    let mut mac = Hmac::<Sha256>::new_from_slice(secret).ok()?;
    mac.update(&authenticated);
    let expected = mac.finalize().into_bytes();
    if !bool::from(expected[..28].ct_eq(&client_random[..28])) {
        return None;
    }
    let timestamp_bytes =
        std::array::from_fn(|index| client_random[28 + index] ^ expected[28 + index]);
    let timestamp = u32::from_le_bytes(timestamp_bytes);
    let now_seconds = now.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let delta = i128::from(timestamp) - i128::from(now_seconds);
    if delta.abs() > i128::from(TIMESTAMP_TOLERANCE_SECONDS) {
        return None;
    }

    let session_id =
        if data.get(43) == Some(&0x20) && data.len() >= SESSION_ID_OFFSET + SESSION_ID_LENGTH {
            data[SESSION_ID_OFFSET..SESSION_ID_OFFSET + SESSION_ID_LENGTH]
                .try_into()
                .ok()?
        } else {
            [0_u8; SESSION_ID_LENGTH]
        };
    Some(VerifiedClientHello {
        client_random,
        session_id,
        timestamp,
    })
}

pub fn build_server_hello(
    secret: &[u8; 16],
    client_random: &[u8; 32],
    session_id: &[u8; 32],
) -> io::Result<Vec<u8>> {
    let mut server_hello = Vec::with_capacity(127);
    server_hello.extend_from_slice(SERVER_HELLO_PREFIX);
    server_hello.extend_from_slice(&[0_u8; 32]);
    server_hello.push(0x20);
    server_hello.extend_from_slice(&[0_u8; 32]);
    server_hello.extend_from_slice(&[
        0x13, 0x01, 0x00, 0x00, 0x2e, 0x00, 0x33, 0x00, 0x24, 0x00, 0x1d, 0x00, 0x20,
    ]);
    server_hello.extend_from_slice(&[0_u8; 32]);
    server_hello.extend_from_slice(&[0x00, 0x2b, 0x00, 0x02, 0x03, 0x04]);
    debug_assert_eq!(server_hello.len(), 127);
    server_hello[SERVER_SESSION_ID_OFFSET..SERVER_SESSION_ID_OFFSET + 32]
        .copy_from_slice(session_id);
    getrandom::fill(&mut server_hello[SERVER_PUBLIC_KEY_OFFSET..SERVER_PUBLIC_KEY_OFFSET + 32])
        .map_err(io::Error::other)?;

    let mut random = [0_u8; 2];
    getrandom::fill(&mut random).map_err(io::Error::other)?;
    let encrypted_size = 1900 + usize::from(u16::from_le_bytes(random)) % 201;
    let mut encrypted = vec![0_u8; encrypted_size];
    getrandom::fill(&mut encrypted).map_err(io::Error::other)?;

    let mut response = server_hello;
    response.extend_from_slice(CHANGE_CIPHER_SPEC);
    response.extend_from_slice(&[0x17, 0x03, 0x03]);
    response.extend_from_slice(
        &u16::try_from(encrypted_size)
            .expect("padding is smaller than u16")
            .to_be_bytes(),
    );
    response.extend_from_slice(&encrypted);

    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts every key length");
    mac.update(client_random);
    mac.update(&response);
    let server_random = mac.finalize().into_bytes();
    response[SERVER_RANDOM_OFFSET..SERVER_RANDOM_OFFSET + 32].copy_from_slice(&server_random);
    Ok(response)
}

pub struct FakeTlsReader<R> {
    inner: R,
    buffered: Vec<u8>,
    offset: usize,
}

impl<R> FakeTlsReader<R>
where
    R: AsyncRead + Unpin,
{
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            buffered: Vec::new(),
            offset: 0,
        }
    }

    pub async fn read(&mut self, destination: &mut [u8]) -> io::Result<usize> {
        if destination.is_empty() {
            return Ok(0);
        }
        if self.offset < self.buffered.len() {
            return Ok(self.copy_buffered(destination));
        }
        loop {
            let mut header = [0_u8; 5];
            match self.inner.read_exact(&mut header).await {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(0),
                Err(error) => return Err(error),
            }
            let record_type = header[0];
            let length = usize::from(u16::from_be_bytes([header[3], header[4]]));
            if length > MAX_TLS_RECORD {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Fake TLS record exceeds maximum length",
                ));
            }
            let mut payload = vec![0_u8; length];
            self.inner.read_exact(&mut payload).await?;
            if record_type == TLS_RECORD_CHANGE_CIPHER_SPEC {
                continue;
            }
            if record_type != TLS_RECORD_APPLICATION_DATA {
                return Ok(0);
            }
            self.buffered = payload;
            self.offset = 0;
            return Ok(self.copy_buffered(destination));
        }
    }

    pub async fn read_exact(&mut self, destination: &mut [u8]) -> io::Result<()> {
        let mut offset = 0;
        while offset < destination.len() {
            let read = self.read(&mut destination[offset..]).await?;
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Fake TLS stream ended early",
                ));
            }
            offset += read;
        }
        Ok(())
    }

    fn copy_buffered(&mut self, destination: &mut [u8]) -> usize {
        let available = self.buffered.len() - self.offset;
        let length = available.min(destination.len());
        destination[..length].copy_from_slice(&self.buffered[self.offset..self.offset + length]);
        self.offset += length;
        if self.offset == self.buffered.len() {
            self.buffered.clear();
            self.offset = 0;
        }
        length
    }
}

pub struct FakeTlsWriter<W> {
    inner: W,
}

impl<W> FakeTlsWriter<W>
where
    W: AsyncWrite + Unpin,
{
    pub fn new(inner: W) -> Self {
        Self { inner }
    }

    pub async fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        for chunk in data.chunks(TLS_APP_DATA_MAX) {
            self.inner.write_all(&[0x17, 0x03, 0x03]).await?;
            self.inner
                .write_all(
                    &u16::try_from(chunk.len())
                        .expect("TLS chunks fit in u16")
                        .to_be_bytes(),
                )
                .await?;
            self.inner.write_all(chunk).await?;
        }
        self.inner.flush().await
    }

    pub async fn shutdown(&mut self) -> io::Result<()> {
        self.inner.shutdown().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signed_client_hello(secret: &[u8; 16], timestamp: u32) -> Vec<u8> {
        let mut hello = vec![0_u8; 76];
        hello[0..3].copy_from_slice(&[TLS_RECORD_HANDSHAKE, 0x03, 0x01]);
        hello[3..5].copy_from_slice(&71_u16.to_be_bytes());
        hello[5] = 0x01;
        hello[6..9].copy_from_slice(&[0, 0, 67]);
        hello[9..11].copy_from_slice(&[0x03, 0x03]);
        hello[43] = 0x20;
        hello[SESSION_ID_OFFSET..SESSION_ID_OFFSET + SESSION_ID_LENGTH].fill(0x42);

        let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
        mac.update(&hello);
        let expected = mac.finalize().into_bytes();
        hello[CLIENT_RANDOM_OFFSET..CLIENT_RANDOM_OFFSET + 28].copy_from_slice(&expected[..28]);
        let encoded_timestamp = timestamp.to_le_bytes();
        for index in 0..4 {
            hello[CLIENT_RANDOM_OFFSET + 28 + index] =
                encoded_timestamp[index] ^ expected[28 + index];
        }
        hello
    }

    #[test]
    fn rejects_inconsistent_record_length() {
        let mut hello = vec![0_u8; 43];
        hello[0] = TLS_RECORD_HANDSHAKE;
        hello[5] = 1;
        hello[3..5].copy_from_slice(&100_u16.to_be_bytes());
        assert!(verify_client_hello(&hello, &[0; 16], SystemTime::now()).is_none());
    }

    #[test]
    fn server_hello_has_consistent_record_sizes() {
        let response = build_server_hello(&[7; 16], &[8; 32], &[9; 32]).unwrap();
        assert_eq!(&response[..5], &[0x16, 0x03, 0x03, 0x00, 0x7a]);
        assert_eq!(&response[127..133], CHANGE_CIPHER_SPEC);
        let app_len = usize::from(u16::from_be_bytes([response[136], response[137]]));
        assert_eq!(response.len(), 138 + app_len);
    }

    #[test]
    fn verifies_current_signed_hello_and_rejects_tampering() {
        let secret = [0x33; 16];
        let timestamp = 1_800_000_000_u32;
        let now = UNIX_EPOCH + std::time::Duration::from_secs(u64::from(timestamp));
        let mut hello = signed_client_hello(&secret, timestamp);
        let verified = verify_client_hello(&hello, &secret, now).unwrap();
        assert_eq!(verified.timestamp, timestamp);
        assert_eq!(verified.session_id, [0x42; 32]);

        hello[75] ^= 1;
        assert!(verify_client_hello(&hello, &secret, now).is_none());
        let stale = now + std::time::Duration::from_secs(121);
        let fresh = signed_client_hello(&secret, timestamp);
        assert!(verify_client_hello(&fresh, &secret, stale).is_none());
    }
}

use std::io;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

use crate::fake_tls::{FakeTlsReader, FakeTlsWriter};

pub enum ClientReader {
    Plain(OwnedReadHalf),
    FakeTls(FakeTlsReader<OwnedReadHalf>),
}

impl ClientReader {
    pub async fn read(&mut self, destination: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(reader) => reader.read(destination).await,
            Self::FakeTls(reader) => reader.read(destination).await,
        }
    }

    pub async fn read_exact(&mut self, destination: &mut [u8]) -> io::Result<()> {
        match self {
            Self::Plain(reader) => reader.read_exact(destination).await.map(|_| ()),
            Self::FakeTls(reader) => reader.read_exact(destination).await,
        }
    }
}

pub enum ClientWriter {
    Plain(OwnedWriteHalf),
    FakeTls(FakeTlsWriter<OwnedWriteHalf>),
}

impl ClientWriter {
    pub async fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        match self {
            Self::Plain(writer) => {
                writer.write_all(data).await?;
                writer.flush().await
            }
            Self::FakeTls(writer) => writer.write_all(data).await,
        }
    }

    pub async fn shutdown(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(writer) => writer.shutdown().await,
            Self::FakeTls(writer) => writer.shutdown().await,
        }
    }
}

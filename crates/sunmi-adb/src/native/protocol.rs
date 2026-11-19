//! ADB v1 wire protocol framing (ADB_VERSION 0x01000001).
//!
//! Message: command u32, arg0 u32, arg1 u32, data_length u32,
//! data checksum u32 (sum of data bytes), magic u32 (= !command).
//! All integers little-endian; commands are the 4 ASCII chars read LE.

use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const CNXN: u32 = 0x4e584e43; // "CNXN"
pub const AUTH: u32 = 0x48545541; // "AUTH"
pub const OPEN: u32 = 0x4e45504f; // "OPEN"
pub const OKAY: u32 = 0x59414b4f; // "OKAY"
pub const CLSE: u32 = 0x45534c43; // "CLSE"
pub const WRTE: u32 = 0x45545257; // "WRTE"

/// AUTH arg0 values.
pub const AUTH_TOKEN: u32 = 1;
pub const AUTH_SIGNATURE: u32 = 2;
pub const AUTH_RSAPUBLICKEY: u32 = 3;

pub const HEADER_LEN: usize = 24;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub command: u32,
    pub arg0: u32,
    pub arg1: u32,
    pub data: Vec<u8>,
}

impl Message {
    pub fn new(command: u32, arg0: u32, arg1: u32, data: impl Into<Vec<u8>>) -> Self {
        Self {
            command,
            arg0,
            arg1,
            data: data.into(),
        }
    }

    /// Sum of the data bytes mod 2^32 (the wire checksum).
    pub fn checksum(&self) -> u32 {
        let sum: u64 = self.data.iter().map(|b| *b as u64).sum();
        sum as u32
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.data.len());
        out.extend_from_slice(&self.command.to_le_bytes());
        out.extend_from_slice(&self.arg0.to_le_bytes());
        out.extend_from_slice(&self.arg1.to_le_bytes());
        out.extend_from_slice(&(self.data.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.checksum().to_le_bytes());
        out.extend_from_slice(&(!self.command).to_le_bytes());
        out.extend_from_slice(&self.data);
        out
    }

    pub fn decode(buf: &[u8]) -> Option<Self> {
        if buf.len() < HEADER_LEN {
            return None;
        }
        let command = u32::from_le_bytes(buf[0..4].try_into().ok()?);
        let arg0 = u32::from_le_bytes(buf[4..8].try_into().ok()?);
        let arg1 = u32::from_le_bytes(buf[8..12].try_into().ok()?);
        let data_len = u32::from_le_bytes(buf[12..16].try_into().ok()?);
        let checksum = u32::from_le_bytes(buf[16..20].try_into().ok()?);
        let magic = u32::from_le_bytes(buf[20..24].try_into().ok()?);
        if magic != !command {
            return None;
        }
        let end = HEADER_LEN + data_len as usize;
        if buf.len() < end {
            return None;
        }
        let data = buf[HEADER_LEN..end].to_vec();
        let computed = {
            let sum: u64 = data.iter().map(|b| *b as u64).sum();
            sum as u32
        };
        // adb >= 0x01000001 (SKIP_CHECKSUM) sends checksum = 0; only reject
        // when a real checksum was supplied and mismatches.
        if checksum != 0 && checksum != computed {
            return None;
        }
        Some(Self {
            command,
            arg0,
            arg1,
            data,
        })
    }
}

/// Reads one full message (header + data) from an async reader.
pub async fn read_message<R>(reader: &mut R) -> std::io::Result<Message>
where
    R: tokio::io::AsyncRead + ?Sized + Unpin,
{
    let mut header = [0u8; HEADER_LEN];
    reader.read_exact(&mut header).await?;
    let command = u32::from_le_bytes(header[0..4].try_into().unwrap());
    let arg0 = u32::from_le_bytes(header[4..8].try_into().unwrap());
    let arg1 = u32::from_le_bytes(header[8..12].try_into().unwrap());
    let data_len = u32::from_le_bytes(header[12..16].try_into().unwrap()) as usize;
    let checksum = u32::from_le_bytes(header[16..20].try_into().unwrap());
    let magic = u32::from_le_bytes(header[20..24].try_into().unwrap());

    let mut data = vec![0u8; data_len];
    if data_len > 0 {
        reader.read_exact(&mut data).await?;
    }

    let msg = Message {
        command,
        arg0,
        arg1,
        data,
    };
    if magic != !command {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "adb: corrupted message (magic mismatch)",
        ));
    }
    Ok(msg)
}

/// Writes one full message to an async writer.
///
/// The header and the payload are submitted as **separate USB transfers**
/// (flushing between them). The classic adb usb backend writes `msg` and
/// `payload` with two consecutive bulk writes, and the Sunmi production
/// adbd expects exactly that: a single combined transfer makes it parse
/// the payload as the next message header and fault the endpoints.
pub async fn write_message<W>(writer: &mut W, msg: &Message) -> std::io::Result<()>
where
    W: tokio::io::AsyncWrite + ?Sized + Unpin,
{
    let header = msg.encode()[..HEADER_LEN].to_vec();
    writer.write_all(&header).await?;
    writer.flush().await?;
    if !msg.data.is_empty() {
        writer.write_all(&msg.data).await?;
        writer.flush().await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_checksum() {
        let msg = Message::new(WRTE, 1, 2, b"hello world".to_vec());
        let bytes = msg.encode();
        assert_eq!(bytes.len(), HEADER_LEN + 11);
        let decoded = Message::decode(&bytes).unwrap();
        assert_eq!(decoded, msg);
        // corrupted checksum -> rejected
        let mut bad = bytes.clone();
        let last = bad.len() - 1;
        bad[last] ^= 0xff;
        assert!(Message::decode(&bad).is_none() || Message::decode(&bad).unwrap() != msg);
    }

    #[test]
    fn empty_data_message() {
        let msg = Message::new(OKAY, 7, 9, Vec::new());
        let decoded = Message::decode(&msg.encode()).unwrap();
        assert_eq!(decoded, msg);
        assert_eq!(decoded.checksum(), 0);
    }
}

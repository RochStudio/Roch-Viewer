//! The frames between the window and the helper.
//!
//! One request, one response, in order, over a pair of pipes. Both ends are the
//! same build of the same program when `pkexec` starts this executable, but an
//! installed helper can be older than the window: the hello carries
//! [`VERSION`], and a mismatch is treated as no helper rather than guessed at.
//!
//! ```text
//! request:  op:u8  length:u32  payload
//! response: ok:u8  length:u32  payload   (ok = 0: payload is the error text)
//! ```

use std::io::{self, Read, Write};

use super::super::KernelSmbus;

pub const VERSION: u32 = 1;

/// What the helper says first: this, then its [`VERSION`], then a response
/// frame saying whether it could get access.
pub const HELLO: &[u8; 8] = b"ROCHHW\r\n";

/// The most one frame may carry. Nothing legitimate comes close: the largest
/// thing read is an SMBIOS table.
pub const MAX_FRAME: usize = 1 << 24;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    ReadPhys { address: u64, size: u32 },
    GetPhysLong { address: u64 },
    SetPhysLong { address: u64, value: u32 },
    Inb { port: u16 },
    Outb { port: u16, value: u8 },
    Inl { port: u16 },
    Outl { port: u16, value: u32 },
    ReadFirmwareFile { path: String },
    KernelSmbus { adapter: u32, address: u8, transfer: KernelSmbus },
    KernelEcByte { register: u8 },
}

fn io_error(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

impl Request {
    pub fn encode(&self) -> Vec<u8> {
        let mut payload = Vec::with_capacity(16);
        let op = match self {
            Request::ReadPhys { address, size } => {
                payload.extend_from_slice(&address.to_le_bytes());
                payload.extend_from_slice(&size.to_le_bytes());
                1
            }
            Request::GetPhysLong { address } => {
                payload.extend_from_slice(&address.to_le_bytes());
                2
            }
            Request::SetPhysLong { address, value } => {
                payload.extend_from_slice(&address.to_le_bytes());
                payload.extend_from_slice(&value.to_le_bytes());
                3
            }
            Request::Inb { port } => {
                payload.extend_from_slice(&port.to_le_bytes());
                4
            }
            Request::Outb { port, value } => {
                payload.extend_from_slice(&port.to_le_bytes());
                payload.push(*value);
                5
            }
            Request::Inl { port } => {
                payload.extend_from_slice(&port.to_le_bytes());
                6
            }
            Request::Outl { port, value } => {
                payload.extend_from_slice(&port.to_le_bytes());
                payload.extend_from_slice(&value.to_le_bytes());
                7
            }
            Request::ReadFirmwareFile { path } => {
                payload.extend_from_slice(path.as_bytes());
                8
            }
            Request::KernelSmbus { adapter, address, transfer } => {
                payload.extend_from_slice(&adapter.to_le_bytes());
                payload.push(*address);
                let (kind, register, value) = match *transfer {
                    KernelSmbus::ReadByte { register } => (0, register, 0),
                    KernelSmbus::ReadWord { register } => (1, register, 0),
                    KernelSmbus::WriteByte { register, value } => (2, register, value),
                    KernelSmbus::SendByte { value } => (3, 0, value),
                };
                payload.extend_from_slice(&[kind, register, value]);
                9
            }
            Request::KernelEcByte { register } => {
                payload.push(*register);
                10
            }
        };
        frame(op, &payload)
    }

    pub fn decode(op: u8, payload: &[u8]) -> io::Result<Request> {
        let bad = || io_error(format!("malformed request {op}"));
        let u16_at = |at: usize| -> io::Result<u16> {
            Ok(u16::from_le_bytes(payload.get(at..at + 2).ok_or_else(bad)?.try_into().map_err(|_| bad())?))
        };
        let u32_at = |at: usize| -> io::Result<u32> {
            Ok(u32::from_le_bytes(payload.get(at..at + 4).ok_or_else(bad)?.try_into().map_err(|_| bad())?))
        };
        let u64_at = |at: usize| -> io::Result<u64> {
            Ok(u64::from_le_bytes(payload.get(at..at + 8).ok_or_else(bad)?.try_into().map_err(|_| bad())?))
        };
        let u8_at = |at: usize| -> io::Result<u8> { payload.get(at).copied().ok_or_else(bad) };
        Ok(match op {
            1 => Request::ReadPhys { address: u64_at(0)?, size: u32_at(8)? },
            2 => Request::GetPhysLong { address: u64_at(0)? },
            3 => Request::SetPhysLong { address: u64_at(0)?, value: u32_at(8)? },
            4 => Request::Inb { port: u16_at(0)? },
            5 => Request::Outb { port: u16_at(0)?, value: u8_at(2)? },
            6 => Request::Inl { port: u16_at(0)? },
            7 => Request::Outl { port: u16_at(0)?, value: u32_at(2)? },
            8 => Request::ReadFirmwareFile {
                path: String::from_utf8(payload.to_vec()).map_err(|_| bad())?,
            },
            9 => Request::KernelSmbus {
                adapter: u32_at(0)?,
                address: u8_at(4)?,
                transfer: match u8_at(5)? {
                    0 => KernelSmbus::ReadByte { register: u8_at(6)? },
                    1 => KernelSmbus::ReadWord { register: u8_at(6)? },
                    2 => KernelSmbus::WriteByte { register: u8_at(6)?, value: u8_at(7)? },
                    3 => KernelSmbus::SendByte { value: u8_at(7)? },
                    _ => return Err(bad()),
                },
            },
            10 => Request::KernelEcByte { register: u8_at(0)? },
            _ => return Err(bad()),
        })
    }
}

/// One frame: its tag byte, its length, its payload.
pub fn frame(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(tag);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// Read one frame: `(tag, payload)`.
pub fn read_frame(reader: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    let mut head = [0u8; 5];
    reader.read_exact(&mut head)?;
    let length = u32::from_le_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if length > MAX_FRAME {
        return Err(io_error(format!("a frame of {length} bytes")));
    }
    let mut payload = vec![0u8; length];
    reader.read_exact(&mut payload)?;
    Ok((head[0], payload))
}

/// Write a response frame carrying `result`.
pub fn write_response(writer: &mut impl Write, result: Result<Vec<u8>, String>) -> io::Result<()> {
    let bytes = match result {
        Ok(payload) => frame(1, &payload),
        Err(error) => frame(0, error.as_bytes()),
    };
    writer.write_all(&bytes)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(request: Request) {
        let bytes = request.encode();
        let (op, payload) = read_frame(&mut bytes.as_slice()).unwrap();
        assert_eq!(Request::decode(op, &payload).unwrap(), request);
    }

    #[test]
    fn every_request_survives_the_pipe() {
        round_trip(Request::ReadPhys { address: 0xFEDC_E000, size: 8 });
        round_trip(Request::GetPhysLong { address: 0xC00F_C020 });
        round_trip(Request::SetPhysLong { address: 0xC000_0060, value: 0x3B1_0524 });
        round_trip(Request::Inb { port: 0x5000 });
        round_trip(Request::Outb { port: 0x2E, value: 0x87 });
        round_trip(Request::Inl { port: 0xCFC });
        round_trip(Request::Outl { port: 0xCF8, value: 0x8000_0060 });
        round_trip(Request::ReadFirmwareFile { path: "/sys/firmware/dmi/tables/DMI".into() });
        round_trip(Request::KernelEcByte { register: 0x33 });
        for transfer in [
            KernelSmbus::ReadByte { register: 0x31 },
            KernelSmbus::ReadWord { register: 0x05 },
            KernelSmbus::WriteByte { register: 0x30, value: 0x05 },
            KernelSmbus::SendByte { value: 0 },
        ] {
            round_trip(Request::KernelSmbus { adapter: 22, address: 0x48, transfer });
        }
    }

    #[test]
    fn a_truncated_request_is_refused_rather_than_read_as_zero() {
        assert!(Request::decode(1, &[0; 3]).is_err());
        assert!(Request::decode(99, &[]).is_err());
    }

    #[test]
    fn an_oversized_frame_is_refused_before_it_is_allocated() {
        let mut bytes = vec![1u8];
        bytes.extend_from_slice(&(MAX_FRAME as u32 + 1).to_le_bytes());
        assert!(read_frame(&mut bytes.as_slice()).is_err());
    }

    #[test]
    fn responses_carry_either_the_bytes_or_the_reason() {
        let mut out = Vec::new();
        write_response(&mut out, Ok(vec![1, 2])).unwrap();
        assert_eq!(read_frame(&mut out.as_slice()).unwrap(), (1, vec![1, 2]));
        let mut out = Vec::new();
        write_response(&mut out, Err("no".into())).unwrap();
        assert_eq!(read_frame(&mut out.as_slice()).unwrap(), (0, b"no".to_vec()));
    }
}

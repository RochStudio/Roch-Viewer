//! The privileged half: read a request, make the call, write the answer.
//!
//! Nothing here decides anything. What is permitted is [`Direct`]'s business, so
//! a request is refused the same way whichever side of the pipe it came from.

use std::io::{self, Write};

use super::super::Hardware;
use super::direct::Direct;
use super::protocol::{self, Request};

/// Serve the parent until it goes away. The process's exit code.
///
/// Stdout is the protocol, so nothing else may be written to it; the parent
/// closing its end is the normal way this stops.
pub fn serve() -> i32 {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();

    let opened = Direct::open();
    let hello = (|| -> io::Result<()> {
        output.write_all(protocol::HELLO)?;
        output.write_all(&protocol::VERSION.to_le_bytes())?;
        protocol::write_response(&mut output, opened.as_ref().map(|_| Vec::new()).map_err(Clone::clone))
    })();
    let direct = match (hello, opened) {
        (Ok(()), Ok(direct)) => direct,
        _ => return 1,
    };

    loop {
        let Ok((op, payload)) = protocol::read_frame(&mut input) else {
            return 0;
        };
        let result = match Request::decode(op, &payload) {
            Ok(request) => answer(&direct, request),
            Err(error) => Err(error.to_string()),
        };
        if protocol::write_response(&mut output, result).is_err() {
            return 0;
        }
    }
}

fn answer(direct: &Direct, request: Request) -> Result<Vec<u8>, String> {
    let result = match request {
        Request::ReadPhys { address, size } => direct.read_phys(address, size as usize),
        Request::GetPhysLong { address } => direct.get_phys_long(address).map(|value| value.to_le_bytes().to_vec()),
        Request::SetPhysLong { address, value } => direct.set_phys_long(address, value).map(|()| Vec::new()),
        Request::Inb { port } => direct.inb(port).map(|value| vec![value]),
        Request::Outb { port, value } => direct.outb(port, value).map(|()| Vec::new()),
        Request::Inl { port } => direct.inl(port).map(|value| value.to_le_bytes().to_vec()),
        Request::Outl { port, value } => direct.outl(port, value).map(|()| Vec::new()),
        Request::ReadFirmwareFile { path } => direct.read_firmware_file(&path),
        Request::KernelSmbus { adapter, address, transfer } => {
            direct.kernel_smbus(adapter, address, transfer).map(|value| value.to_le_bytes().to_vec())
        }
        Request::KernelEcByte { register } => direct.kernel_ec_byte(register).map(|value| vec![value]),
    };
    result.map_err(|error| error.0)
}

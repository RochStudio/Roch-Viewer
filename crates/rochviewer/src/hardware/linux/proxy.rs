//! The window's half: the [`Hardware`] backend that asks the helper.

use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;

use super::super::{Hardware, HwError, HwResult, KernelSmbus};
use super::protocol::{self, Request};
use super::{HELPER_ARG, INSTALLED_HELPER};

struct Channel {
    /// Kept so the helper is reaped with the viewer rather than left a zombie.
    _child: Child,
    input: ChildStdin,
    output: ChildStdout,
}

/// A running helper.
pub struct Proxy {
    channel: Mutex<Option<Channel>>,
}

/// How starting one helper went.
enum Attempt {
    Started(Proxy),
    /// It started and refused, for a reason every other attempt would also hit.
    Refused(String),
    /// This route is not available; another might be.
    NotHere(String),
}

/// Start the helper, the prompt-free way when it is installed.
pub fn start() -> Result<Proxy, String> {
    if Path::new(INSTALLED_HELPER).exists() {
        match attempt(Command::new(INSTALLED_HELPER), INSTALLED_HELPER) {
            Attempt::Started(proxy) => return Ok(proxy),
            Attempt::Refused(reason) => return Err(reason),
            // An older installed helper, or one for another user: not fatal,
            // because the prompt still works.
            Attempt::NotHere(reason) => eprintln!("{reason}"),
        }
    }
    let exe = std::env::current_exe().map_err(|error| format!("Could not find this program: {error}"))?;
    let mut pkexec = Command::new("pkexec");
    // pkexec refuses to run at all when SHELL is not listed in /etc/shells, as one
    // installed by a version manager often is, and it has no use for the value.
    pkexec.env_remove("SHELL").arg(&exe);
    match attempt(pkexec, "pkexec") {
        Attempt::Started(proxy) => Ok(proxy),
        Attempt::Refused(reason) | Attempt::NotHere(reason) => Err(reason),
    }
}

fn attempt(mut command: Command, label: &str) -> Attempt {
    command.arg(HELPER_ARG).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == io::ErrorKind::NotFound && label == "pkexec" => {
            return Attempt::NotHere(
                "Register readings need administrator access, and pkexec is not installed to ask for it. \
                 Install polkit, or run: sudo rochviewer setup"
                    .into(),
            );
        }
        Err(error) => return Attempt::NotHere(format!("Could not start {label}: {error}")),
    };
    let (Some(input), Some(mut output)) = (child.stdin.take(), child.stdout.take()) else {
        return Attempt::NotHere(format!("Could not talk to {label}"));
    };

    let mut hello = [0u8; protocol::HELLO.len()];
    if output.read_exact(&mut hello).is_err() || &hello != protocol::HELLO {
        // No hello at all: pkexec exited without running anything, which is
        // what declining the prompt looks like.
        let status = child.wait().ok().and_then(|status| status.code());
        return Attempt::NotHere(no_hello_message(label, status));
    }
    let mut version = [0u8; 4];
    if output.read_exact(&mut version).is_err() {
        return Attempt::NotHere(format!("{label} stopped before it was ready"));
    }
    let version = u32::from_le_bytes(version);
    if version != protocol::VERSION {
        let _ = child.kill();
        let _ = child.wait();
        return Attempt::NotHere(format!(
            "{INSTALLED_HELPER} speaks version {version} and this viewer speaks {}. \
             Run the setup again to update it: sudo rochviewer setup",
            protocol::VERSION
        ));
    }
    match protocol::read_frame(&mut output) {
        Ok((1, _)) => Attempt::Started(Proxy { channel: Mutex::new(Some(Channel { _child: child, input, output })) }),
        Ok((_, reason)) => {
            let _ = child.wait();
            Attempt::Refused(String::from_utf8_lossy(&reason).into_owned())
        }
        Err(error) => Attempt::NotHere(format!("{label} stopped before it was ready: {error}")),
    }
}

/// What pkexec's exit code says. 126 is a dismissed or refused prompt, 127 no
/// way to ask at all.
pub fn no_hello_message(label: &str, status: Option<i32>) -> String {
    if label != "pkexec" {
        return format!("{label} did not start ({status:?}).");
    }
    match status {
        Some(126) => "Register readings need administrator access, and the prompt was declined. \
                      Open the viewer again to be asked again."
            .into(),
        Some(127) => "Register readings need administrator access, and there is nothing running here to ask \
                      for it: no polkit agent. Run: sudo rochviewer setup"
            .into(),
        other => format!("Register readings need administrator access, and pkexec could not provide it ({other:?})."),
    }
}

impl Proxy {
    fn call(&self, request: Request) -> HwResult<Vec<u8>> {
        let mut slot = self.channel.lock().unwrap_or_else(|e| e.into_inner());
        let Some(channel) = slot.as_mut() else {
            return Err(HwError::new("The register helper has stopped."));
        };
        let exchange = (|| -> io::Result<(u8, Vec<u8>)> {
            channel.input.write_all(&request.encode())?;
            protocol::read_frame(&mut channel.output)
        })();
        match exchange {
            Ok((1, payload)) => Ok(payload),
            Ok((_, reason)) => Err(HwError(String::from_utf8_lossy(&reason).into_owned())),
            Err(error) => {
                // Nothing after this can be trusted to be in step.
                *slot = None;
                Err(HwError(format!("The register helper has stopped: {error}")))
            }
        }
    }

    fn call_fixed<const N: usize>(&self, request: Request) -> HwResult<[u8; N]> {
        self.call(request)?
            .try_into()
            .map_err(|_| HwError::new("The register helper gave an answer of the wrong size."))
    }
}

impl Hardware for Proxy {
    fn read_phys(&self, address: u64, size: usize) -> HwResult<Vec<u8>> {
        let size = u32::try_from(size).map_err(|_| HwError::new("Too large a physical read."))?;
        self.call(Request::ReadPhys { address, size })
    }

    fn get_phys_long(&self, address: u64) -> HwResult<u32> {
        self.call_fixed(Request::GetPhysLong { address }).map(u32::from_le_bytes)
    }

    fn set_phys_long(&self, address: u64, value: u32) -> HwResult<()> {
        self.call(Request::SetPhysLong { address, value }).map(|_| ())
    }

    fn inb(&self, port: u16) -> HwResult<u8> {
        self.call_fixed::<1>(Request::Inb { port }).map(|[value]| value)
    }

    fn outb(&self, port: u16, value: u8) -> HwResult<()> {
        self.call(Request::Outb { port, value }).map(|_| ())
    }

    fn inl(&self, port: u16) -> HwResult<u32> {
        self.call_fixed(Request::Inl { port }).map(u32::from_le_bytes)
    }

    fn outl(&self, port: u16, value: u32) -> HwResult<()> {
        self.call(Request::Outl { port, value }).map(|_| ())
    }

    fn is_driver_open(&self) -> bool {
        self.channel.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    fn read_firmware_file(&self, path: &str) -> HwResult<Vec<u8>> {
        self.call(Request::ReadFirmwareFile { path: path.to_string() })
    }

    fn kernel_smbus(&self, adapter: u32, address: u8, transfer: KernelSmbus) -> HwResult<u16> {
        self.call_fixed(Request::KernelSmbus { adapter, address, transfer }).map(u16::from_le_bytes)
    }

    fn kernel_ec_byte(&self, register: u8) -> HwResult<u8> {
        self.call_fixed::<1>(Request::KernelEcByte { register }).map(|[value]| value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in helper: a shell script given the protocol's bytes.
    fn script(text: &str) -> Attempt {
        let mut command = Command::new("sh");
        command.args(["-c", text]);
        attempt(command, "test helper")
    }

    const HELLO: &str = r"printf 'ROCHHW\r\n\001\000\000\000'";
    const READY: &str = r"printf '\001\000\000\000\000'";

    #[test]
    fn one_call_goes_there_and_back() {
        // A GetPhysLong is 13 bytes: tag, length, the address. It answers
        // 0x12345678, then the same request again is refused with a reason.
        let text = format!(
            r"{HELLO}; {READY}; head -c 13 >/dev/null; printf '\001\004\000\000\000\170\126\064\022'; \
              head -c 13 >/dev/null; printf '\000\002\000\000\000no'; cat >/dev/null"
        );
        let Attempt::Started(proxy) = script(&text) else { panic!("the helper was not taken up") };
        assert!(proxy.is_driver_open());
        assert_eq!(proxy.get_phys_long(0xFEDC_0000), Ok(0x1234_5678));
        assert_eq!(proxy.get_phys_long(0xFEDC_0000), Err(HwError::new("no")));
        assert!(proxy.is_driver_open(), "a refusal does not end the session");
    }

    #[test]
    fn a_helper_that_stops_is_reported_once_and_not_written_to_again() {
        let text = format!("{HELLO}; {READY}; exit 0");
        let Attempt::Started(proxy) = script(&text) else { panic!("the helper was not taken up") };
        assert!(proxy.get_phys_long(0).unwrap_err().0.contains("stopped"));
        assert!(!proxy.is_driver_open());
        assert_eq!(proxy.inb(0x2E), Err(HwError::new("The register helper has stopped.")));
    }

    #[test]
    fn an_answer_of_the_wrong_size_is_refused_rather_than_read_as_zero() {
        let text = format!(r"{HELLO}; {READY}; cat <&0 >/dev/null & printf '\001\001\000\000\000\377'; wait");
        let Attempt::Started(proxy) = script(&text) else { panic!("the helper was not taken up") };
        assert!(proxy.get_phys_long(0).unwrap_err().0.contains("wrong size"));
    }

    #[test]
    fn a_helper_that_cannot_get_access_says_why() {
        let text = format!(r"{HELLO}; printf '\000\004\000\000\000nope'");
        match script(&text) {
            Attempt::Refused(reason) => assert_eq!(reason, "nope"),
            _ => panic!("that is an answer, not a failure to start"),
        }
    }

    #[test]
    fn an_older_helper_is_not_trusted_to_speak_this_protocol() {
        let text = r"printf 'ROCHHW\r\n\377\000\000\000'; sleep 5";
        match script(text) {
            Attempt::NotHere(reason) => assert!(reason.contains("version 255"), "{reason}"),
            _ => panic!("a different version must not be used"),
        }
    }

    #[test]
    fn no_hello_at_all_is_not_a_helper() {
        assert!(matches!(script("exit 126"), Attempt::NotHere(_)));
        assert!(matches!(script(r"printf 'garbage!'; exit 0"), Attempt::NotHere(_)));
    }

    #[test]
    fn a_declined_prompt_says_so_rather_than_failing_vaguely() {
        assert!(no_hello_message("pkexec", Some(126)).contains("declined"));
        assert!(no_hello_message("pkexec", Some(127)).contains("polkit agent"));
        assert!(no_hello_message("pkexec", Some(1)).contains("could not provide"));
    }
}

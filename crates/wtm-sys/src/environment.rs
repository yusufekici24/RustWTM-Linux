//! A process's environment variables, read from its process parameters as
//! Process Explorer's Environment tab shows them.
//!
//! Environment variables often hold secrets (tokens, keys, passwords), so
//! they are read only with the rights of whoever asks: never through a
//! privileged helper such as the service, which runs as SYSTEM and would
//! show one user another's. Needs
//! [`Access::READ_PARAMETERS`](crate::control::Access::READ_PARAMETERS).

use crate::Result;
use crate::control::{Architecture, ProcessHandle, unsupported, utf16_units};

/// The most of an environment block read. Its size comes from the target's
/// memory, so it is not trusted; real blocks are a few kilobytes.
const MOST_BYTES: usize = 1 << 20;

impl ProcessHandle {
    /// The process's environment variables as `(name, value)` pairs, in the
    /// block's order. 32-bit (WOW64) processes are read from their own
    /// 32-bit parameters, which is where they change them.
    pub fn environment(&self) -> Result<Vec<(String, String)>> {
        let (address, size) = match self.peb32_address()? {
            // 32-bit layouts: PEB32.ProcessParameters at 0x10, and
            // RTL_USER_PROCESS_PARAMETERS32.Environment at 0x48 and
            // EnvironmentSize at 0x290.
            Some(peb32) => {
                let parameters = self.remote_u32(peb32 + 0x10)? as usize;
                (
                    self.remote_u32(parameters + 0x48)? as usize,
                    self.remote_u32(parameters + 0x290)? as usize,
                )
            }
            None => {
                if self
                    .architecture()
                    .is_ok_and(|a| a == Architecture::X86 || a == Architecture::Arm)
                {
                    return Err(unsupported());
                }
                // 64-bit layouts: PEB.ProcessParameters at 0x20, and
                // RTL_USER_PROCESS_PARAMETERS.Environment at 0x80 and
                // EnvironmentSize at 0x3F0.
                let parameters = self.remote_word(self.peb_address()? + 0x20)?;
                (
                    self.remote_word(parameters + 0x80)?,
                    self.remote_word(parameters + 0x3F0)?,
                )
            }
        };
        if address == 0 || size == 0 {
            return Err(unsupported());
        }
        let mut bytes = vec![0u8; size.min(MOST_BYTES) & !1];
        self.read_remote(address, &mut bytes)?;
        Ok(parse_block(&utf16_units(&bytes)))
    }
}

/// `NAME=value` strings, each ended by a NUL, the block by an empty one.
/// A string cut off by the end of `units` is dropped. Names may start with
/// `=`, as the hidden per-drive directories (`=C:=C:\Users`) do.
pub(crate) fn parse_block(units: &[u16]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, &unit) in units.iter().enumerate() {
        if unit != 0 {
            continue;
        }
        if i == start {
            break;
        }
        let entry = &units[start..i];
        let split = entry
            .iter()
            .skip(1)
            .position(|&u| u == u16::from(b'='))
            .map_or(entry.len(), |p| p + 1);
        let value = entry.get(split + 1..).unwrap_or_default();
        out.push((
            crate::utf16_lossy(&entry[..split]),
            crate::utf16_lossy(value),
        ));
        start = i + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::Access;

    fn block(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    #[test]
    fn parses_a_block_up_to_its_empty_string() {
        let vars = parse_block(&block("=C:=C:\\Users\0PATH=a;b\0EMPTY=\0X=1=2\0\0JUNK=1\0"));
        let expect = [
            ("=C:", "C:\\Users"),
            ("PATH", "a;b"),
            ("EMPTY", ""),
            ("X", "1=2"),
        ];
        assert_eq!(
            vars,
            expect.map(|(n, v)| (n.to_owned(), v.to_owned())).to_vec()
        );
        // A string without '=' is a name without a value.
        assert_eq!(
            parse_block(&block("ODD\0\0")),
            vec![("ODD".to_owned(), String::new())]
        );
        // A string cut off by the end of what was read is dropped.
        assert_eq!(
            parse_block(&block("A=1\0B=tru")),
            vec![("A".to_owned(), "1".to_owned())]
        );
        assert!(parse_block(&[]).is_empty() && parse_block(&[0, 0]).is_empty());
    }

    /// A child started with a known variable; `pause` waits on the open
    /// pipe until it is ended.
    fn child_with_variable(program: &str) -> std::process::Child {
        std::process::Command::new(program)
            .args(["/d", "/c", "pause"])
            .env("RUSTWTM_ENV_TEST", "one=two; three")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap()
    }

    fn read_until_initialized(pid: u32) -> Vec<(String, String)> {
        let h = ProcessHandle::open(pid, Access::READ_PARAMETERS).unwrap();
        // A WOW64 child sets up its 32-bit parameters as it starts.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Ok(vars) = h.environment()
                && vars.iter().any(|(n, _)| n == "RUSTWTM_ENV_TEST")
            {
                return vars;
            }
            assert!(std::time::Instant::now() < deadline, "never readable");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    fn reads_a_native_and_a_32_bit_childs_variables() {
        let mut programs = vec![r"C:\Windows\System32\cmd.exe"];
        if std::path::Path::new(r"C:\Windows\SysWOW64\cmd.exe").exists() {
            programs.push(r"C:\Windows\SysWOW64\cmd.exe");
        }
        for program in programs {
            let mut child = child_with_variable(program);
            let vars = read_until_initialized(child.id());
            let _ = child.kill();
            let _ = child.wait();
            let value = vars
                .iter()
                .find(|(n, _)| n == "RUSTWTM_ENV_TEST")
                .map(|(_, v)| v.as_str());
            assert_eq!(value, Some("one=two; three"), "{program}");
            assert!(
                vars.iter().any(|(n, _)| n.eq_ignore_ascii_case("PATH")),
                "{program}"
            );
        }
    }

    #[test]
    fn query_rights_alone_cannot_read_it() {
        let h = ProcessHandle::open(std::process::id(), Access::QUERY).unwrap();
        assert!(h.environment().is_err());
    }
}

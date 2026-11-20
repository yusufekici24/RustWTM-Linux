//! A process's environment block (/proc/PID/environ): readable for one's
//! own processes, and for all as root.

use crate::Result;
use crate::control::ProcessHandle;

impl ProcessHandle {
    pub fn environment(&self) -> Result<Vec<(String, String)>> {
        self.check()?;
        let raw =
            std::fs::read(self.proc_path("environ")).map_err(|e| crate::Error::io("environ", e))?;
        Ok(parse_block(&raw))
    }
}

pub(crate) fn parse_block(raw: &[u8]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = raw
        .split(|b| *b == 0)
        .filter(|kv| !kv.is_empty())
        .filter_map(|kv| {
            let kv = String::from_utf8_lossy(kv);
            let (k, v) = kv.split_once('=')?;
            Some((k.to_owned(), v.to_owned()))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::Access;

    #[test]
    fn parses_and_reads_self() {
        assert_eq!(
            parse_block(b"B=2\0A=1\0\0"),
            vec![("A".into(), "1".into()), ("B".into(), "2".into())]
        );
        let h = ProcessHandle::open(std::process::id(), Access::QUERY).unwrap();
        assert!(h.environment().unwrap().iter().any(|(k, _)| k == "PATH"));
    }
}

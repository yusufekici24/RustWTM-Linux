use std::io;

/// Errors from the Linux back end. The predicates match the Windows ones so
/// callers classify failures the same way on both systems.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{call} failed: {}", crate::linux::util::errno_text(*errno))]
    Os { call: &'static str, errno: i32 },

    #[error("{call}: {source}")]
    Io {
        call: &'static str,
        #[source]
        source: io::Error,
    },

    #[error("{0} is not supported on Linux")]
    Unsupported(&'static str),

    #[error("{0}")]
    Message(String),

    #[error("process {pid} exited and its PID now belongs to another process")]
    Reused { pid: u32 },
}

impl Error {
    pub(crate) fn io(call: &'static str, source: io::Error) -> Self {
        Self::Io { call, source }
    }

    fn errno(&self) -> Option<i32> {
        match self {
            Error::Os { errno, .. } => Some(*errno),
            Error::Io { source, .. } => source.raw_os_error().or(match source.kind() {
                io::ErrorKind::PermissionDenied => Some(libc::EACCES),
                io::ErrorKind::NotFound => Some(libc::ENOENT),
                _ => None,
            }),
            _ => None,
        }
    }

    pub fn is_access_denied(&self) -> bool {
        matches!(self.errno(), Some(libc::EACCES | libc::EPERM))
    }

    /// The user declined an authentication prompt (pkexec exit 126).
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Error::Message(m) if m.contains("dismissed") || m.contains("Not authorized"))
            || matches!(self.errno(), Some(libc::ECANCELED))
    }

    /// A PID that does not exist, as `OpenProcess` reports it on Windows.
    pub fn is_invalid_parameter(&self) -> bool {
        matches!(
            self.errno(),
            Some(libc::ESRCH | libc::ENOENT | libc::EINVAL)
        )
    }
}

impl From<io::Error> for Error {
    fn from(source: io::Error) -> Self {
        Self::Io { call: "io", source }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_errno() {
        assert!(
            Error::Os {
                call: "x",
                errno: libc::EPERM
            }
            .is_access_denied()
        );
        assert!(
            Error::Os {
                call: "x",
                errno: libc::ESRCH
            }
            .is_invalid_parameter()
        );
        let nf = Error::from(io::Error::from(io::ErrorKind::NotFound));
        assert!(nf.is_invalid_parameter() && !nf.is_access_denied());
        assert!(!Error::Unsupported("x").is_access_denied());
    }
}

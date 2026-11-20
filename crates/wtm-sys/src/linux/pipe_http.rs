//! Bounded, read-only HTTP GET over a local Unix socket (the Docker
//! engine's `/var/run/docker.sock`), under one deadline for the exchange.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

pub fn get(pipe: &str, path: &str, timeout: Duration, limit: usize) -> io::Result<Vec<u8>> {
    if !path.starts_with('/')
        || !path.bytes().all(|b| b.is_ascii_graphic())
        || limit == 0
        || limit > 4 * 1024 * 1024
        || timeout.is_zero()
        || timeout > Duration::from_secs(3)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid Docker request bounds",
        ));
    }
    let socket = crate::pipe::socket_path(pipe);
    let deadline = Instant::now() + timeout;
    let mut stream = UnixStream::connect(&socket)?;
    let left = || {
        let d = deadline.saturating_duration_since(Instant::now());
        if d.is_zero() {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Docker request timed out",
            ))
        } else {
            Ok(d)
        }
    };
    stream.set_write_timeout(Some(left()?))?;
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: docker\r\nConnection: close\r\nAccept: application/json\r\n\r\n"
    );
    stream.write_all(request.as_bytes())?;
    let mut response = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        stream.set_read_timeout(Some(left()?))?;
        let n = match stream.read(&mut buf) {
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Docker request timed out",
                ));
            }
            Err(e) => return Err(e),
        };
        if n == 0 {
            break;
        }
        if response.len() + n > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Docker response is too large",
            ));
        }
        response.extend_from_slice(&buf[..n]);
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_from_a_socket() {
        let path = std::env::temp_dir().join(format!("wtm-http-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let l = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let t = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut b = [0u8; 256];
            let _ = s.read(&mut b).unwrap();
            s.write_all(b"HTTP/1.1 200 OK\r\n\r\n[]").unwrap();
        });
        let r = get(
            path.to_str().unwrap(),
            "/containers/json",
            Duration::from_secs(2),
            1024,
        )
        .unwrap();
        assert!(r.ends_with(b"[]"));
        t.join().unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(get("/x", "/", Duration::from_secs(9), 10).is_err());
    }
}

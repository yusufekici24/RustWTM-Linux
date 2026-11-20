//! The Linux counterpart of DPAPI: secrets sealed with ChaCha20-Poly1305
//! under a random 256-bit key only the current user can read
//! (`$XDG_CONFIG_HOME/rustwtm/seal.key`, mode 0600; for root
//! `/var/lib/rustwtm/seal.key`). `entropy` is bound as associated data, so
//! a secret sealed for one purpose cannot be opened for another.

use std::fs;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};

use crate::{Error, Result};

const MAGIC: &[u8; 4] = b"WTS1";

fn key_path() -> PathBuf {
    if crate::linux::util::is_root() {
        return PathBuf::from("/var/lib/rustwtm/seal.key");
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("rustwtm").join("seal.key")
}

fn random(buf: &mut [u8]) -> Result<()> {
    let mut f = fs::File::open("/dev/urandom").map_err(|e| Error::io("/dev/urandom", e))?;
    f.read_exact(buf).map_err(|e| Error::io("/dev/urandom", e))
}

fn key(create: bool) -> Result<[u8; 32]> {
    let path = key_path();
    if let Ok(bytes) = fs::read(&path)
        && bytes.len() == 32
    {
        let mut k = [0u8; 32];
        k.copy_from_slice(&bytes);
        return Ok(k);
    }
    if !create {
        return Err(Error::Message("no sealing key for this user".into()));
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| Error::io("create key dir", e))?;
    }
    let mut k = [0u8; 32];
    random(&mut k)?;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| Error::io("create key", e))?;
    std::io::Write::write_all(&mut f, &k).map_err(|e| Error::io("write key", e))?;
    Ok(k)
}

pub fn protect(secret: &[u8], entropy: &[u8]) -> Result<Vec<u8>> {
    let k = key(true)?;
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&k));
    let mut nonce = [0u8; 12];
    random(&mut nonce)?;
    let sealed = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: secret,
                aad: entropy,
            },
        )
        .map_err(|_| Error::Message("sealing failed".into()))?;
    let mut out = Vec::with_capacity(4 + 12 + sealed.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&nonce);
    out.extend(sealed);
    Ok(out)
}

pub fn unprotect(sealed: &[u8], entropy: &[u8]) -> Result<Vec<u8>> {
    if sealed.len() < 16 + 16 || &sealed[..4] != MAGIC {
        return Err(Error::Message("not a sealed secret".into()));
    }
    let k = key(false)?;
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&k));
    cipher
        .decrypt(
            Nonce::from_slice(&sealed[4..16]),
            Payload {
                msg: &sealed[16..],
                aad: entropy,
            },
        )
        .map_err(|_| Error::Message("the secret cannot be opened by this user".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seals_and_binds_entropy() {
        let s = protect(b"api-key", b"purpose").unwrap();
        assert_eq!(unprotect(&s, b"purpose").unwrap(), b"api-key");
        assert!(unprotect(&s, b"other").is_err());
        let mut bad = s.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert!(unprotect(&bad, b"purpose").is_err());
    }
}

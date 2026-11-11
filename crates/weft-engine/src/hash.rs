//! Whole-file checksums.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use md5::Md5;
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Algorithm {
    Sha256,
    Sha1,
    Md5,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checksum {
    pub algorithm: Algorithm,
    /// Lowercase hex.
    pub hex: String,
}

impl Checksum {
    /// Accepts `sha256:<hex>`, `sha1:<hex>`, `md5:<hex>` or a bare hex digest whose
    /// length identifies the algorithm.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let (alg, hex) = match s.split_once([':', '=']) {
            Some((a, h)) => (Some(a.trim().to_ascii_lowercase().replace('-', "")), h.trim()),
            None => (None, s),
        };
        if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let algorithm = match (alg.as_deref(), hex.len()) {
            (Some("sha256") | None, 64) => Algorithm::Sha256,
            (Some("sha1") | None, 40) => Algorithm::Sha1,
            (Some("md5") | None, 32) => Algorithm::Md5,
            _ => return None,
        };
        Some(Self { algorithm, hex: hex.to_ascii_lowercase() })
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn run<D: Digest>(file: &mut File, progress: &mut dyn FnMut(u64)) -> io::Result<String> {
    let mut hasher = D::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        progress(n as u64);
    }
    Ok(hex(&hasher.finalize()))
}

/// Hashes a file from start to end (blocking).
pub fn file_digest(path: &Path, algorithm: Algorithm, mut progress: impl FnMut(u64)) -> io::Result<String> {
    let mut file = File::open(path)?;
    match algorithm {
        Algorithm::Sha256 => run::<Sha256>(&mut file, &mut progress),
        Algorithm::Sha1 => run::<Sha1>(&mut file, &mut progress),
        Algorithm::Md5 => run::<Md5>(&mut file, &mut progress),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_hashes() {
        let c = Checksum::parse("SHA-256:E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855")
            .unwrap();
        assert_eq!(c.algorithm, Algorithm::Sha256);
        assert_eq!(Checksum::parse("d41d8cd98f00b204e9800998ecf8427e").unwrap().algorithm, Algorithm::Md5);
        assert!(Checksum::parse("sha1:abc").is_none());
        assert!(Checksum::parse("zz").is_none());
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("e");
        std::fs::write(&p, b"").unwrap();
        assert_eq!(file_digest(&p, Algorithm::Sha256, |_| {}).unwrap(), c.hex);
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(file_digest(&p, Algorithm::Md5, |_| {}).unwrap(), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            file_digest(&p, Algorithm::Sha1, |_| {}).unwrap(),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
    }
}

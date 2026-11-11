//! Keeping cookies and credentials out of plain-text files.
//!
//! On Windows values are sealed with DPAPI, so only the same user account on the
//! same machine can read them back. Elsewhere nothing is written at all: a
//! download that needs a cookie to resume asks for a fresh link instead.

/// Seals `plain`. Returns `None` when this platform has no safe way to do it.
pub fn seal(plain: &[u8]) -> Option<Vec<u8>> {
    imp::seal(plain)
}

/// Opens something sealed by [`seal`] on this machine for this user.
pub fn open(sealed: &[u8]) -> Option<Vec<u8>> {
    imp::open(sealed)
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };

    fn run(input: &[u8], protect: bool) -> Option<Vec<u8>> {
        let blob_in = CRYPT_INTEGER_BLOB {
            cbData: u32::try_from(input.len()).ok()?,
            pbData: input.as_ptr().cast_mut(),
        };
        let mut blob_out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        // SAFETY: both blobs are valid for the call; the output buffer is owned by
        // the system and released with LocalFree after copying.
        unsafe {
            let ok = if protect {
                CryptProtectData(
                    &raw const blob_in,
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &raw mut blob_out,
                )
            } else {
                CryptUnprotectData(
                    &raw const blob_in,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &raw mut blob_out,
                )
            };
            if ok == 0 || blob_out.pbData.is_null() {
                return None;
            }
            let out = std::slice::from_raw_parts(blob_out.pbData, blob_out.cbData as usize).to_vec();
            LocalFree(blob_out.pbData.cast());
            Some(out)
        }
    }

    pub fn seal(plain: &[u8]) -> Option<Vec<u8>> {
        run(plain, true)
    }

    pub fn open(sealed: &[u8]) -> Option<Vec<u8>> {
        run(sealed, false)
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn seal(_plain: &[u8]) -> Option<Vec<u8>> {
        None
    }

    pub fn open(_sealed: &[u8]) -> Option<Vec<u8>> {
        None
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_not_plain() {
        let secret = b"Cookie: session=abc123";
        let sealed = seal(secret).unwrap();
        assert!(!sealed.windows(6).any(|w| w == b"abc123"));
        assert_eq!(open(&sealed).unwrap(), secret);
        assert!(open(b"garbage").is_none());
    }
}

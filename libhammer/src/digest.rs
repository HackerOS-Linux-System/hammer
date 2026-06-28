use sha2::{Digest, Sha256};

/// Compute the SHA-256 digest of `data` and return it as a hex string.
///
/// # Example
/// ```
/// use libhammer::digest::sha256_hex;
/// let h = sha256_hex(b"hello");
/// assert_eq!(h.len(), 64);
/// ```
pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// Verify that `data` matches the expected `sha256` hex string.
///
/// Returns `Ok(())` on match, `Err` with a description on mismatch.
pub fn verify_sha256(data: &[u8], expected: &str) -> anyhow::Result<()> {
    let actual = sha256_hex(data);
    if actual.eq_ignore_ascii_case(expected.trim()) {
        Ok(())
    } else {
        anyhow::bail!(
            "SHA-256 mismatch: expected {}, got {}",
            expected.trim(),
            actual
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_hash() {
        // echo -n "" | sha256sum
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn verify_ok() {
        let data = b"libhammer";
        let hash = sha256_hex(data);
        assert!(verify_sha256(data, &hash).is_ok());
    }

    #[test]
    fn verify_fail() {
        assert!(verify_sha256(b"wrong", "deadbeef").is_err());
    }
}

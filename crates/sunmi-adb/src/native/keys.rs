//! ADB host key management, mirroring `~/.android/adbkey`.
//!
//! Reusing the standard adb key files means a terminal that already
//! authorized this machine authorizes the native client with no re-prompt.
//! Tokens are signed with RSASSA-PKCS1-v1_5 over SHA-1 (the device-side
//! `RSA_sign(NID_sha1, token)` equivalent).

use std::fs;
use std::path::PathBuf;

use rand::rngs::OsRng;
use rsa::pkcs1::{DecodeRsaPrivateKey, EncodeRsaPrivateKey};
use rsa::{pkcs1v15, RsaPrivateKey};
use sha1::{Digest, Sha1};
use tokio::sync::OnceCell;

use crate::process::ToolError;

static HOST_KEY: OnceCell<RsaPrivateKey> = OnceCell::const_new();

fn key_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".android")
}

fn key_path() -> PathBuf {
    key_dir().join("adbkey")
}

async fn load_or_create_key() -> Result<RsaPrivateKey, ToolError> {
    let path = key_path();
    if path.is_file() {
        let pem = fs::read_to_string(&path).map_err(|e| ToolError::Protocol(format!(
            "failed to read {}: {e}",
            path.display()
        )))?;
        let key = RsaPrivateKey::from_pkcs1_pem(&pem).or_else(|e1| {
            use rsa::pkcs8::DecodePrivateKey;
            RsaPrivateKey::from_pkcs8_pem(&pem).map_err(|e2| ToolError::Protocol(format!(
                "adbkey at {} is neither PKCS#1 nor PKCS#8: {e1} / {e2}",
                path.display()
            )))
        })?;
        return Ok(key);
    }

    // Fresh machine: generate a 2048-bit key in the standard adb layout.
    let key = RsaPrivateKey::new(&mut OsRng, 2048)
        .map_err(|e| ToolError::Protocol(format!("key generation failed: {e}")))?;
    let dir = key_dir();
    fs::create_dir_all(&dir).map_err(|e| ToolError::Io {
        program: dir.display().to_string(),
        source: e,
    })?;
    let pem = key
        .to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
        .map_err(|e| ToolError::Protocol(format!("key export failed: {e}")))?;
    fs::write(&path, pem.as_bytes()).map_err(|e| ToolError::Io {
        program: path.display().to_string(),
        source: e,
    })?;
    let pub_path = key_dir().join("adbkey.pub");
    fs::write(&pub_path, android_pubkey_file(&key)).ok();
    Ok(key)
}

/// Per-process shared host key (loaded lazily on first USB handshake).
pub async fn host_key() -> Result<&'static RsaPrivateKey, ToolError> {
    HOST_KEY
        .get_or_try_init(load_or_create_key)
        .await
}

/// RSASSA-PKCS1-v1_5 / SHA-1 signature of an AUTH token.
pub async fn sign_token(token: &[u8]) -> Result<Vec<u8>, ToolError> {
    let key = host_key().await?;
    let params = pkcs1v15::Pkcs1v15Sign::new::<Sha1>();
    let digest = Sha1::digest(token);
    key.sign(params, &digest)
        .map_err(|e| ToolError::Protocol(format!("token signing failed: {e}")))
}

/// The `adbkey.pub` payload: base64 of the Android `RSAPublicKey` struct
/// plus a `user@host` comment, as expected by adbd when saving a newly
/// authorized key.
pub async fn public_key_line() -> Result<String, ToolError> {
    let key = host_key().await?;
    Ok(android_pubkey_file(key))
}

/// Android public key encoding (`android_pubkey`): 2048-bit modulus and
/// precomputed Montgomery parameters, word-oriented little-endian.
fn android_pubkey_file(key: &RsaPrivateKey) -> String {
    use base64::Engine as _;
    use rsa::traits::PublicKeyParts as _;

    let pubk = key.to_public_key();
    let n = pubk.n(); // modulus, big-endian integer
    let e = pubk.e();

    const KEY_WORDS: usize = 64; // 2048 bits / 32
    const N_BYTES: usize = KEY_WORDS * 4; // 256

    let n_be = n.to_bytes_be();
    let mut modulus = vec![0u8; N_BYTES];
    let start = N_BYTES - n_be.len();
    modulus[start..].copy_from_slice(&n_be);

    // n0inv = -n^-1 mod 2^32, computed on the first little-endian word of n
    // (always odd for an RSA modulus) with Hensel lifting.
    let n_le = n.to_bytes_le();
    let mut word0 = [0u8; 4];
    for (dst, src) in word0.iter_mut().zip(n_le.iter()) {
        *dst = *src;
    }
    let w0 = u32::from_le_bytes(word0) as u64;
    let mut inv: u64 = 1;
    for _ in 0..5 {
        inv = inv.wrapping_mul(2u64.wrapping_sub(w0.wrapping_mul(inv)));
    }
    let n0inv = (inv as u32).wrapping_neg();

    // rr = R^2 mod n with R = 2^2048 (Montgomery constant).
    let rr_big = rsa::BigUint::from(2u8).modpow(
        &rsa::BigUint::from(4096u32),
        n,
    );
    let mut rr_le = rr_big.to_bytes_le();
    rr_le.resize(N_BYTES, 0);

    let mut encoded = Vec::with_capacity(4 + 4 + N_BYTES * 2 + 4);
    encoded.extend_from_slice(&(KEY_WORDS as u32).to_le_bytes());
    encoded.extend_from_slice(&n0inv.to_le_bytes());
    encoded.extend_from_slice(&modulus);
    encoded.extend_from_slice(&rr_le);
    // exponent: 65537 as little-endian u32
    let mut exponent = e.to_bytes_le();
    exponent.resize(4, 0);
    encoded.extend_from_slice(&exponent);

    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "sunmimgr".to_string());
    let host = hostname();
    format!(
        "{} {}@{}",
        base64::engine::general_purpose::STANDARD.encode(encoded),
        user,
        host
    )
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .unwrap_or_else(|| "localhost".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::traits::PublicKeyParts as _;

    #[test]
    fn generates_key_files_in_adb_layout() {
        // Point HOME at a temp dir so we do not touch the real ~/.android.
        let dir = std::env::temp_dir().join(format!("sunmi-adb-keys-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HOME", &dir);
        std::env::remove_var("USERPROFILE");

        let key = futures_block_on(load_or_create_key()).unwrap();
        let key_path = dir.join(".android").join("adbkey");
        assert!(key_path.is_file());

        let pem = fs::read_to_string(&key_path).unwrap();
        let reloaded = RsaPrivateKey::from_pkcs1_pem(&pem).unwrap();
        assert_eq!(key.n(), reloaded.n());
        assert!(dir.join(".android").join("adbkey.pub").is_file());

        // Deterministic auth pieces: "base64 user@host"
        let line = android_pubkey_file(&key);
        let (b64, comment) = line
            .split_once(' ')
            .expect("pubkey line must contain a space");
        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .expect("valid base64");
        // struct: len(4) + n0inv(4) + modulus(256) + rr(256) + exponent(4)
        assert_eq!(encoded.len(), 4 + 4 + 256 + 256 + 4);
        assert!(comment.contains('@'));
        let sig_len = {
            // RSA 2048 -> 256-byte signature; sign a dummy token synchronously
            // through the same code path minus the OnceCell.
            let params = pkcs1v15::Pkcs1v15Sign::new::<Sha1>();
            let digest = Sha1::digest([0u8; 20]);
            key.sign(params, &digest).unwrap().len()
        };
        assert_eq!(sig_len, 256);

        // public key parts sane
        assert_eq!(key.n().bits(), 2048);
    }

    fn futures_block_on<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(fut)
    }
}

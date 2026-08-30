use aes_gcm::aead::{Aead, KeyInit, Nonce};
use aes_gcm::{Aes256Gcm, Key};
use rand::RngCore;
use sha2::{Digest, Sha256};

use kaniop_backup_core::auth::ENCRYPTION_KEY_ENV;
use kaniop_backup_core::manifest::ClientSideEncryptionMeta;

pub const AES_256_GCM_ALGORITHM: &str = "AES-256-GCM";
pub const DEK_SIZE: usize = 32;
pub const KEK_SIZE: usize = 32;
pub const NONCE_SIZE: usize = 12;
pub const SALT_SIZE: usize = 4;
pub const TAG_SIZE: usize = 16;

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("encryption key not found: set {0} environment variable")]
    MissingKek(String),
    #[error("encryption key must be exactly {expected} bytes (got {actual})")]
    InvalidKekLength { expected: usize, actual: usize },
    #[error("encryption key decode failed: {0}")]
    KekDecode(String),
    #[error("encryption operation failed: {0}")]
    Encrypt(String),
    #[error("decryption operation failed: {0}")]
    Decrypt(String),
    #[error("wrapped DEK decode failed: {0}")]
    WrappedDekDecode(String),
    #[error("nonce salt decode failed: {0}")]
    NonceSaltDecode(String),
}

pub struct EnvelopeKeys {
    pub dek: [u8; DEK_SIZE],
    pub nonce_salt: [u8; SALT_SIZE],
}

pub fn load_kek_from_env() -> Result<[u8; KEK_SIZE], CryptoError> {
    let raw = std::env::var(ENCRYPTION_KEY_ENV)
        .map_err(|_| CryptoError::MissingKek(ENCRYPTION_KEY_ENV.to_string()))?;
    parse_kek(&raw)
}

pub fn parse_kek(raw: &str) -> Result<[u8; KEK_SIZE], CryptoError> {
    let trimmed = raw.trim();
    if trimmed.len() == KEK_SIZE {
        let mut kek = [0u8; KEK_SIZE];
        kek.copy_from_slice(trimmed.as_bytes());
        return Ok(kek);
    }
    let decoded = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, trimmed)
        .map_err(|e| CryptoError::KekDecode(format!("base64 decode failed: {e}")))?;
    if decoded.len() != KEK_SIZE {
        return Err(CryptoError::InvalidKekLength {
            expected: KEK_SIZE,
            actual: decoded.len(),
        });
    }
    let mut kek = [0u8; KEK_SIZE];
    kek.copy_from_slice(&decoded);
    Ok(kek)
}

pub fn generate_dek() -> [u8; DEK_SIZE] {
    let mut dek = [0u8; DEK_SIZE];
    rand::rng().fill_bytes(&mut dek);
    dek
}

pub fn generate_nonce_salt() -> [u8; SALT_SIZE] {
    let mut salt = [0u8; SALT_SIZE];
    rand::rng().fill_bytes(&mut salt);
    salt
}

pub fn wrap_dek(dek: &[u8; DEK_SIZE], kek: &[u8; KEK_SIZE]) -> Result<Vec<u8>, CryptoError> {
    let key: Key<Aes256Gcm> = kek.as_slice().try_into().unwrap();
    let cipher = Aes256Gcm::new(&key);
    let mut wrap_nonce_bytes = [0u8; NONCE_SIZE];
    rand::rng().fill_bytes(&mut wrap_nonce_bytes);
    let nonce: Nonce<Aes256Gcm> = wrap_nonce_bytes.as_slice().try_into().unwrap();
    let ciphertext = cipher
        .encrypt(&nonce, dek.as_ref())
        .map_err(|e| CryptoError::Encrypt(format!("DEK wrap failed: {e}")))?;
    let mut wrapped = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
    wrapped.extend_from_slice(&wrap_nonce_bytes);
    wrapped.extend_from_slice(&ciphertext);
    Ok(wrapped)
}

pub fn unwrap_dek(wrapped: &[u8], kek: &[u8; KEK_SIZE]) -> Result<[u8; DEK_SIZE], CryptoError> {
    if wrapped.len() < NONCE_SIZE + TAG_SIZE {
        return Err(CryptoError::WrappedDekDecode(format!(
            "wrapped DEK too short: {} bytes",
            wrapped.len()
        )));
    }
    let key: Key<Aes256Gcm> = kek.as_slice().try_into().unwrap();
    let cipher = Aes256Gcm::new(&key);
    let nonce: Nonce<Aes256Gcm> = wrapped[..NONCE_SIZE].try_into().unwrap();
    let plaintext = cipher
        .decrypt(&nonce, &wrapped[NONCE_SIZE..])
        .map_err(|e| CryptoError::Decrypt(format!("DEK unwrap failed: {e}")))?;
    if plaintext.len() != DEK_SIZE {
        return Err(CryptoError::Decrypt(format!(
            "unwrapped DEK has wrong length: {} (expected {DEK_SIZE})",
            plaintext.len()
        )));
    }
    let mut dek = [0u8; DEK_SIZE];
    dek.copy_from_slice(&plaintext);
    Ok(dek)
}

pub fn derive_nonce(salt: &[u8; SALT_SIZE], part_index: u64) -> [u8; NONCE_SIZE] {
    let mut nonce = [0u8; NONCE_SIZE];
    nonce[..SALT_SIZE].copy_from_slice(salt);
    nonce[SALT_SIZE..].copy_from_slice(&part_index.to_le_bytes());
    nonce
}

pub fn seal_chunk(
    dek: &[u8; DEK_SIZE],
    nonce: &[u8; NONCE_SIZE],
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let key: Key<Aes256Gcm> = dek.as_slice().try_into().unwrap();
    let cipher = Aes256Gcm::new(&key);
    let nonce: Nonce<Aes256Gcm> = nonce.as_slice().try_into().unwrap();
    cipher
        .encrypt(&nonce, plaintext)
        .map_err(|e| CryptoError::Encrypt(format!("chunk seal failed: {e}")))
}

pub fn open_chunk(
    dek: &[u8; DEK_SIZE],
    nonce: &[u8; NONCE_SIZE],
    ciphertext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let key: Key<Aes256Gcm> = dek.as_slice().try_into().unwrap();
    let cipher = Aes256Gcm::new(&key);
    let nonce: Nonce<Aes256Gcm> = nonce.as_slice().try_into().unwrap();
    cipher
        .decrypt(&nonce, ciphertext)
        .map_err(|e| CryptoError::Decrypt(format!("chunk open failed: {e}")))
}

pub fn kek_fingerprint(kek: &[u8; KEK_SIZE]) -> String {
    let hash = Sha256::digest(kek);
    hex::encode(&hash[..8])
}

pub fn prepare_envelope(
    kek: &[u8; KEK_SIZE],
    chunk_size: u64,
) -> Result<(EnvelopeKeys, ClientSideEncryptionMeta), CryptoError> {
    let dek = generate_dek();
    let nonce_salt = generate_nonce_salt();
    let wrapped_dek = wrap_dek(&dek, kek)?;
    let meta = ClientSideEncryptionMeta {
        algorithm: AES_256_GCM_ALGORITHM.to_string(),
        wrapped_dek: base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            &wrapped_dek,
        ),
        nonce_salt: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, nonce_salt),
        chunk_size_bytes: chunk_size,
        kek_fingerprint: kek_fingerprint(kek),
    };
    let keys = EnvelopeKeys { dek, nonce_salt };
    Ok((keys, meta))
}

pub fn decode_nonce_salt(meta: &ClientSideEncryptionMeta) -> Result<[u8; SALT_SIZE], CryptoError> {
    let decoded =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &meta.nonce_salt)
            .map_err(|e| CryptoError::NonceSaltDecode(format!("base64 decode failed: {e}")))?;
    if decoded.len() != SALT_SIZE {
        return Err(CryptoError::NonceSaltDecode(format!(
            "nonce salt has wrong length: {} (expected {SALT_SIZE})",
            decoded.len()
        )));
    }
    let mut salt = [0u8; SALT_SIZE];
    salt.copy_from_slice(&decoded);
    Ok(salt)
}

pub fn decode_wrapped_dek(meta: &ClientSideEncryptionMeta) -> Result<Vec<u8>, CryptoError> {
    base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        &meta.wrapped_dek,
    )
    .map_err(|e| CryptoError::WrappedDekDecode(format!("base64 decode failed: {e}")))
}

pub fn load_envelope_for_upload(
    chunk_size: u64,
) -> Result<(EnvelopeKeys, ClientSideEncryptionMeta), CryptoError> {
    let kek = load_kek_from_env()?;
    prepare_envelope(&kek, chunk_size)
}

pub fn load_envelope_for_download(
    meta: &ClientSideEncryptionMeta,
) -> Result<(EnvelopeKeys, ClientSideEncryptionMeta), CryptoError> {
    let kek = load_kek_from_env()?;
    let wrapped_dek_bytes = decode_wrapped_dek(meta)?;
    let dek = unwrap_dek(&wrapped_dek_bytes, &kek)?;
    let nonce_salt = decode_nonce_salt(meta)?;
    let fingerprint = kek_fingerprint(&kek);
    if fingerprint != meta.kek_fingerprint {
        return Err(CryptoError::Decrypt(format!(
            "KEK fingerprint mismatch: manifest expects {}, derived {} (wrong KEK?)",
            meta.kek_fingerprint, fingerprint
        )));
    }
    let keys = EnvelopeKeys { dek, nonce_salt };
    Ok((keys, meta.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_kek() -> [u8; KEK_SIZE] {
        [0x42u8; KEK_SIZE]
    }

    #[test]
    fn encrypt_decrypt_single_chunk_roundtrip() {
        let kek = test_kek();
        let (keys, _meta) = prepare_envelope(&kek, 1024).unwrap();
        let nonce = derive_nonce(&keys.nonce_salt, 0);
        let plaintext = b"hello world backup data";
        let ciphertext = seal_chunk(&keys.dek, &nonce, plaintext).unwrap();
        let decrypted = open_chunk(&keys.dek, &nonce, &ciphertext).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn encrypt_decrypt_multi_chunk_roundtrip() {
        let kek = test_kek();
        let (keys, _meta) = prepare_envelope(&kek, 16).unwrap();
        let chunks: Vec<&[u8]> = vec![b"chunk-zero-data!", b"chunk-one-data!", b"last"];
        for (i, plain) in chunks.iter().enumerate() {
            let nonce = derive_nonce(&keys.nonce_salt, i as u64);
            let ct = seal_chunk(&keys.dek, &nonce, plain).unwrap();
            let pt = open_chunk(&keys.dek, &nonce, &ct).unwrap();
            assert_eq!(pt, *plain);
        }
    }

    #[test]
    fn chunk_tamper_detected() {
        let kek = test_kek();
        let (keys, _meta) = prepare_envelope(&kek, 1024).unwrap();
        let nonce = derive_nonce(&keys.nonce_salt, 0);
        let plaintext = b"important data";
        let mut ciphertext = seal_chunk(&keys.dek, &nonce, plaintext).unwrap();
        let last = ciphertext.len() - 1;
        ciphertext[last] ^= 0xFF;
        let result = open_chunk(&keys.dek, &nonce, &ciphertext);
        assert!(result.is_err());
    }

    #[test]
    fn wrong_kek_fails_unwrap() {
        let kek = test_kek();
        let (_keys, meta) = prepare_envelope(&kek, 1024).unwrap();
        let wrapped = decode_wrapped_dek(&meta).unwrap();
        let wrong_kek = [0x99u8; KEK_SIZE];
        let result = unwrap_dek(&wrapped, &wrong_kek);
        assert!(result.is_err());
    }

    #[test]
    fn nonce_uniqueness_across_parts() {
        let salt = [0xABu8; SALT_SIZE];
        let n0 = derive_nonce(&salt, 0);
        let n1 = derive_nonce(&salt, 1);
        let n2 = derive_nonce(&salt, 2);
        assert_ne!(n0, n1);
        assert_ne!(n1, n2);
        assert_ne!(n0, n2);
        assert_eq!(&n0[..SALT_SIZE], &salt);
        assert_eq!(&n1[..SALT_SIZE], &salt);
    }

    #[test]
    fn kek_length_validation_raw() {
        let short = "tooshort";
        assert!(parse_kek(short).is_err());
    }

    #[test]
    fn kek_length_validation_base64() {
        let kek = test_kek();
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, kek);
        let parsed = parse_kek(&encoded).unwrap();
        assert_eq!(parsed, kek);
    }

    #[test]
    fn kek_raw_32_bytes_accepted() {
        let raw = "A".repeat(KEK_SIZE);
        let parsed = parse_kek(&raw).unwrap();
        assert_eq!(parsed, [0x41u8; KEK_SIZE]);
    }

    #[test]
    fn wrapped_dek_unwrap_roundtrip() {
        let kek = test_kek();
        let dek = generate_dek();
        let wrapped = wrap_dek(&dek, &kek).unwrap();
        let unwrapped = unwrap_dek(&wrapped, &kek).unwrap();
        assert_eq!(unwrapped, dek);
    }

    #[test]
    fn kek_fingerprint_is_deterministic() {
        let kek = test_kek();
        let fp1 = kek_fingerprint(&kek);
        let fp2 = kek_fingerprint(&kek);
        assert_eq!(fp1, fp2);
        assert_eq!(fp1.len(), 16);
    }

    #[test]
    fn metadata_roundtrip() {
        let kek = test_kek();
        let (_keys, meta) = prepare_envelope(&kek, 8 * 1024 * 1024).unwrap();
        let json = serde_json::to_string(&meta).unwrap();
        let parsed: ClientSideEncryptionMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, meta);
        assert_eq!(parsed.algorithm, AES_256_GCM_ALGORITHM);
        assert_eq!(parsed.chunk_size_bytes, 8 * 1024 * 1024);
    }
}

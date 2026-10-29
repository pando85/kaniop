//! EAP/RADIUS certificate preflight checks.
//!
//! The official Kanidm RADIUS entrypoint expects files for a certificate chain
//! and matching private key. Reject invalid referenced Kubernetes Secret data
//! before modifying the active Deployment or Kanidm credentials.
//!
//! This only validates PEM structure, certificate validity dates and leaf/key
//! correspondence. It does not issue certificates or establish which CA a NAS
//! device trusts; clients must trust the operator-supplied certificate chain.

use std::cmp::Ordering;

use openssl::asn1::Asn1Time;
use openssl::pkey::PKey;
use openssl::x509::X509;
use thiserror::Error;

/// Errors deliberately omit certificate/key data and sensitive identifiers.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum RadiusTlsError {
    #[error("RADIUS EAP TLS certificate chain is missing or invalid")]
    InvalidCertificate,
    #[error("RADIUS EAP TLS private key is missing or invalid")]
    InvalidPrivateKey,
    #[error("RADIUS EAP TLS certificate public key does not match private key")]
    KeyMismatch,
    #[error("RADIUS EAP TLS server certificate is not yet valid")]
    NotYetValid,
    #[error("RADIUS EAP TLS server certificate has expired")]
    Expired,
    #[error("RADIUS EAP TLS CA bundle is invalid")]
    InvalidCaBundle,
}

/// Validate the expected keys from a user-owned TLS Secret.
///
/// The first certificate in cert_chain_pem must be the leaf certificate.
/// ca_bundle_pem is optional; a provided empty or invalid bundle is an error.
/// Neither source bytes nor parsed key material are retained or returned.
pub fn validate_eap_tls_material(
    cert_chain_pem: &[u8],
    private_key_pem: &[u8],
    ca_bundle_pem: Option<&[u8]>,
) -> Result<(), RadiusTlsError> {
    let chain = X509::stack_from_pem(cert_chain_pem)
        .map_err(|_| RadiusTlsError::InvalidCertificate)?;
    let leaf = chain.first().ok_or(RadiusTlsError::InvalidCertificate)?;

    let key = PKey::private_key_from_pem(private_key_pem)
        .map_err(|_| RadiusTlsError::InvalidPrivateKey)?;
    let public = leaf
        .public_key()
        .map_err(|_| RadiusTlsError::InvalidCertificate)?;
    if !public.public_eq(&key) {
        return Err(RadiusTlsError::KeyMismatch);
    }

    let now = Asn1Time::days_from_now(0).map_err(|_| RadiusTlsError::InvalidCertificate)?;
    if leaf
        .not_before()
        .compare(&now)
        .map_err(|_| RadiusTlsError::InvalidCertificate)?
        == Ordering::Greater
    {
        return Err(RadiusTlsError::NotYetValid);
    }
    if leaf
        .not_after()
        .compare(&now)
        .map_err(|_| RadiusTlsError::InvalidCertificate)?
        != Ordering::Greater
    {
        return Err(RadiusTlsError::Expired);
    }

    if let Some(ca_bundle_pem) = ca_bundle_pem {
        let certs =
            X509::stack_from_pem(ca_bundle_pem).map_err(|_| RadiusTlsError::InvalidCaBundle)?;
        if certs.is_empty() {
            return Err(RadiusTlsError::InvalidCaBundle);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use openssl::hash::MessageDigest;
    use openssl::rsa::Rsa;
    use openssl::x509::X509Name;

    fn self_signed_certificate() -> (Vec<u8>, Vec<u8>) {
        let key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
        let mut subject = X509Name::builder().unwrap();
        subject
            .append_entry_by_text("CN", "radius.example.com")
            .unwrap();
        let subject = subject.build();
        let mut cert = X509::builder().unwrap();
        cert.set_version(2).unwrap();
        cert.set_subject_name(&subject).unwrap();
        cert.set_issuer_name(&subject).unwrap();
        cert.set_pubkey(&key).unwrap();
        let not_before = Asn1Time::days_from_now(0).unwrap();
        let not_after = Asn1Time::days_from_now(30).unwrap();
        cert.set_not_before(&not_before).unwrap();
        cert.set_not_after(&not_after).unwrap();
        cert.sign(&key, MessageDigest::sha256()).unwrap();
        (
            cert.build().to_pem().unwrap(),
            key.private_key_to_pem_pkcs8().unwrap(),
        )
    }

    #[test]
    fn accepts_valid_matching_material_and_ca() {
        let (cert, key) = self_signed_certificate();
        assert_eq!(validate_eap_tls_material(&cert, &key, None), Ok(()));
        assert_eq!(
            validate_eap_tls_material(&cert, &key, Some(&cert)),
            Ok(())
        );
    }

    #[test]
    fn rejects_invalid_certificate_and_key_without_leaking_bytes() {
        let (cert, key) = self_signed_certificate();
        assert_eq!(
            validate_eap_tls_material(b"invalid cert", &key, None),
            Err(RadiusTlsError::InvalidCertificate)
        );
        assert_eq!(
            validate_eap_tls_material(&cert, b"invalid key secret", None),
            Err(RadiusTlsError::InvalidPrivateKey)
        );
        let error = validate_eap_tls_material(&cert, b"invalid key secret", None)
            .unwrap_err()
            .to_string();
        assert!(!error.contains("invalid key secret"));
    }

    #[test]
    fn rejects_mismatching_private_key_and_bad_ca() {
        let (cert, _key) = self_signed_certificate();
        let (_other_cert, other_key) = self_signed_certificate();
        assert_eq!(
            validate_eap_tls_material(&cert, &other_key, None),
            Err(RadiusTlsError::KeyMismatch)
        );
        let (cert, key) = self_signed_certificate();
        assert_eq!(
            validate_eap_tls_material(&cert, &key, Some(b"not a PEM certificate")),
            Err(RadiusTlsError::InvalidCaBundle)
        );
    }
}

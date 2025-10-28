use crate::controller::context::BackoffContext;
use crate::kanidm::reconcile::secret::REPLICA_SECRET_KEY;
use crate::metrics::ControllerMetrics;
use crate::{controller::context::Context as KaniopContext, kanidm::crd::Kanidm};
use kaniop_k8s_util::error::{Error, Result};

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::URL_SAFE};
use k8s_openapi::api::apps::v1::StatefulSet;
use k8s_openapi::api::core::v1::{Secret, Service};
use k8s_openapi::api::networking::v1::Ingress;
use kube::runtime::reflector::{ObjectRef, Store};
use openssl::asn1::Asn1Time;
use openssl::x509::X509;
use tokio::sync::RwLock;
use tracing::trace;

#[derive(Clone)]
pub struct Context {
    pub kaniop_ctx: KaniopContext<Kanidm>,
    /// Shared store
    pub stores: Arc<Stores>,
    repl_cert_exp_cache: Arc<RwLock<ReplicaCertExpiration>>,
    repl_cert_host_cache: Arc<RwLock<ReplicaCertHost>>,
}

impl Context {
    pub fn new(kaniop_ctx: KaniopContext<Kanidm>, stores: Stores) -> Self {
        Context {
            kaniop_ctx,
            stores: Arc::new(stores),
            repl_cert_exp_cache: Arc::default(),
            repl_cert_host_cache: Arc::default(),
        }
    }

    pub async fn get_repl_cert_exp(&self, secret_ref: &ObjectRef<Secret>) -> Option<i64> {
        trace!(msg = format!("getting replica certificate expiration for {secret_ref}"));
        self.repl_cert_exp_cache
            .read()
            .await
            .0
            .get(secret_ref)
            .cloned()
    }

    pub async fn insert_repl_cert_exp(&self, secret: &Secret) -> Result<()> {
        trace!(
            msg = format!(
                "inserting replica certificate expiration for {:?}",
                &ObjectRef::from(secret)
            )
        );
        match &secret.data {
            None => Err(Error::MissingData("secret data empty".to_string())),
            Some(data) => match data.get(REPLICA_SECRET_KEY) {
                None => Err(Error::MissingData(format!(
                    "secret data missing key {REPLICA_SECRET_KEY}"
                ))),
                Some(cert_b64url) => {
                    let (expiration, host) = parse_cert_expiration_and_host(
                        String::from_utf8_lossy(&cert_b64url.0).as_ref(),
                    )?;
                    let obj_ref = ObjectRef::from(secret);
                    self.repl_cert_exp_cache
                        .write()
                        .await
                        .0
                        .insert(obj_ref.clone(), expiration);
                    self.repl_cert_host_cache
                        .write()
                        .await
                        .0
                        .insert(obj_ref, host);
                    Ok(())
                }
            },
        }
    }

    #[inline]
    pub async fn remove_repl_cert_exp(&self, secret_ref: &ObjectRef<Secret>) {
        trace!(msg = format!("removing replica certificate expiration for {secret_ref}",));
        self.repl_cert_exp_cache.write().await.0.remove(secret_ref);
    }

    pub async fn get_repl_cert_host(&self, secret_ref: &ObjectRef<Secret>) -> Option<String> {
        trace!(msg = format!("getting replica certificate host for {secret_ref}"));
        self.repl_cert_host_cache
            .read()
            .await
            .0
            .get(secret_ref)
            .cloned()
    }

    #[inline]
    pub async fn remove_repl_cert_host(&self, secret_ref: &ObjectRef<Secret>) {
        trace!(msg = format!("removing replica certificate host for {secret_ref}",));
        self.repl_cert_host_cache.write().await.0.remove(secret_ref);
    }
}

impl BackoffContext<Kanidm> for Context {
    fn metrics(&self) -> &Arc<ControllerMetrics> {
        self.kaniop_ctx.metrics()
    }
    async fn get_backoff(&self, obj_ref: ObjectRef<Kanidm>) -> Duration {
        self.kaniop_ctx.get_backoff(obj_ref).await
    }

    async fn reset_backoff(&self, obj_ref: ObjectRef<Kanidm>) {
        self.kaniop_ctx.reset_backoff(obj_ref).await
    }
}

pub struct Stores {
    pub stateful_set_store: Store<StatefulSet>,
    pub service_store: Store<Service>,
    pub ingress_store: Store<Ingress>,
    pub secret_store: Store<Secret>,
}

#[derive(Default)]
struct ReplicaCertExpiration(HashMap<ObjectRef<Secret>, i64>);

#[derive(Default)]
struct ReplicaCertHost(HashMap<ObjectRef<Secret>, String>);

fn parse_cert_expiration_and_host(cert_b64url: &str) -> Result<(i64, String)> {
    let der_bytes = URL_SAFE
        .decode(cert_b64url)
        .map_err(|e| Error::ParseError(format!("invalid base64url encoding: {e}")))?;

    let cert = X509::from_der(&der_bytes)
        .map_err(|e| Error::ParseError(format!("failed to parse DER certificate: {e}")))?;
    let not_after = cert.not_after();
    trace!(msg = format!("certificate not after: {not_after}"));

    let epoch = Asn1Time::from_unix(0).unwrap();
    let duration = epoch.diff(not_after).unwrap();
    let timestamp = duration.days as i64 * 86400 + duration.secs as i64;

    let san = cert
        .subject_alt_names()
        .ok_or_else(|| Error::ParseError("no SAN extension".to_string()))?;
    let host = san
        .iter()
        .find_map(|name| {
            if let Some(dns) = name.dnsname() {
                Some(dns.to_string())
            } else if let Some(ip_bytes) = name.ipaddress() {
                if ip_bytes.len() == 4 {
                    let ip = std::net::Ipv4Addr::from(<[u8; 4]>::try_from(ip_bytes).unwrap());
                    Some(ip.to_string())
                } else if ip_bytes.len() == 16 {
                    let ip = std::net::Ipv6Addr::from(<[u8; 16]>::try_from(ip_bytes).unwrap());
                    Some(ip.to_string())
                } else {
                    None
                }
            } else {
                None
            }
        })
        .ok_or_else(|| Error::ParseError("no DNS or IP in SAN".to_string()))?;
    Ok((timestamp, host))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_cert_expiration_valid_cert() {
        let cert_b64url = "MIIB_DCCAaGgAwIBAgIBATAKBggqhkjOPQQDAjBMMRswGQYDVQQKDBJLYW5pZG0gUmVwbGljYXRpb24xLTArBgNVBAMMJDJiYTgzMTZhLWViYWEtNGJjMS04NDkzLTVmODZmYWZhZTU5NDAeFw0yNDExMDYxOTEzMjdaFw0yODExMDYxOTEzMjdaMEwxGzAZBgNVBAoMEkthbmlkbSBSZXBsaWNhdGlvbjEtMCsGA1UEAwwkMmJhODMxNmEtZWJhYS00YmMxLTg0OTMtNWY4NmZhZmFlNTk0MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEuXp1hNNZerxDQbCh7rAGW6uM0CPECNd3IvbSh7qH34MkO_plwwDVKFbzcTG8HJE2ouIJlJYN8P4wf6qmrRQMAKN0MHIwDAYDVR0TAQH_BAIwADAOBgNVHQ8BAf8EBAMCBaAwHQYDVR0lBBYwFAYIKwYBBQUHAwEGCCsGAQUFBwMCMB0GA1UdDgQWBBTaOaPuXmtLDTJVv--VYBiQr9gHCTAUBgNVHREEDTALgglsb2NhbGhvc3QwCgYIKoZIzj0EAwIDSQAwRgIhAIZD_J4LyR7D0kg41GRg_TcRxm5mEVhM6WL9BO3XmfUsAiEA7Wpbkvd0b1e-Sg8AS9jP-CpBpmTnC7oEChkyhUYKyFc=";
        let (expiration, host) = parse_cert_expiration_and_host(cert_b64url).unwrap();
        assert_eq!(expiration, 1857150807);
        assert_eq!(host, "localhost");
    }
}

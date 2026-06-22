use jiff::Timestamp;
use k8s_openapi::api::coordination::v1::Lease;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
use kube::Client;
use kube::api::{Api, Patch, PatchParams, PostParams};
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, info, warn};

#[derive(Clone)]
pub struct LeaseLock {
    client: Client,
    namespace: String,
    params: LeaseLockParams,
}

#[derive(Clone)]
pub struct LeaseLockParams {
    pub holder_id: String,
    pub lease_name: String,
    pub lease_ttl: Duration,
}

pub struct LeaseLockResult {
    pub acquired_lease: bool,
}

impl LeaseLock {
    pub fn new(client: Client, namespace: &str, params: LeaseLockParams) -> Self {
        Self {
            client,
            namespace: namespace.to_string(),
            params,
        }
    }

    pub async fn try_acquire_or_renew(&self) -> anyhow::Result<LeaseLockResult> {
        let leases: Api<Lease> = Api::namespaced(self.client.clone(), &self.namespace);
        let ttl_seconds = self.params.lease_ttl.as_secs() as i32;

        let now = Timestamp::now();
        let now_micros = MicroTime(now);

        let lease = leases.get(&self.params.lease_name).await;

        match lease {
            Ok(existing) => {
                let holder = existing
                    .spec
                    .as_ref()
                    .and_then(|s| s.holder_identity.as_ref());
                let lease_ttl = existing
                    .spec
                    .as_ref()
                    .and_then(|s| s.lease_duration_seconds);

                let is_current_holder =
                    holder.map(|h| h == &self.params.holder_id).unwrap_or(false);
                let lease_expired = lease_ttl
                    .map(|ttl| {
                        let renew_time = existing.spec.as_ref().and_then(|s| s.renew_time.as_ref());
                        if let Some(renew) = renew_time {
                            let renew_ts = renew.0;
                            let elapsed = now - renew_ts;
                            elapsed.total(jiff::Unit::Second).unwrap_or(0.0) > (ttl as f64)
                        } else {
                            true
                        }
                    })
                    .unwrap_or(true);

                if is_current_holder || lease_expired || holder.is_none() {
                    debug!(
                        lease_name = %self.params.lease_name,
                        holder_id = %self.params.holder_id,
                        is_current_holder = is_current_holder,
                        lease_expired = lease_expired,
                        "attempting to acquire/renew lease"
                    );

                    let acquire_time = if is_current_holder {
                        existing.spec.as_ref().and_then(|s| s.acquire_time.clone())
                    } else {
                        Some(now_micros.clone())
                    };

                    let patch = Lease {
                        metadata: kube::api::ObjectMeta {
                            name: Some(self.params.lease_name.clone()),
                            namespace: Some(self.namespace.clone()),
                            ..Default::default()
                        },
                        spec: Some(k8s_openapi::api::coordination::v1::LeaseSpec {
                            holder_identity: Some(self.params.holder_id.clone()),
                            lease_duration_seconds: Some(ttl_seconds),
                            acquire_time,
                            renew_time: Some(now_micros),
                            ..Default::default()
                        }),
                    };

                    let params = PatchParams::default();
                    leases
                        .patch(&self.params.lease_name, &params, &Patch::Merge(&patch))
                        .await?;

                    info!(
                        lease_name = %self.params.lease_name,
                        holder_id = %self.params.holder_id,
                        "acquired/renewed lease"
                    );

                    Ok(LeaseLockResult {
                        acquired_lease: true,
                    })
                } else {
                    debug!(
                        lease_name = %self.params.lease_name,
                        current_holder = ?holder,
                        "lease held by another instance"
                    );
                    Ok(LeaseLockResult {
                        acquired_lease: false,
                    })
                }
            }
            Err(e) => {
                if let kube::Error::Api(api_err) = &e {
                    if api_err.code == 404 {
                        debug!(
                            lease_name = %self.params.lease_name,
                            "lease does not exist, creating"
                        );

                        let new_lease = Lease {
                            metadata: kube::api::ObjectMeta {
                                name: Some(self.params.lease_name.clone()),
                                namespace: Some(self.namespace.clone()),
                                ..Default::default()
                            },
                            spec: Some(k8s_openapi::api::coordination::v1::LeaseSpec {
                                holder_identity: Some(self.params.holder_id.clone()),
                                lease_duration_seconds: Some(ttl_seconds),
                                acquire_time: Some(now_micros.clone()),
                                renew_time: Some(now_micros),
                                ..Default::default()
                            }),
                        };

                        let params = PostParams::default();
                        leases.create(&params, &new_lease).await?;

                        info!(
                            lease_name = %self.params.lease_name,
                            holder_id = %self.params.holder_id,
                            "created and acquired lease"
                        );

                        return Ok(LeaseLockResult {
                            acquired_lease: true,
                        });
                    }
                }
                warn!(
                    lease_name = %self.params.lease_name,
                    error = %e,
                    "failed to get lease"
                );
                Err(e.into())
            }
        }
    }

    pub async fn step_down(&self) -> anyhow::Result<()> {
        let leases: Api<Lease> = Api::namespaced(self.client.clone(), &self.namespace);

        let lease = leases.get(&self.params.lease_name).await?;

        let holder = lease.spec.as_ref().and_then(|s| s.holder_identity.as_ref());

        if holder.map(|h| h == &self.params.holder_id).unwrap_or(false) {
            debug!(
                lease_name = %self.params.lease_name,
                holder_id = %self.params.holder_id,
                "stepping down from leadership"
            );

            let patch = Lease {
                metadata: kube::api::ObjectMeta {
                    name: Some(self.params.lease_name.clone()),
                    namespace: Some(self.namespace.clone()),
                    ..Default::default()
                },
                spec: Some(k8s_openapi::api::coordination::v1::LeaseSpec {
                    lease_duration_seconds: Some(1),
                    holder_identity: None,
                    ..Default::default()
                }),
            };

            let params = PatchParams::default();
            leases
                .patch(&self.params.lease_name, &params, &Patch::Merge(&patch))
                .await?;

            info!(
                lease_name = %self.params.lease_name,
                "stepped down from leadership"
            );
        }

        Ok(())
    }
}

pub async fn acquire_lease_with_retry(
    lease_lock: &LeaseLock,
    retry_interval: Duration,
    max_retries: Option<u32>,
) -> anyhow::Result<()> {
    let mut retry_count = 0u32;

    loop {
        if let Some(max) = max_retries {
            if retry_count >= max {
                return Err(anyhow::anyhow!(
                    "failed to acquire lease after {} retries",
                    max
                ));
            }
        }

        let result = lease_lock.try_acquire_or_renew().await?;

        if result.acquired_lease {
            return Ok(());
        }

        retry_count += 1;
        sleep(retry_interval).await;
    }
}

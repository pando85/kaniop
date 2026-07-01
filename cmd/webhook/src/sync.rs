use crate::state::WebhookState;

use kaniop_operator::kanidm::crd::Kanidm;

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use kube::runtime::{WatchStreamExt, watcher};
use kube::{Api, Client, ResourceExt};
use tokio::time;
use tracing::{debug, error, info, trace, warn};

/// Background task that syncs internal entities from Kanidm clusters with external replication
pub async fn sync_internal_entities(state: Arc<WebhookState>, client: Client) {
    info!("Starting internal entity sync task");

    // Watch for changes to Kanidm resources
    let kanidm_api = Api::<Kanidm>::all(client.clone());
    let mut watcher_stream = watcher(kanidm_api, watcher::Config::default())
        .default_backoff()
        .boxed();

    while let Some(event) = watcher_stream.next().await {
        match event {
            Ok(event) => match event {
                watcher::Event::Apply(kanidm) => {
                    handle_kanidm_apply(state.clone(), kanidm).await;
                }
                watcher::Event::Delete(kanidm) => {
                    handle_kanidm_delete(state.clone(), kanidm).await;
                }
                _ => {}
            },
            Err(e) => {
                error!(msg = "error watching Kanidm resources", %e);
            }
        }
    }
}

/// Handle when a Kanidm resource is created or updated
async fn handle_kanidm_apply(state: Arc<WebhookState>, kanidm: Kanidm) {
    let name = kanidm.name_any();
    let namespace = match kanidm.namespace() {
        Some(ns) => ns,
        None => {
            warn!(msg = "Kanidm resource has no namespace, skipping", name);
            return;
        }
    };

    debug!(msg = "handling Kanidm apply event", namespace, name);

    // Check if this Kanidm has external replication configured
    if kanidm.spec.external_replication_nodes.is_empty() {
        trace!(
            msg = "Kanidm has no external replication, clearing cache",
            namespace, name
        );
        state.internal_cache.clear_kanidm(&name, &namespace).await;
        return;
    }

    info!(
        msg = "Kanidm has external replication enabled, syncing internal entities",
        namespace,
        name,
        external_nodes = kanidm.spec.external_replication_nodes.len()
    );

    // Spawn a background task to sync entities
    let state_clone = state.clone();
    let kanidm_clone = kanidm.clone();
    tokio::spawn(async move {
        sync_kanidm_entities(state_clone, kanidm_clone).await;
    });
}

/// Handle when a Kanidm resource is deleted
async fn handle_kanidm_delete(state: Arc<WebhookState>, kanidm: Kanidm) {
    let name = kanidm.name_any();
    let namespace = match kanidm.namespace() {
        Some(ns) => ns,
        None => {
            warn!(msg = "Kanidm resource has no namespace, skipping", name);
            return;
        }
    };

    debug!(msg = "handling Kanidm delete event", namespace, name);

    // Clear cached entities for this Kanidm cluster
    state.internal_cache.clear_kanidm(&name, &namespace).await;

    // Remove client from cache
    state
        .kanidm_client_cache
        .remove_client(&namespace, &name)
        .await;
}

/// Sync all entities from a Kanidm cluster to the internal cache
async fn sync_kanidm_entities(state: Arc<WebhookState>, kanidm: Kanidm) {
    let name = kanidm.name_any();
    let namespace = match kanidm.namespace() {
        Some(ns) => ns,
        None => {
            warn!(msg = "Kanidm resource has no namespace, skipping", name);
            return;
        }
    };

    // Wait a bit for the Kanidm cluster to be ready
    time::sleep(Duration::from_secs(5)).await;

    loop {
        match sync_kanidm_entities_once(&state, &kanidm).await {
            Ok(_) => {
                debug!(
                    msg = "successfully synced entities from Kanidm cluster",
                    namespace, name
                );
                break;
            }
            Err(e) => {
                warn!(
                    msg = "failed to sync entities from Kanidm cluster, will retry",
                    namespace,
                    name,
                    error = %e
                );
                // Wait before retrying
                time::sleep(Duration::from_secs(30)).await;
            }
        }
    }

    // Continue syncing periodically
    let mut interval = time::interval(Duration::from_secs(300)); // Sync every 5 minutes

    loop {
        interval.tick().await;

        // Check if Kanidm still exists and has external replication
        let current_kanidm = state
            .kanidm_store
            .find(|k| k.namespace().as_ref() == Some(&namespace) && k.name_any() == name);

        match current_kanidm {
            Some(k) => {
                if k.spec.external_replication_nodes.is_empty() {
                    debug!(
                        msg = "Kanidm no longer has external replication, stopping sync",
                        namespace, name
                    );
                    state.internal_cache.clear_kanidm(&name, &namespace).await;
                    break;
                }

                if let Err(e) = sync_kanidm_entities_once(&state, &k).await {
                    warn!(
                        msg = "failed to sync entities from Kanidm cluster",
                        namespace,
                        name,
                        error = %e
                    );
                }
            }
            None => {
                debug!(
                    msg = "Kanidm no longer exists, stopping sync",
                    namespace, name
                );
                break;
            }
        }
    }
}

/// Sync entities from a Kanidm cluster once
async fn sync_kanidm_entities_once(
    state: &WebhookState,
    kanidm: &Kanidm,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let name = kanidm.name_any();
    let namespace = kanidm.namespace().ok_or("Kanidm has no namespace")?;

    trace!(
        msg = "syncing entities from Kanidm cluster",
        namespace, name
    );

    // Get Kanidm client
    let client = state
        .kanidm_client_cache
        .get_client(kanidm, state.kube_client.clone())
        .await
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;

    // Clear existing cached entities for this cluster
    state.internal_cache.clear_kanidm(&name, &namespace).await;

    // Sync groups
    match client.idm_group_list().await {
        Ok(groups) => {
            debug!(
                msg = "syncing groups from Kanidm",
                namespace,
                name,
                count = groups.len()
            );
            for group in groups {
                // Extract name from Entry attributes
                if let Some(name_attr) = group.attrs.get("name") {
                    if let Some(group_name) = name_attr.first() {
                        state
                            .internal_cache
                            .add_group(group_name.clone(), name.clone(), namespace.clone())
                            .await;
                    }
                }
            }
        }
        Err(e) => {
            warn!(
                msg = "failed to list groups from Kanidm",
                namespace,
                name,
                error = ?e
            );
        }
    }

    // Sync persons
    match client.idm_person_account_list().await {
        Ok(persons) => {
            debug!(
                msg = "syncing person accounts from Kanidm",
                namespace,
                name,
                count = persons.len()
            );
            for person in persons {
                // Extract name from Entry attributes
                if let Some(name_attr) = person.attrs.get("name") {
                    if let Some(person_name) = name_attr.first() {
                        state
                            .internal_cache
                            .add_person(person_name.clone(), name.clone(), namespace.clone())
                            .await;
                    }
                }
            }
        }
        Err(e) => {
            warn!(
                msg = "failed to list person accounts from Kanidm",
                namespace,
                name,
                error = ?e
            );
        }
    }

    // Sync OAuth2 clients
    match client.idm_oauth2_rs_list().await {
        Ok(oauth2_clients) => {
            debug!(
                msg = "syncing OAuth2 clients from Kanidm",
                namespace,
                name,
                count = oauth2_clients.len()
            );
            for oauth2 in oauth2_clients {
                // Extract name from Entry attributes
                if let Some(name_attr) = oauth2.attrs.get("name") {
                    if let Some(oauth2_name) = name_attr.first() {
                        state
                            .internal_cache
                            .add_oauth2_client(oauth2_name.clone(), name.clone(), namespace.clone())
                            .await;
                    }
                }
            }
        }
        Err(e) => {
            warn!(
                msg = "failed to list OAuth2 clients from Kanidm",
                namespace,
                name,
                error = ?e
            );
        }
    }

    // Sync service accounts
    match client.idm_service_account_list().await {
        Ok(service_accounts) => {
            debug!(
                msg = "syncing service accounts from Kanidm",
                namespace,
                name,
                count = service_accounts.len()
            );
            for sa in service_accounts {
                // Extract name from Entry attributes
                if let Some(name_attr) = sa.attrs.get("name") {
                    if let Some(sa_name) = name_attr.first() {
                        state
                            .internal_cache
                            .add_service_account(sa_name.clone(), name.clone(), namespace.clone())
                            .await;
                    }
                }
            }
        }
        Err(e) => {
            warn!(
                msg = "failed to list service accounts from Kanidm",
                namespace,
                name,
                error = ?e
            );
        }
    }

    Ok(())
}

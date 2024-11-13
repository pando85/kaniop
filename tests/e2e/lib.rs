#[cfg(all(test, feature = "e2e-test"))]
mod test {
    use std::collections::BTreeMap;
    use std::sync::LazyLock;
    use std::time::Duration;

    use kaniop_k8s_util::types::short_type_name;
    use kaniop_kanidm::crd::{Kanidm, ReplicaGroup};
    use kaniop_kanidm::reconcile::secret::SecretExt;
    use kaniop_kanidm::reconcile::statefulset::StatefulSetExt;

    use futures::future::JoinAll;
    use futures::{join, AsyncBufReadExt, TryStreamExt};
    use json_patch::merge;
    use k8s_openapi::api::apps::v1::StatefulSet;
    use k8s_openapi::api::core::v1::{PersistentVolumeClaim, Pod, Secret};
    use k8s_openapi::ByteString;
    use kube::api::{Api, LogParams, ObjectMeta, Patch, PatchParams, PostParams};
    use kube::client::Client;
    use kube::runtime::wait::{await_condition, conditions, Condition};
    use kube::ResourceExt;
    use serde_json::json;
    use tokio::time::timeout;

    const CERT: &[u8] = b"-----BEGIN CERTIFICATE-----\nMIIChDCCAiugAwIBAgIBAjAKBggqhkjOPQQDAjCBhDELMAkGA1UEBhMCQVUxDDAK\nBgNVBAgMA1FMRDEPMA0GA1UECgwGS2FuaWRtMRwwGgYDVQQDDBNLYW5pZG0gR2Vu\nZXJhdGVkIENBMTgwNgYDVQQLDC9EZXZlbG9wbWVudCBhbmQgRXZhbHVhdGlvbiAt\nIE5PVCBGT1IgUFJPRFVDVElPTjAeFw0yNDEwMTMyMDQzMjhaFw0yNDEwMTgyMDQz\nMjhaMIGAMQswCQYDVQQGEwJBVTEMMAoGA1UECAwDUUxEMQ8wDQYDVQQKDAZLYW5p\nZG0xGDAWBgNVBAMMD2lkbS5leGFtcGxlLmNvbTE4MDYGA1UECwwvRGV2ZWxvcG1l\nbnQgYW5kIEV2YWx1YXRpb24gLSBOT1QgRk9SIFBST0RVQ1RJT04wWTATBgcqhkjO\nPQIBBggqhkjOPQMBBwNCAARTi7hqo0Z3BU3p95z6hQzPmYAox3bKfAAu4GmY8Qhf\nBq3TM8hf//EPcSQmbmqFUdspI0r31hfc0lIXHX5qNBaIo4GPMIGMMAkGA1UdEwQC\nMAAwDgYDVR0PAQH/BAQDAgWgMBMGA1UdJQQMMAoGCCsGAQUFBwMBMB0GA1UdDgQW\nBBQaarHTRm4Yj6TMPzvduAB7nODKHzAfBgNVHSMEGDAWgBTaOaPuXmtLDTJVv++V\nYBiQr9gHCTAaBgNVHREEEzARgg9pZG0uZXhhbXBsZS5jb20wCgYIKoZIzj0EAwID\nRwAwRAIgQpLs9MZvBRUpR15wvSwIq/QyWotvVg/3vZl8D1mTFz8CIEVbm+/+z4JL\nLYwNXnerv9Nc+anGtz+9beT4bkS4CpJS\n-----END CERTIFICATE-----\n-----BEGIN CERTIFICATE-----\nMIICPjCCAeSgAwIBAgIBATAKBggqhkjOPQQDAjCBhDELMAkGA1UEBhMCQVUxDDAK\nBgNVBAgMA1FMRDEPMA0GA1UECgwGS2FuaWRtMRwwGgYDVQQDDBNLYW5pZG0gR2Vu\nZXJhdGVkIENBMTgwNgYDVQQLDC9EZXZlbG9wbWVudCBhbmQgRXZhbHVhdGlvbiAt\nIE5PVCBGT1IgUFJPRFVDVElPTjAeFw0yNDEwMTMyMDQzMjhaFw0yNDExMTIyMDQz\nMjhaMIGEMQswCQYDVQQGEwJBVTEMMAoGA1UECAwDUUxEMQ8wDQYDVQQKDAZLYW5p\nZG0xHDAaBgNVBAMME0thbmlkbSBHZW5lcmF0ZWQgQ0ExODA2BgNVBAsML0RldmVs\nb3BtZW50IGFuZCBFdmFsdWF0aW9uIC0gTk9UIEZPUiBQUk9EVUNUSU9OMFkwEwYH\nKoZIzj0CAQYIKoZIzj0DAQcDQgAEiz5mqHozpsj5iGCDH8uSJy8TFqNIGnIw8U/L\nswyeFTGHT4S2HwBb7QAouYVuXdwL8hZGMtzAqoYMFhCt1epXjqNFMEMwEgYDVR0T\nAQH/BAgwBgEB/wIBADAOBgNVHQ8BAf8EBAMCAQYwHQYDVR0OBBYEFNo5o+5ea0sN\nMlW/75VgGJCv2AcJMAoGCCqGSM49BAMCA0gAMEUCIGyZjBs4pp1HAlFdk0mdVBz4\n440t8pRHh8/SOY5ZtMcSAiEA6qOf9aQbWwEXLj0jajX9lHgdqlwRk7wnnyLMGF5/\nlz8=\n-----END CERTIFICATE-----\n";
    const KEY: &[u8] = b"-----BEGIN PRIVATE KEY-----\nMIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgHLZoMTUMadxOKMlt\nTq/kDnuN38GCJkwj8Y2kqyGlcf+hRANCAARTi7hqo0Z3BU3p95z6hQzPmYAox3bK\nfAAu4GmY8QhfBq3TM8hf//EPcSQmbmqFUdspI0r31hfc0lIXHX5qNBaI\n-----END PRIVATE KEY-----\n";

    const DEFAULT_REPLICA_GROUP_NAME: &str = "default";
    static KANIDM_DEFAULT_SPEC_JSON: LazyLock<serde_json::Value> = LazyLock::new(|| {
        json!({
            "domain": "idm.example.com",
            "replicaGroups": [{"name": DEFAULT_REPLICA_GROUP_NAME, "replicas": 1}],
        })
    });

    const WAIT_FOR_REPLICATION_READY_SECONDS: u64 = 60;

    static STORAGE_VOLUME_CLAIM_TEMPLATE_JSON: LazyLock<serde_json::Value> = LazyLock::new(|| {
        json!(
            {
                "storage": {
                    "volumeClaimTemplate": {
                        "spec": {
                            "accessModes": [
                                "ReadWriteOnce"
                            ],
                            "resources": {
                                "requests": {
                                    "storage": "1Gi"
                                },
                            }
                        }
                    }
                }
            }
        )
    });

    struct SetupResult {
        client: Client,
        kanidm_api: Api<Kanidm>,
        statefulset_api: Api<StatefulSet>,
        secret_api: Api<Secret>,
    }

    fn check_kanidm_condition(cond: &str, status: String) -> impl Condition<Kanidm> + '_ {
        move |obj: Option<&Kanidm>| {
            obj.and_then(|kanidm| kanidm.status.as_ref())
                .and_then(|status| status.conditions.as_ref())
                .map_or(false, |conditions| {
                    conditions
                        .iter()
                        .any(|c| c.type_ == cond && c.status == status)
                })
        }
    }

    fn is_kanidm(cond: &str) -> impl Condition<Kanidm> + '_ {
        check_kanidm_condition(cond, "True".to_string())
    }

    fn is_kanidm_false(cond: &str) -> impl Condition<Kanidm> + '_ {
        check_kanidm_condition(cond, "False".to_string())
    }

    fn is_statefulset_ready(obj: Option<&StatefulSet>) -> bool {
        obj.and_then(|statefulset| statefulset.status.as_ref())
            .map_or(false, |s| {
                s.ready_replicas
                    .map_or(false, |ready_replicas| s.replicas == ready_replicas)
            })
    }

    async fn wait_for<K, C>(api: Api<K>, name: &str, condition: C)
    where
        K: kube::Resource
            + Clone
            + std::fmt::Debug
            + for<'de> k8s_openapi::serde::Deserialize<'de>
            + 'static
            + Send,
        C: Condition<K>,
    {
        timeout(
            Duration::from_secs(60),
            await_condition(api, name, condition),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "timeout waiting for {}/{name}",
                short_type_name::<K>().unwrap_or("Unknown resource")
            )
        })
        .unwrap();
    }

    async fn create_secret(client: &Client, name: &str) {
        let secret_api = Api::<Secret>::namespaced(client.clone(), "default");

        let mut data = BTreeMap::new();
        data.insert("tls.crt".to_string(), ByteString(CERT.to_vec()));
        data.insert("tls.key".to_string(), ByteString(KEY.to_vec()));

        let secret = Secret {
            metadata: ObjectMeta {
                name: Some(format!("{name}-tls")),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            data: Some(data),
            type_: Some("kubernetes.io/tls".to_string()),
            ..Default::default()
        };

        secret_api
            .create(&PostParams::default(), &secret)
            .await
            .unwrap();
    }

    async fn create_kanidm(
        client: &Client,
        name: &str,
        kanidm_spec_patch: Option<serde_json::Value>,
    ) -> (Kanidm, Api<Kanidm>) {
        let mut kanidm_spec_json = KANIDM_DEFAULT_SPEC_JSON.clone();
        if let Some(patch) = kanidm_spec_patch {
            merge(&mut kanidm_spec_json, &patch);
        };

        let kanidm = Kanidm::new(name, serde_json::from_value(kanidm_spec_json).unwrap());

        let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), "default");
        kanidm_api
            .create(&PostParams::default(), &kanidm)
            .await
            .unwrap();
        (kanidm, kanidm_api)
    }

    fn validate_admin_passwords(admin_passwords: Secret) {
        let admin_passwords_data = admin_passwords.data.clone().unwrap();
        assert_eq!(admin_passwords_data.len(), 2);

        let admin_password =
            String::from_utf8(admin_passwords_data.get("admin").unwrap().clone().0).unwrap();
        let idm_admin_password =
            String::from_utf8(admin_passwords_data.get("idm_admin").unwrap().clone().0).unwrap();

        assert_eq!(admin_password.len(), 48);
        assert_eq!(idm_admin_password.len(), 48);
        assert!(admin_password.chars().all(char::is_alphanumeric));
        assert!(idm_admin_password.chars().all(char::is_alphanumeric));
    }

    async fn setup(name: &str, kanidm_spec_patch: Option<serde_json::Value>) -> SetupResult {
        let client = Client::try_default().await.unwrap();
        let (kanidm, kanidm_api) = create_kanidm(&client, name, kanidm_spec_patch).await;
        create_secret(&client, name).await;

        let statefulset_api = Api::<StatefulSet>::namespaced(client.clone(), "default");
        let sts_names_vec = kanidm
            .spec
            .replica_groups
            .iter()
            .map(|rg| kanidm.statefulset_name(&rg.name))
            .collect::<Vec<_>>();
        let sts_futures = sts_names_vec
            .iter()
            .map(|sts_name| wait_for(statefulset_api.clone(), sts_name, is_statefulset_ready))
            .collect::<JoinAll<_>>();
        join!(sts_futures);

        wait_for(kanidm_api.clone(), name, is_kanidm("Available")).await;
        wait_for(kanidm_api.clone(), name, is_kanidm("Initialized")).await;

        let secret_api = Api::<Secret>::namespaced(client.clone(), "default");
        let admin_passwords = secret_api
            .get(&format!("{name}-admin-passwords"))
            .await
            .unwrap();
        validate_admin_passwords(admin_passwords);

        SetupResult {
            client,
            kanidm_api,
            statefulset_api,
            secret_api,
        }
    }

    #[tokio::test]
    async fn kanidm_create() {
        let name = "test-create";
        setup(name, None).await;
    }

    #[tokio::test]
    async fn kanidm_delete_statefulset() {
        let name = "test-delete-statefulset";
        let s = setup(name, None).await;

        let sts_name = format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}");
        let sts = s.statefulset_api.get(&sts_name).await.unwrap();
        s.statefulset_api
            .delete(&sts_name, &Default::default())
            .await
            .unwrap();

        wait_for(
            s.statefulset_api.clone(),
            &sts_name,
            conditions::is_deleted(&sts.uid().unwrap()),
        )
        .await;
        wait_for(s.statefulset_api.clone(), &sts_name, is_statefulset_ready).await;
        wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;

        let check_sts_deleted = s.statefulset_api.get(&sts_name).await.unwrap();

        s.kanidm_api
            .delete(name, &Default::default())
            .await
            .unwrap();

        wait_for(
            s.statefulset_api,
            &sts_name,
            conditions::is_deleted(&check_sts_deleted.uid().unwrap()),
        )
        .await;
    }

    #[tokio::test]
    async fn kanidm_delete_kanidm() {
        let name = "test-delete-kanidm";
        let s = setup(name, None).await;

        let sts_name = format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}");
        let sts = s.statefulset_api.get(&sts_name).await.unwrap();
        let kanidm = s.kanidm_api.get(name).await.unwrap();
        s.kanidm_api
            .delete(name, &Default::default())
            .await
            .unwrap();

        wait_for(
            s.kanidm_api.clone(),
            name,
            conditions::is_deleted(&kanidm.uid().unwrap()),
        )
        .await;

        wait_for(
            s.statefulset_api.clone(),
            &sts_name,
            conditions::is_deleted(&sts.uid().unwrap()),
        )
        .await;
    }

    #[tokio::test]
    async fn kanidm_change_statefulset() {
        let name = "test-change-statefulset";
        let s = setup(name, None).await;

        let sts_name = format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}");
        let mut sts = s.statefulset_api.get(&sts_name).await.unwrap();
        sts.spec.as_mut().unwrap().replicas = Some(2);
        sts.metadata.managed_fields = None;
        sts.metadata.resource_version = None;
        sts.metadata.uid = None;
        sts.metadata.creation_timestamp = None;
        s.statefulset_api
            .patch(
                &sts_name,
                &PatchParams::apply("e2e-test").force(),
                &Patch::Apply(&sts),
            )
            .await
            .unwrap();

        wait_for(
            s.statefulset_api.clone(),
            &sts_name,
            |obj: Option<&StatefulSet>| {
                obj.and_then(|statefulset| statefulset.status.as_ref())
                    .map_or(false, |status| status.replicas == 2)
            },
        )
        .await;

        wait_for(s.statefulset_api.clone(), &sts_name, is_statefulset_ready).await;

        let check_sts_replica_0 = s.statefulset_api.get(&sts_name).await.unwrap();

        assert_eq!(check_sts_replica_0.spec.unwrap().replicas.unwrap(), 1);
    }

    #[tokio::test]
    async fn kanidm_change_kanidm_replicas() {
        let name = "test-change-kanidm-replicas";
        let s = setup(name, Some(STORAGE_VOLUME_CLAIM_TEMPLATE_JSON.clone())).await;

        let mut kanidm = s.kanidm_api.get(name).await.unwrap();
        kanidm.spec.replica_groups[0].replicas = 2;
        kanidm.spec.replica_groups[0].primary_node = true;
        kanidm.metadata.managed_fields = None;
        s.kanidm_api
            .patch(
                name,
                &PatchParams::apply("e2e-test").force(),
                &Patch::Apply(&kanidm),
            )
            .await
            .unwrap();

        wait_for(s.kanidm_api.clone(), name, |obj: Option<&Kanidm>| {
            obj.and_then(|kanidm| kanidm.status.as_ref())
                .map_or(true, |status| status.updated_replicas == 2)
        })
        .await;
        wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;
        wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;

        let check_sts = s
            .statefulset_api
            .get(&format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}"))
            .await
            .unwrap();

        assert_eq!(check_sts.clone().spec.unwrap().replicas.unwrap(), 2);
        let sts_name = check_sts.name_any();
        // wait for restarts
        wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;

        // wait for restarts and readiness
        // TODO: replace with proper wait_for
        tokio::time::sleep(Duration::from_secs(WAIT_FOR_REPLICATION_READY_SECONDS)).await;

        let pod_api = Api::<Pod>::namespaced(s.client.clone(), "default");
        for i in 0..2 {
            let pod_name = format!("{sts_name}-{i}");
            let secret_name = kanidm.replica_secret_name(&pod_name);
            let secret = s.secret_api.get(&secret_name).await.unwrap();
            assert_eq!(secret.data.unwrap().len(), 1);
            let mut logs = pod_api
                .log_stream(&pod_name, &LogParams::default())
                .await
                .unwrap()
                .lines();
            let mut lines = Vec::new();
            while let Some(line) = logs.try_next().await.unwrap() {
                lines.push(line);
            }
            dbg!(&lines);
            assert!(lines
                .iter()
                .any(|line| line.contains("Incremental Replication Success")));
        }
    }

    #[tokio::test]
    async fn kanidm_statefulset_already_exists() {
        let name = "test-statefulset-already-exists";
        let statefulset = json!({
            "apiVersion": "apps/v1",
            "kind": "StatefulSet",
            "metadata": {
                "name": format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}")
            },
            "spec": {
                "replicas": 1,
                "selector": {
                    "matchLabels": {
                        "app": name
                    }
                },
                "template": {
                    "metadata": {
                        "labels": {
                            "app": name
                        }
                    },
                    "spec": {
                        "containers": [
                            {
                                "name": name,
                                "image": "kanidm/server:latest"
                            }
                        ]
                    }
                }
            }
        });
        let statefulset_api =
            Api::<StatefulSet>::namespaced(Client::try_default().await.unwrap(), "default");
        statefulset_api
            .create(
                &PostParams::default(),
                &serde_json::from_value(statefulset).unwrap(),
            )
            .await
            .unwrap();

        setup(name, None).await;
    }

    #[tokio::test]
    async fn kanidm_change_domain() {
        let name = "test-change-kanidm-domain";
        let s = setup(name, None).await;

        let mut kanidm = s.kanidm_api.get(name).await.unwrap();
        kanidm.spec.domain = "changed.example.com".to_string();
        kanidm.metadata.managed_fields = None;
        let result = s
            .kanidm_api
            .patch(
                name,
                &PatchParams::apply("e2e-test").force(),
                &Patch::Apply(&kanidm),
            )
            .await;

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Domain cannot be changed."));
    }

    #[tokio::test]
    async fn kanidm_donwscale_to_zero() {
        let name = "test-downscale-to-zero";
        let s = setup(name, Some(STORAGE_VOLUME_CLAIM_TEMPLATE_JSON.clone())).await;
        let mut kanidm = s.kanidm_api.get(name).await.unwrap();
        kanidm.spec.replica_groups[0].replicas = 0;
        kanidm.metadata.managed_fields = None;
        s.kanidm_api
            .patch(
                name,
                &PatchParams::apply("e2e-test").force(),
                &Patch::Apply(&kanidm),
            )
            .await
            .unwrap();

        let sts_name = kanidm.statefulset_name(&kanidm.spec.replica_groups.first().unwrap().name);
        let sts = s.statefulset_api.get(&sts_name).await.unwrap();
        let kanidm = s.kanidm_api.get(name).await.unwrap();
        s.kanidm_api
            .delete(name, &Default::default())
            .await
            .unwrap();

        wait_for(
            s.kanidm_api.clone(),
            name,
            conditions::is_deleted(&kanidm.uid().unwrap()),
        )
        .await;

        wait_for(
            s.statefulset_api.clone(),
            &sts_name,
            conditions::is_deleted(&sts.uid().unwrap()),
        )
        .await;

        let pvc_api = Api::<PersistentVolumeClaim>::namespaced(s.client.clone(), "default");
        pvc_api
            .get(&format!(
                "kanidm-data-{name}-{DEFAULT_REPLICA_GROUP_NAME}-0"
            ))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn kanidm_recreate_admin_passwords() {
        let name = "test-recreate-admin-passwords";
        let s = setup(name, None).await;

        let secret_name = format!("{name}-admin-passwords");
        let secret = s.secret_api.get(&secret_name).await.unwrap();
        s.secret_api
            .delete(&secret_name, &Default::default())
            .await
            .unwrap();
        wait_for(
            s.secret_api.clone(),
            &secret_name,
            conditions::is_deleted(&secret.uid().unwrap()),
        )
        .await;

        wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Initialized")).await;
        wait_for(s.kanidm_api.clone(), name, is_kanidm("Initialized")).await;
        let new_secret = s.secret_api.get(&secret_name).await.unwrap();
        validate_admin_passwords(new_secret);
    }

    #[tokio::test]
    async fn kanidm_invalid_name() {
        let client = Client::try_default().await.unwrap();

        let kanidm = Kanidm::new(
            "test-invalid.name",
            serde_json::from_value(KANIDM_DEFAULT_SPEC_JSON.clone()).unwrap(),
        );
        let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), "default");
        let result = kanidm_api.create(&PostParams::default(), &kanidm).await;

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Invalid name. Only lowercase alphanumeric characters and '-' are allowed."));
    }

    #[tokio::test]
    async fn kanidm_no_replica_groups() {
        let client = Client::try_default().await.unwrap();
        let mut kanidm_spec_json = KANIDM_DEFAULT_SPEC_JSON.clone();
        let patch = json!({
            "replicaGroups": [],
        });

        merge(&mut kanidm_spec_json, &patch);
        let kanidm = Kanidm::new(
            "test-no-replica-groups",
            serde_json::from_value(kanidm_spec_json).unwrap(),
        );
        let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), "default");
        let result = kanidm_api.create(&PostParams::default(), &kanidm).await;

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("spec.replicaGroups in body should have at least 1 items"));
    }

    #[tokio::test]
    async fn kanidm_no_replication_with_ephemeral_storage() {
        let client = Client::try_default().await.unwrap();
        let mut kanidm_spec_json = KANIDM_DEFAULT_SPEC_JSON.clone();
        let patch = json!({
            "replicaGroups": [
                {"name": "default", "replicas": 2},
            ],
        });

        merge(&mut kanidm_spec_json, &patch);

        let kanidm = Kanidm::new(
            "no-replication-with-ephemeral-storage",
            serde_json::from_value(kanidm_spec_json).unwrap(),
        );
        let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), "default");
        let result = kanidm_api.create(&PostParams::default(), &kanidm).await;

        dbg!(&result);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Replication not available for ephemeral storage."));
    }

    #[tokio::test]
    async fn kanidm_replica_groups_same_name() {
        let client = Client::try_default().await.unwrap();
        let mut kanidm_spec_json = KANIDM_DEFAULT_SPEC_JSON.clone();
        let patch_rgs = json!({
            "replicaGroups": [
                {"name": "same-name", "replicas": 1},
                {"name": "same-name", "replicas": 1},
            ],
        });
        let patch_storage = STORAGE_VOLUME_CLAIM_TEMPLATE_JSON.clone();

        merge(&mut kanidm_spec_json, &patch_rgs);
        merge(&mut kanidm_spec_json, &patch_storage);

        let kanidm = Kanidm::new(
            "test-replica-groups-same-name",
            serde_json::from_value(kanidm_spec_json).unwrap(),
        );
        let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), "default");
        let result = kanidm_api.create(&PostParams::default(), &kanidm).await;

        dbg!(&result);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Replica group names must be unique."));
    }

    #[tokio::test]
    async fn kanidm_replica_groups_read_replica_primary() {
        let client = Client::try_default().await.unwrap();
        let mut kanidm_spec_json = KANIDM_DEFAULT_SPEC_JSON.clone();
        let patch_rgs = json!({
            "replicaGroups": [
                {"name": "default", "replicas": 1},
                {"name": "read-replica", "replicas": 1, "role": "read_only_replica", "primaryNode": true},
            ],
        });
        let patch_storage = STORAGE_VOLUME_CLAIM_TEMPLATE_JSON.clone();

        merge(&mut kanidm_spec_json, &patch_rgs);
        merge(&mut kanidm_spec_json, &patch_storage);

        let kanidm = Kanidm::new(
            "test-replica-groups-read-replica-primary",
            serde_json::from_value(kanidm_spec_json).unwrap(),
        );
        let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), "default");
        let result = kanidm_api.create(&PostParams::default(), &kanidm).await;

        dbg!(&result);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains(
            "Primary node only can be true if role is 'write_replica' or 'write_replica_no_ui'."
        ));
    }

    #[tokio::test]
    async fn kanidm_replica_groups_two_primary() {
        let client = Client::try_default().await.unwrap();
        let mut kanidm_spec_json = KANIDM_DEFAULT_SPEC_JSON.clone();
        let patch_rgs = json!({
            "replicaGroups": [
                {"name": "default", "replicas": 1, "primaryNode": true},
                {"name": "write-replica", "replicas": 1, "primaryNode": true},
            ],
        });
        let patch_storage = STORAGE_VOLUME_CLAIM_TEMPLATE_JSON.clone();

        merge(&mut kanidm_spec_json, &patch_rgs);
        merge(&mut kanidm_spec_json, &patch_storage);

        let kanidm = Kanidm::new(
            "test-replica-groups-two-primary",
            serde_json::from_value(kanidm_spec_json).unwrap(),
        );
        let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), "default");
        let result = kanidm_api.create(&PostParams::default(), &kanidm).await;

        dbg!(&result);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains(
            "Only one primary node replica group or automatic refresh external node is allowed."
        ));
    }

    #[tokio::test]
    async fn kanidm_change_kanidm_replica_groups() {
        let name = "test-change-kanidm-replica-groups";
        let s = setup(name, Some(STORAGE_VOLUME_CLAIM_TEMPLATE_JSON.clone())).await;

        let mut kanidm = s.kanidm_api.get(name).await.unwrap();
        kanidm.spec.replica_groups.push(ReplicaGroup {
            name: "new".to_string(),
            replicas: 1,
            primary_node: true,
            ..ReplicaGroup::default()
        });
        kanidm.metadata.managed_fields = None;
        s.kanidm_api
            .patch(
                name,
                &PatchParams::apply("e2e-test").force(),
                &Patch::Apply(&kanidm),
            )
            .await
            .unwrap();

        wait_for(s.kanidm_api.clone(), name, is_kanidm("Progressing")).await;
        wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;
        wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;

        let sts_names = kanidm
            .spec
            .replica_groups
            .iter()
            .map(|rg| kanidm.statefulset_name(&rg.name))
            .collect::<Vec<_>>();
        for sts_name in sts_names.clone() {
            let check_sts = s.statefulset_api.get(&sts_name).await.unwrap();

            assert_eq!(check_sts.clone().spec.unwrap().replicas.unwrap(), 1);
            let sts_name = check_sts.name_any();
            let secret_name = kanidm.replica_secret_name(&format!("{sts_name}-0"));
            let secret = s.secret_api.get(&secret_name).await.unwrap();
            assert_eq!(secret.data.unwrap().len(), 1);
        }
        wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;

        // wait for restarts and readiness
        // TODO: replace with proper wait_for
        tokio::time::sleep(Duration::from_secs(WAIT_FOR_REPLICATION_READY_SECONDS)).await;

        let pod_api = Api::<Pod>::namespaced(s.client.clone(), "default");

        for sts_name in sts_names {
            let pod_name = format!("{sts_name}-0");
            let mut logs = pod_api
                .log_stream(&pod_name, &LogParams::default())
                .await
                .unwrap()
                .lines();
            let mut lines = Vec::new();
            while let Some(line) = logs.try_next().await.unwrap() {
                lines.push(line);
            }

            dbg!(&lines);
            assert!(lines
                .iter()
                .any(|line| line.contains("Incremental Replication Success")));
        }
    }

    #[tokio::test]
    async fn kanidm_replica_groups_one_read_only() {
        let name = "test-replica-groups-one-read-only";
        let mut patch_rgs = json!({
            "replicaGroups": [
                {"name": "default", "replicas": 1, "primaryNode": true},
                {"name": "read", "replicas": 1, "role": "read_only_replica"},
            ],
        });
        let patch_storage = STORAGE_VOLUME_CLAIM_TEMPLATE_JSON.clone();
        merge(&mut patch_rgs, &patch_storage);

        let s = setup(name, Some(patch_rgs)).await;
        let kanidm = s.kanidm_api.get(name).await.unwrap();
        let sts_names = kanidm
            .spec
            .replica_groups
            .iter()
            .map(|rg| kanidm.statefulset_name(&rg.name))
            .collect::<Vec<_>>();
        for sts_name in sts_names.clone() {
            let check_sts = s.statefulset_api.get(&sts_name).await.unwrap();

            assert_eq!(check_sts.clone().spec.unwrap().replicas.unwrap(), 1);
            let sts_name = check_sts.name_any();
            let secret_name = kanidm.replica_secret_name(&format!("{sts_name}-0"));
            let secret = s.secret_api.get(&secret_name).await.unwrap();
            assert_eq!(secret.data.unwrap().len(), 1);
        }

        // wait for restarts
        wait_for(s.kanidm_api.clone(), name, is_kanidm("Progressing")).await;
        wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;

        // wait for restarts and readiness
        // TODO: replace with proper wait_for
        tokio::time::sleep(Duration::from_secs(WAIT_FOR_REPLICATION_READY_SECONDS)).await;

        let pod_api = Api::<Pod>::namespaced(s.client.clone(), "default");
        let mut lines = Vec::new();

        // just second pod show the `Incremental Replication Success` message
        let sts_name_read_only = sts_names.last().unwrap();
        let pod_name = format!("{sts_name_read_only}-0");
        let mut logs = pod_api
            .log_stream(&pod_name, &LogParams::default())
            .await
            .unwrap()
            .lines();
        while let Some(line) = logs.try_next().await.unwrap() {
            lines.push(line);
        }
        dbg!(&lines);
        assert!(lines
            .iter()
            .any(|line| line.contains("Incremental Replication Success")));
    }

    #[tokio::test]
    async fn kanidm_delete_replica_group() {
        let name = "test-delete-replica-group";
        let mut kanidm_path = json!({
        "replicaGroups": [
            {"name": "default", "replicas": 1},
            {"name": "to-delete", "replicas": 1},
            ],
        });
        let patch_storage = STORAGE_VOLUME_CLAIM_TEMPLATE_JSON.clone();

        merge(&mut kanidm_path, &patch_storage);
        let s = setup(name, Some(kanidm_path.clone())).await;
        let mut kanidm = s.kanidm_api.get(name).await.unwrap();
        kanidm.spec.replica_groups.pop();
        kanidm.metadata.managed_fields = None;
        s.kanidm_api
            .patch(
                name,
                &PatchParams::apply("e2e-test").force(),
                &Patch::Apply(&kanidm),
            )
            .await
            .unwrap();

        wait_for(s.kanidm_api.clone(), name, is_kanidm("Progressing")).await;
        wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;
        wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;

        let sts_name = kanidm.statefulset_name("to-delete");
        let check_sts = s.statefulset_api.get(&sts_name).await;

        assert!(check_sts.is_err());
        let pod_name = format!("{sts_name}-0");
        let secret = kanidm.replica_secret_name(&pod_name);
        let check_secret = s.secret_api.get(&secret).await;
        assert!(check_secret.is_err());
    }

    #[tokio::test]
    async fn kanidm_replica_groups_one_primary_and_external_node_automatic_refresh() {
        let client = Client::try_default().await.unwrap();
        let mut kanidm_spec_json = KANIDM_DEFAULT_SPEC_JSON.clone();
        let patch_rgs = json!({
                "replicaGroups": [
                    {"name": "default", "replicas": 1, "primaryNode": true}
                ],
                "externalReplicationNodes": [
                    {
                        "name": "external-node",
                        "hostname": "host-0",
                        "port": 8444,
                        "certificate": {
                            "name": "external-node-cert",
                            "key": "tls.der.b64url",
                        },
                        "automaticRefresh": true
        }
                ]
            });
        let patch_storage = STORAGE_VOLUME_CLAIM_TEMPLATE_JSON.clone();

        merge(&mut kanidm_spec_json, &patch_rgs);
        merge(&mut kanidm_spec_json, &patch_storage);

        let kanidm = Kanidm::new(
            "test-replica-groups-one-primary-one-external-node-automatic-refresh",
            serde_json::from_value(kanidm_spec_json).unwrap(),
        );
        let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), "default");
        let result = kanidm_api.create(&PostParams::default(), &kanidm).await;

        dbg!(&result);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains(
            "Only one primary node replica group or automatic refresh external node is allowed."
        ));
    }

    #[tokio::test]
    async fn kanidm_no_replication_with_ephemeral_storage_external_replication_node() {
        let client = Client::try_default().await.unwrap();
        let mut kanidm_spec_json = KANIDM_DEFAULT_SPEC_JSON.clone();
        let patch = json!({
            "externalReplicationNodes": [
                {
                    "name": "external-node",
                    "hostname": "host-0",
                    "port": 8444,
                    "certificate": {
                        "name": "external-node-cert",
                        "key": "tls.der.b64url",
                    }
                }
            ],
        });

        merge(&mut kanidm_spec_json, &patch);

        let kanidm = Kanidm::new(
            "no-replication-with-ephemeral-storage-ern",
            serde_json::from_value(kanidm_spec_json).unwrap(),
        );
        let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), "default");
        let result = kanidm_api.create(&PostParams::default(), &kanidm).await;

        dbg!(&result);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Replication not available for ephemeral storage."));
    }

    #[tokio::test]
    async fn kanidm_external_replication_node() {
        let kanidms_params = [
            (
                "test-external-replication-node-0",
                "test-external-replication-node-1-default-0",
                "test-external-replication-node-1-default-0-cert",
                false,
            ),
            (
                "test-external-replication-node-1",
                "test-external-replication-node-0-default-0",
                "test-external-replication-node-0-default-0-cert",
                true,
            ),
        ];

        let mut s = None;

        for (name, hostname, cert_name, automatic_refresh) in &kanidms_params {
            let mut kanidm_path = json!({
                "externalReplicationNodes": [
                    {
                        "name": "external-node",
                        "hostname": hostname,
                        "port": 8444,
                        "certificate": {
                            "name": cert_name,
                            "key": "tls.der.b64url",
                            "optional": true,
                        },
                        "automaticRefresh": *automatic_refresh,
                    }
                ],
            });

            merge(
                &mut kanidm_path,
                &STORAGE_VOLUME_CLAIM_TEMPLATE_JSON.clone(),
            );
            let setup_result = setup(name, Some(kanidm_path.clone())).await;
            if s.is_none() {
                s = Some(setup_result);
            }
        }

        let s = s.unwrap();

        dbg!("setup done");
        for (name, _, _, _) in &kanidms_params {
            wait_for(s.kanidm_api.clone(), name, is_kanidm("Available")).await;
            wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;
        }
        dbg!("kanidms available");

        for (name, _, _, _) in &kanidms_params {
            let sts_name = format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}");
            s.statefulset_api.restart(&sts_name).await.unwrap();
            dbg!(format!("restarting sts/{sts_name}"));
            wait_for(s.kanidm_api.clone(), name, is_kanidm("Progressing")).await;
            wait_for(s.kanidm_api.clone(), name, is_kanidm_false("Progressing")).await;
        }

        // wait for restarts and readiness
        // TODO: replace with proper wait_for
        tokio::time::sleep(Duration::from_secs(30)).await;

        let pod_api = Api::<Pod>::namespaced(s.client.clone(), "default");
        for (name, _, _, _) in &kanidms_params {
            let pod_name = format!("{name}-{DEFAULT_REPLICA_GROUP_NAME}-0");
            let mut logs = pod_api
                .log_stream(&pod_name, &LogParams::default())
                .await
                .unwrap()
                .lines();
            let mut lines = Vec::new();
            while let Some(line) = logs.try_next().await.unwrap() {
                lines.push(line);
            }

            dbg!(&lines);
            assert!(lines
                .iter()
                .any(|line| line.contains("Incremental Replication Success")));
        }
    }
}

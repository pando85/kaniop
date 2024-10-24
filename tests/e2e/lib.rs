#[cfg(all(test, feature = "e2e-test"))]
mod test {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use k8s_openapi::api::core::v1::{PersistentVolumeClaim, Secret};
    use k8s_openapi::ByteString;
    use kaniop_kanidm::crd::Kanidm;

    use futures::future::JoinAll;
    use futures::join;
    use json_patch::merge;
    use k8s_openapi::api::apps::v1::StatefulSet;
    use kube::api::{Api, ObjectMeta, Patch, PatchParams, PostParams};
    use kube::client::Client;
    use kube::runtime::wait::{await_condition, conditions, Condition};
    use kube::ResourceExt;
    use serde_json::json;
    use tokio::time::timeout;

    fn is_kanidm_available() -> impl Condition<Kanidm> {
        |obj: Option<&Kanidm>| {
            obj.and_then(|kanidm| kanidm.status.as_ref())
                .and_then(|status| status.conditions.as_ref())
                .map_or(false, |conditions| {
                    conditions.iter().any(|c| c.type_ == "Available")
                })
        }
    }

    fn is_statefulset_ready() -> impl Condition<StatefulSet> {
        |obj: Option<&StatefulSet>| {
            obj.and_then(|statefulset| statefulset.status.as_ref())
                .map_or(false, |s| {
                    s.ready_replicas
                        .map_or(false, |ready_replicas| s.replicas == ready_replicas)
                })
        }
    }

    async fn wait_for<R, C>(api: Api<R>, name: String, condition: C)
    where
        R: kube::Resource
            + Clone
            + std::fmt::Debug
            + for<'de> k8s_openapi::serde::Deserialize<'de>
            + 'static
            + Send,
        C: Condition<R>,
    {
        timeout(
            Duration::from_secs(60),
            await_condition(api, &name, condition),
        )
        .await
        .unwrap()
        .unwrap();
    }

    async fn setup(
        name: &str,
        kanidm_spec_patch: Option<serde_json::Value>,
    ) -> (Client, Api<Kanidm>, Api<StatefulSet>) {
        let mut kanidm_spec_json = json!({
            "domain": "idm.example.com",
        });
        if let Some(patch) = kanidm_spec_patch {
            merge(&mut kanidm_spec_json, &patch);
        };

        let kanidm = Kanidm::new(
            name,
            // for getting serde default values
            serde_json::from_value(kanidm_spec_json).unwrap(),
        );

        let client = Client::try_default().await.unwrap();
        let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), "default");

        kanidm_api
            .create(&PostParams::default(), &kanidm)
            .await
            .unwrap();

        let secret_api = Api::<Secret>::namespaced(client.clone(), "default");

        let cert = b"-----BEGIN CERTIFICATE-----\nMIIChDCCAiugAwIBAgIBAjAKBggqhkjOPQQDAjCBhDELMAkGA1UEBhMCQVUxDDAK\nBgNVBAgMA1FMRDEPMA0GA1UECgwGS2FuaWRtMRwwGgYDVQQDDBNLYW5pZG0gR2Vu\nZXJhdGVkIENBMTgwNgYDVQQLDC9EZXZlbG9wbWVudCBhbmQgRXZhbHVhdGlvbiAt\nIE5PVCBGT1IgUFJPRFVDVElPTjAeFw0yNDEwMTMyMDQzMjhaFw0yNDEwMTgyMDQz\nMjhaMIGAMQswCQYDVQQGEwJBVTEMMAoGA1UECAwDUUxEMQ8wDQYDVQQKDAZLYW5p\nZG0xGDAWBgNVBAMMD2lkbS5leGFtcGxlLmNvbTE4MDYGA1UECwwvRGV2ZWxvcG1l\nbnQgYW5kIEV2YWx1YXRpb24gLSBOT1QgRk9SIFBST0RVQ1RJT04wWTATBgcqhkjO\nPQIBBggqhkjOPQMBBwNCAARTi7hqo0Z3BU3p95z6hQzPmYAox3bKfAAu4GmY8Qhf\nBq3TM8hf//EPcSQmbmqFUdspI0r31hfc0lIXHX5qNBaIo4GPMIGMMAkGA1UdEwQC\nMAAwDgYDVR0PAQH/BAQDAgWgMBMGA1UdJQQMMAoGCCsGAQUFBwMBMB0GA1UdDgQW\nBBQaarHTRm4Yj6TMPzvduAB7nODKHzAfBgNVHSMEGDAWgBTaOaPuXmtLDTJVv++V\nYBiQr9gHCTAaBgNVHREEEzARgg9pZG0uZXhhbXBsZS5jb20wCgYIKoZIzj0EAwID\nRwAwRAIgQpLs9MZvBRUpR15wvSwIq/QyWotvVg/3vZl8D1mTFz8CIEVbm+/+z4JL\nLYwNXnerv9Nc+anGtz+9beT4bkS4CpJS\n-----END CERTIFICATE-----\n-----BEGIN CERTIFICATE-----\nMIICPjCCAeSgAwIBAgIBATAKBggqhkjOPQQDAjCBhDELMAkGA1UEBhMCQVUxDDAK\nBgNVBAgMA1FMRDEPMA0GA1UECgwGS2FuaWRtMRwwGgYDVQQDDBNLYW5pZG0gR2Vu\nZXJhdGVkIENBMTgwNgYDVQQLDC9EZXZlbG9wbWVudCBhbmQgRXZhbHVhdGlvbiAt\nIE5PVCBGT1IgUFJPRFVDVElPTjAeFw0yNDEwMTMyMDQzMjhaFw0yNDExMTIyMDQz\nMjhaMIGEMQswCQYDVQQGEwJBVTEMMAoGA1UECAwDUUxEMQ8wDQYDVQQKDAZLYW5p\nZG0xHDAaBgNVBAMME0thbmlkbSBHZW5lcmF0ZWQgQ0ExODA2BgNVBAsML0RldmVs\nb3BtZW50IGFuZCBFdmFsdWF0aW9uIC0gTk9UIEZPUiBQUk9EVUNUSU9OMFkwEwYH\nKoZIzj0CAQYIKoZIzj0DAQcDQgAEiz5mqHozpsj5iGCDH8uSJy8TFqNIGnIw8U/L\nswyeFTGHT4S2HwBb7QAouYVuXdwL8hZGMtzAqoYMFhCt1epXjqNFMEMwEgYDVR0T\nAQH/BAgwBgEB/wIBADAOBgNVHQ8BAf8EBAMCAQYwHQYDVR0OBBYEFNo5o+5ea0sN\nMlW/75VgGJCv2AcJMAoGCCqGSM49BAMCA0gAMEUCIGyZjBs4pp1HAlFdk0mdVBz4\n440t8pRHh8/SOY5ZtMcSAiEA6qOf9aQbWwEXLj0jajX9lHgdqlwRk7wnnyLMGF5/\nlz8=\n-----END CERTIFICATE-----\n";
        let key = b"-----BEGIN PRIVATE KEY-----\nMIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgHLZoMTUMadxOKMlt\nTq/kDnuN38GCJkwj8Y2kqyGlcf+hRANCAARTi7hqo0Z3BU3p95z6hQzPmYAox3bK\nfAAu4GmY8QhfBq3TM8hf//EPcSQmbmqFUdspI0r31hfc0lIXHX5qNBaI\n-----END PRIVATE KEY-----\n";

        // Prepare the data for the secret
        let mut data = BTreeMap::new();
        data.insert("tls.crt".to_string(), ByteString(cert.to_vec()));
        data.insert("tls.key".to_string(), ByteString(key.to_vec()));

        // Create the secret object
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
        let statefulset_api = Api::<StatefulSet>::namespaced(client.clone(), "default");
        let sts_futures = kanidm
            .iter_statefulset_names()
            .map(|sts_name| wait_for(statefulset_api.clone(), sts_name, is_statefulset_ready()))
            .collect::<JoinAll<_>>();
        join!(sts_futures);
        wait_for(kanidm_api.clone(), name.to_string(), is_kanidm_available()).await;
        (client, kanidm_api, statefulset_api)
    }

    #[tokio::test]
    async fn kanidm_create() {
        let name = "test-create";
        setup(name, None).await;
    }

    #[tokio::test]
    async fn kanidm_delete_statefulset() {
        let name = "test-delete-statefulset";
        let (_, kanidm_api, statefulset_api) = setup(name, None).await;

        let sts_name = format!("{name}-0");
        let sts = statefulset_api.get(&sts_name).await.unwrap();
        statefulset_api
            .delete(&sts_name, &Default::default())
            .await
            .unwrap();

        wait_for(
            statefulset_api.clone(),
            sts_name.clone(),
            conditions::is_deleted(&sts.uid().unwrap()),
        )
        .await;
        wait_for(
            statefulset_api.clone(),
            sts_name.clone(),
            is_statefulset_ready(),
        )
        .await;
        wait_for(kanidm_api.clone(), name.to_string(), is_kanidm_available()).await;

        let check_sts_deleted = statefulset_api.get(&sts_name).await.unwrap();

        kanidm_api.delete(name, &Default::default()).await.unwrap();

        wait_for(
            statefulset_api,
            sts_name,
            conditions::is_deleted(&check_sts_deleted.uid().unwrap()),
        )
        .await;
    }

    #[tokio::test]
    async fn kanidm_delete_kanidm() {
        let name = "test-delete-kanidm";
        let (_, kanidm_api, statefulset_api) = setup(name, None).await;

        let sts_name = format!("{name}-0");
        let sts = statefulset_api.get(&sts_name).await.unwrap();
        let kanidm = kanidm_api.get(name).await.unwrap();
        kanidm_api.delete(name, &Default::default()).await.unwrap();

        wait_for(
            kanidm_api.clone(),
            name.to_string(),
            conditions::is_deleted(&kanidm.uid().unwrap()),
        )
        .await;

        wait_for(
            statefulset_api.clone(),
            sts_name,
            conditions::is_deleted(&sts.uid().unwrap()),
        )
        .await;
    }

    #[tokio::test]
    async fn kanidm_change_statefulset() {
        let name = "test-change-statefulset";
        let (_, _, statefulset_api) = setup(name, None).await;

        let sts_name = format!("{name}-0");
        let mut sts = statefulset_api.get(&sts_name).await.unwrap();
        sts.spec.as_mut().unwrap().replicas = Some(2);
        sts.metadata.managed_fields = None;
        statefulset_api
            .patch(
                &sts_name,
                &PatchParams::apply("e2e-test").force(),
                &Patch::Apply(&sts),
            )
            .await
            .unwrap();

        wait_for(
            statefulset_api.clone(),
            sts_name.clone(),
            |obj: Option<&StatefulSet>| {
                obj.and_then(|statefulset| statefulset.status.as_ref())
                    .map_or(false, |status| status.replicas == 2)
            },
        )
        .await;

        wait_for(
            statefulset_api.clone(),
            sts_name.clone(),
            is_statefulset_ready(),
        )
        .await;

        let check_sts_replica_0 = statefulset_api.get(&sts_name).await.unwrap();

        assert_eq!(check_sts_replica_0.spec.unwrap().replicas.unwrap(), 1);
    }

    #[tokio::test]
    async fn kanidm_change_kanidm_replicas() {
        let name = "test-change-kanidm-replicas";
        let (_, kanidm_api, statefulset_api) = setup(name, None).await;

        let mut kanidm = kanidm_api.get(name).await.unwrap();
        kanidm.spec.replicas = 2;
        kanidm.metadata.managed_fields = None;
        kanidm_api
            .patch(
                name,
                &PatchParams::apply("e2e-test").force(),
                &Patch::Apply(&kanidm),
            )
            .await
            .unwrap();

        wait_for(
            kanidm_api.clone(),
            name.to_string(),
            |obj: Option<&Kanidm>| {
                obj.and_then(|kanidm| kanidm.status.as_ref())
                    .map_or(true, |status| status.updated_replicas == 2)
            },
        )
        .await;
        wait_for(kanidm_api.clone(), name.to_string(), is_kanidm_available()).await;

        let check_sts_replica_0 = statefulset_api.get(&format!("{name}-0")).await.unwrap();
        let check_sts_replica_1 = statefulset_api.get(&format!("{name}-1")).await.unwrap();

        assert_eq!(check_sts_replica_0.spec.unwrap().replicas.unwrap(), 1);
        assert_eq!(check_sts_replica_1.spec.unwrap().replicas.unwrap(), 1);
    }

    #[tokio::test]
    async fn kanidm_statefulset_already_exists() {
        let name = "test-statefulset-already-exists";
        let statefulset = json!({
            "apiVersion": "apps/v1",
            "kind": "StatefulSet",
            "metadata": {
                "name": format!("{name}-0")
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
        let (_, kanidm_api, _) = setup(name, None).await;

        let mut kanidm = kanidm_api.get(name).await.unwrap();
        kanidm.spec.domain = "changed.example.com".to_string();
        kanidm.metadata.managed_fields = None;
        kanidm_api
            .patch(
                name,
                &PatchParams::apply("e2e-test").force(),
                &Patch::Apply(&kanidm),
            )
            .await
            .unwrap();
        // TODO: Update to schemars 1.0.0 to fix this
        //.expect_err("should not be able to change domain");
    }

    #[tokio::test]
    async fn kanidm_donwscale_to_zero() {
        let name = "test-downscale-to-zero";
        let (client, kanidm_api, statefulset_api) = setup(
            name,
            Some(json!(
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
            )),
        )
        .await;
        let mut kanidm = kanidm_api.get(name).await.unwrap();
        kanidm.spec.replicas = 0;
        kanidm.metadata.managed_fields = None;
        kanidm_api
            .patch(
                name,
                &PatchParams::apply("e2e-test").force(),
                &Patch::Apply(&kanidm),
            )
            .await
            .unwrap();

        let sts_name = format!("{name}-0");
        let sts = statefulset_api.get(&sts_name).await.unwrap();
        let kanidm = kanidm_api.get(name).await.unwrap();
        kanidm_api.delete(name, &Default::default()).await.unwrap();

        wait_for(
            kanidm_api.clone(),
            name.to_string(),
            conditions::is_deleted(&kanidm.uid().unwrap()),
        )
        .await;

        wait_for(
            statefulset_api.clone(),
            sts_name,
            conditions::is_deleted(&sts.uid().unwrap()),
        )
        .await;

        let pvc_api = Api::<PersistentVolumeClaim>::namespaced(client.clone(), "default");
        pvc_api
            .get(&format!("kanidm-data-{name}-0-0"))
            .await
            .unwrap();
    }
}

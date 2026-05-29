use std::time::Duration;

use k8s_openapi::api::core::v1::Secret;
use kanidm_client::KanidmClient;
use kaniop_operator::kanidm::crd::Kanidm;
use kaniop_person::crd::KanidmPersonAccount;
use kube::api::{DeleteParams, PostParams};
use kube::runtime::wait::Condition;
use kube::{Api, Client, ResourceExt};
use serde_json::json;
use tokio::time::sleep;

use crate::kanidm::get_dependency_version;

use super::{init_crypto_provider, kanidm::is_kanidm, stabilization_delay, wait_for};

use backon::{ExponentialBuilder, Retryable};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::LazyLock;
use tokio::sync::Semaphore;

const KANIDM_NAME: &str = "test-person-1103";
const NAMESPACE: &str = "default";

static SETUP_LOCK: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::const_new(1)));

struct Setup {
    kanidm_client: KanidmClient,
    client: Client,
}

fn check_person_condition(cond: &str, status: String) -> impl Condition<KanidmPersonAccount> + '_ {
    move |obj: Option<&KanidmPersonAccount>| {
        obj.and_then(|person| person.status.as_ref())
            .and_then(|status| status.conditions.as_ref())
            .is_some_and(|conditions| {
                conditions
                    .iter()
                    .any(|c| c.type_ == cond && c.status == status)
            })
    }
}

fn is_person(cond: &str) -> impl Condition<KanidmPersonAccount> + '_ {
    check_person_condition(cond, "True".to_string())
}

fn is_person_ready() -> impl Condition<KanidmPersonAccount> {
    move |obj: Option<&KanidmPersonAccount>| {
        obj.and_then(|group| group.status.as_ref())
            .is_some_and(|status| status.ready)
    }
}

async fn setup_kanidm_1103() -> Setup {
    init_crypto_provider();
    let client = Client::try_default().await.unwrap();
    let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), NAMESPACE);
    let domain = format!("{KANIDM_NAME}.localhost");

    let idm_admin_password = {
        let _lock = SETUP_LOCK.acquire().await;

        if kanidm_api.get(KANIDM_NAME).await.is_ok() {
            drop(_lock);
            let secret_api = Api::<Secret>::namespaced(client.clone(), NAMESPACE);
            wait_for(kanidm_api.clone(), KANIDM_NAME, is_kanidm("Available")).await;
            wait_for(kanidm_api.clone(), KANIDM_NAME, is_kanidm("Initialized")).await;
            let admin_secret = secret_api
                .get(&format!("{KANIDM_NAME}-admin-passwords"))
                .await
                .unwrap();
            let secret_data = admin_secret.data.unwrap();
            let password_bytes = secret_data.get("IDM_ADMIN_PASSWORD").unwrap();
            std::str::from_utf8(&password_bytes.0).unwrap().to_string()
        } else {
            let kanidm_spec = json!({
                "domain": domain,
                "image": format!("kanidm/server:{}", get_dependency_version().unwrap()),
                "replicaGroups": [{"name": "default", "replicas": 1}],
                "ingress": {
                    "annotations": {
                        "nginx.ingress.kubernetes.io/backend-protocol": "HTTPS",
                    }
                }
            });
            let kanidm = Kanidm::new(KANIDM_NAME, serde_json::from_value(kanidm_spec).unwrap());
            kanidm_api
                .create(&PostParams::default(), &kanidm)
                .await
                .unwrap();

            let secret_api = Api::<Secret>::namespaced(client.clone(), NAMESPACE);

            let mut data = BTreeMap::new();
            data.insert(
                "tls.crt".to_string(),
                k8s_openapi::ByteString(b"-----BEGIN CERTIFICATE-----\nMIIChDCCAiugAwIBAgIBAjAKBggqhkjOPQQDAjCBhDELMAkGA1UEBhMCQVUxDDAK\nBgNVBAgMA1FMRDEPMA0GA1UECgwGS2FuaWRtMRwwGgYDVQQDDBNLYW5pZG0gR2Vu\nZXJhdGVkIENBMTgwNgYDVQQLDC9EZXZlbG9wbWVudCBhbmQgRXZhbHVhdGlvbiAt\nIE5PVCBGT1IgUFJPRFVDVElPTjAeFw0yNDEwMTMyMDQzMjhaFw0yNDEwMTgyMDQz\nMjhaMIGAMQswCQYDVQQGEwJBVTEMMAoGA1UECAwDUUxEMQ8wDQYDVQQKDAZLYW5p\nZG0xGDAWBgNVBAMMD2lkbS5leGFtcGxlLmNvbTE4MDYGA1UECwwvRGV2ZWxvcG1l\nbnQgYW5kIEV2YWx1YXRpb24gLSBOT1QgRk9SIFBST0RVQ1RJT04wWTATBgcqhkjO\nPQIBBggqhkjOPQMBBwNCAARTi7hqo0Z3BU3p95z6hQzPmYAox3bKfAAu4GmY8Qhf\nBq3TM8hf//EPcSQmbmqFUdspI0r31hfc0lIXHX5qNBaIo4GPMIGMMAkGA1UdEwQC\nMAAwDgYDVR0PAQH/BAQDAgWgMBMGA1UdJQQMMAoGCCsGAQUFBwMBMB0GA1UdDgQW\nBBQaarHTRm4Yj6TMPzvduAB7nODKHzAfBgNVHSMEGDAWgBTaOaPuXmtLDTJVv++V\nYBiQr9gHCTAaBgNVHREEEzARgg9pZG0uZXhhbXBsZS5jb20wCgYIKoZIzj0EAwID\nRwAwRAIgQpLs9MZvBRUpR15wvSwIq/QyWotvVg/3vZl8D1mTFz8CIEVbm+/+z4JL\nLYwNXnerv9Nc+anGtz+9beT4bkS4CpJS\n-----END CERTIFICATE-----\n-----BEGIN CERTIFICATE-----\nMIICPjCCAeSgAwIBAgIBATAKBggqhkjOPQQDAjCBhDELMAkGA1UEBhMCQVUxDDAK\nBgNVBAgMA1FMRDEPMA0GA1UECgwGS2FuaWRtMRwwGgYDVQQDDBNLYW5pZG0gR2Vu\nZXJhdGVkIENBMTgwNgYDVQQLDC9EZXZlbG9wbWVudCBhbmQgRXZhbHVhdGlvbiAt\nIE5PVCBGT1IgUFJPRFVDVElPTjAeFw0yNDEwMTMyMDQzMjhaFw0yNDExMTIyMDQz\nMjhaMIGEMQswCQYDVQQGEwJBVTEMMAoGA1UECAwDUUxEMQ8wDQYDVQQKDAZLYW5p\nZG0xHDAaBgNVBAMME0thbmlkbSBHZW5lcmF0ZWQgQ0ExODA2BgNVBAsML0RldmVs\nb3BtZW50IGFuZCBFdmFsdWF0aW9uIC0gTk9UIEZPUiBQUk9EVUNUSU9OMFkwEwYH\nKoZIzj0CAQYIKoZIzj0DAQcDQgAEiz5mqHozpsj5iGCDH8uSJy8TFqNIGnIw8U/L\nswyeFTGHT4S2HwBb7QAouYVuXdwL8hZGMtzAqoYMFhCt1epXjqNFMEMwEgYDVR0T\nAQH/BAgwBgEB/wIBADAOBgNVHQ8BAf8EBAMCAQYwHQYVR0OBBYEFNo5o+5ea0sN\nMlW/75VgGJCv2AcJMAoGCCqGSM49BAMCA0gAMEUCIGyZjBs4pp1HAlFdk0mdVBz4\n440t8pRHh8/SOY5ZtMcSAiEA6qOf9aQbWwEXLj0jajX9lHgdqlwRk7wnnyLMGF5/\nlz8=\n-----END CERTIFICATE-----\n".to_vec()),
            );
            data.insert(
                "tls.key".to_string(),
                k8s_openapi::ByteString(b"-----BEGIN PRIVATE KEY-----\nMIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgHLZoMTUMadxOKMlt\nTq/kDnuN38GCJkwj8Y2kqyGlcf+hRANCAARTi7hqo0Z3BU3p95z6hQzPmYAox3bK\nfAAu4GmY8QhfBq3TM8hf//EPcSQmbmqFUdspI0r31hfc0lIXHX5qNBaI\n-----END PRIVATE KEY-----\n".to_vec()),
            );

            let secret = Secret {
                metadata: kube::api::ObjectMeta {
                    name: Some(format!("{KANIDM_NAME}-tls")),
                    namespace: Some(NAMESPACE.to_string()),
                    ..Default::default()
                },
                data: Some(data),
                type_: Some("kubernetes.io/tls".to_string()),
                ..Default::default()
            };

            if secret_api.get(&format!("{KANIDM_NAME}-tls")).await.is_err() {
                secret_api
                    .create(&PostParams::default(), &secret)
                    .await
                    .unwrap();
            }

            drop(_lock);

            wait_for(kanidm_api.clone(), KANIDM_NAME, is_kanidm("Available")).await;
            wait_for(kanidm_api.clone(), KANIDM_NAME, is_kanidm("Initialized")).await;

            let admin_secret = secret_api
                .get(&format!("{KANIDM_NAME}-admin-passwords"))
                .await
                .unwrap();
            let secret_data = admin_secret.data.unwrap();
            let password_bytes = secret_data.get("IDM_ADMIN_PASSWORD").unwrap();
            std::str::from_utf8(&password_bytes.0).unwrap().to_string()
        }
    };

    let kanidm_client = kanidm_client::KanidmClientBuilder::new()
        .danger_accept_invalid_certs(true)
        .address(format!("https://{domain}"))
        .connect_timeout(5)
        .build()
        .unwrap();

    let retryable_future = || async {
        kanidm_client
            .auth_simple_password("idm_admin", &idm_admin_password)
            .await
    };

    retryable_future
        .retry(ExponentialBuilder::default().with_max_times(8))
        .sleep(tokio::time::sleep)
        .await
        .unwrap();

    Setup {
        kanidm_client,
        client,
    }
}

async fn create_and_wait(
    s: &Setup,
    name: &str,
    person_spec: serde_json::Value,
) -> (Api<KanidmPersonAccount>, KanidmPersonAccount) {
    let person = KanidmPersonAccount::new(name, serde_json::from_value(person_spec).unwrap());
    let person_api = Api::<KanidmPersonAccount>::namespaced(s.client.clone(), NAMESPACE);
    person_api
        .create(&PostParams::default(), &person)
        .await
        .unwrap();

    wait_for(person_api.clone(), name, is_person("Exists")).await;
    wait_for(person_api.clone(), name, is_person("Updated")).await;
    wait_for(person_api.clone(), name, is_person_ready()).await;

    (person_api.clone(), person_api.get(name).await.unwrap())
}

async fn check_resource_version_stable(
    person_api: &Api<KanidmPersonAccount>,
    name: &str,
) -> KanidmPersonAccount {
    sleep(stabilization_delay()).await;
    let person_after = person_api.get(name).await.unwrap();
    let initial_rv = person_after.resource_version();

    sleep(stabilization_delay()).await;
    let person_final = person_api.get(name).await.unwrap();

    assert_eq!(
        person_final.resource_version(),
        initial_rv,
        "Resource version must not change (no reconcile loop)"
    );
    person_final
}

async fn assert_updated_condition_true(person_api: &Api<KanidmPersonAccount>, name: &str) {
    let person = person_api.get(name).await.unwrap();
    let updated_cond = person
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_ref())
        .and_then(|conds| conds.iter().find(|c| c.type_ == "Updated"));
    assert_eq!(
        updated_cond.unwrap().status,
        "True",
        "Updated condition must be True"
    );
}

async fn dump_kanidm_attrs(s: &Setup, name: &str) {
    let person = s
        .kanidm_client
        .idm_person_account_get(name)
        .await
        .unwrap()
        .unwrap();
    eprintln!("=== Kanidm raw attributes for {name} ===");
    for (key, values) in &person.attrs {
        eprintln!("  {key}: {values:?}");
    }
}

async fn cleanup(person_api: &Api<KanidmPersonAccount>, name: &str) {
    person_api
        .delete(name, &DeleteParams::default())
        .await
        .unwrap();
}

// ============================================================================
// Phase 2: Baseline RV stability tests with Kanidm 1.10.3
// ============================================================================

e2e_test!(kanidm_1103_minimal_displayname, {
    let s = setup_kanidm_1103().await;
    let name = "test-1103-minimal";

    let (person_api, _) = create_and_wait(
        &s,
        name,
        json!({
            "kanidmRef": {"name": KANIDM_NAME},
            "personAttributes": {"displayname": "Test Minimal"}
        }),
    )
    .await;

    dump_kanidm_attrs(&s, name).await;
    assert_updated_condition_true(&person_api, name).await;
    check_resource_version_stable(&person_api, name).await;

    cleanup(&person_api, name).await;
});

e2e_test!(kanidm_1103_displayname_and_mail, {
    let s = setup_kanidm_1103().await;
    let name = "test-1103-mail";

    let (person_api, _) = create_and_wait(
        &s,
        name,
        json!({
            "kanidmRef": {"name": KANIDM_NAME},
            "personAttributes": {
                "displayname": "Eugene Marcotte",
                "mail": ["emarcotte@example.com"]
            }
        }),
    )
    .await;

    dump_kanidm_attrs(&s, name).await;
    assert_updated_condition_true(&person_api, name).await;
    check_resource_version_stable(&person_api, name).await;

    cleanup(&person_api, name).await;
});

e2e_test!(kanidm_1103_mixed_case_email, {
    let s = setup_kanidm_1103().await;
    let name = "test-1103-mixed-mail";

    let (person_api, _) = create_and_wait(
        &s,
        name,
        json!({
            "kanidmRef": {"name": KANIDM_NAME},
            "personAttributes": {
                "displayname": "Mixed Case Email",
                "mail": ["MixedCase@Example.COM"]
            }
        }),
    )
    .await;

    dump_kanidm_attrs(&s, name).await;

    let kanidm_person = s.kanidm_client.idm_person_account_get(name).await.unwrap();
    let mail = kanidm_person
        .as_ref()
        .unwrap()
        .attrs
        .get("mail")
        .cloned()
        .unwrap_or_default();
    eprintln!("  mail from Kanidm: {mail:?}");
    eprintln!(
        "  mail from spec:   {:?}",
        vec!["MixedCase@Example.COM".to_string()]
    );

    assert_updated_condition_true(&person_api, name).await;
    check_resource_version_stable(&person_api, name).await;

    cleanup(&person_api, name).await;
});

e2e_test!(kanidm_1103_with_legalname, {
    let s = setup_kanidm_1103().await;
    let name = "test-1103-legal";

    let (person_api, _) = create_and_wait(
        &s,
        name,
        json!({
            "kanidmRef": {"name": KANIDM_NAME},
            "personAttributes": {
                "displayname": "Test Legal",
                "legalname": "Test Legal Name"
            }
        }),
    )
    .await;

    dump_kanidm_attrs(&s, name).await;
    assert_updated_condition_true(&person_api, name).await;
    check_resource_version_stable(&person_api, name).await;

    cleanup(&person_api, name).await;
});

e2e_test!(kanidm_1103_with_timestamps, {
    let s = setup_kanidm_1103().await;
    let name = "test-1103-timestamps";

    let (person_api, _) = create_and_wait(
        &s,
        name,
        json!({
            "kanidmRef": {"name": KANIDM_NAME},
            "personAttributes": {
                "displayname": "Test Timestamps",
                "accountValidFrom": "2024-01-01T00:00:00Z",
                "accountExpire": "2025-12-31T23:59:59Z"
            }
        }),
    )
    .await;

    dump_kanidm_attrs(&s, name).await;
    assert_updated_condition_true(&person_api, name).await;
    check_resource_version_stable(&person_api, name).await;

    cleanup(&person_api, name).await;
});

e2e_test!(kanidm_1103_multiple_mails, {
    let s = setup_kanidm_1103().await;
    let name = "test-1103-multi-mail";

    let (person_api, _) = create_and_wait(
        &s,
        name,
        json!({
            "kanidmRef": {"name": KANIDM_NAME},
            "personAttributes": {
                "displayname": "Multi Mail",
                "mail": ["primary@example.com", "secondary@example.com", "tertiary@example.com"]
            }
        }),
    )
    .await;

    dump_kanidm_attrs(&s, name).await;
    assert_updated_condition_true(&person_api, name).await;
    check_resource_version_stable(&person_api, name).await;

    cleanup(&person_api, name).await;
});

e2e_test!(kanidm_1103_all_fields, {
    let s = setup_kanidm_1103().await;
    let name = "test-1103-all-fields";

    let (person_api, _) = create_and_wait(
        &s,
        name,
        json!({
            "kanidmRef": {"name": KANIDM_NAME},
            "personAttributes": {
                "displayname": "All Fields",
                "mail": ["allfields@example.com"],
                "legalname": "All Fields Legal",
                "accountValidFrom": "2024-01-01T00:00:00Z",
                "accountExpire": "2025-12-31T23:59:59Z"
            }
        }),
    )
    .await;

    dump_kanidm_attrs(&s, name).await;
    assert_updated_condition_true(&person_api, name).await;
    check_resource_version_stable(&person_api, name).await;

    cleanup(&person_api, name).await;
});

// ============================================================================
// Phase 3: User's exact spec scenario
// ============================================================================

e2e_test!(kanidm_1103_user_exact_scenario, {
    let s = setup_kanidm_1103().await;
    let name = "emarcotte";

    let (person_api, _) = create_and_wait(
        &s,
        name,
        json!({
            "kanidmRef": {"name": KANIDM_NAME},
            "personAttributes": {
                "displayname": "Eugene Marcotte",
                "mail": ["emarcotte@example.com"]
            }
        }),
    )
    .await;

    dump_kanidm_attrs(&s, name).await;

    sleep(Duration::from_secs(3)).await;
    let person_after = person_api.get(name).await.unwrap();
    let rv1 = person_after.resource_version();
    eprintln!("  initial resource version: {rv1:?}");

    sleep(Duration::from_secs(5)).await;
    let person_final = person_api.get(name).await.unwrap();
    let rv2 = person_final.resource_version();
    eprintln!("  final resource version:   {rv2:?}");

    assert_eq!(
        rv2, rv1,
        "Resource version must not change over 5 seconds (no reconcile loop)"
    );

    cleanup(&person_api, name).await;
});

// ============================================================================
// Phase 4: Edge cases
// ============================================================================

e2e_test!(kanidm_1103_timestamp_non_z_suffix, {
    let s = setup_kanidm_1103().await;
    let name = "test-1103-ts-offset";

    let (person_api, _) = create_and_wait(
        &s,
        name,
        json!({
            "kanidmRef": {"name": KANIDM_NAME},
            "personAttributes": {
                "displayname": "TS Offset",
                "accountValidFrom": "2024-01-01T00:00:00+00:00",
                "accountExpire": "2025-12-31T23:59:59+00:00"
            }
        }),
    )
    .await;

    dump_kanidm_attrs(&s, name).await;
    assert_updated_condition_true(&person_api, name).await;
    check_resource_version_stable(&person_api, name).await;

    cleanup(&person_api, name).await;
});

e2e_test!(kanidm_1103_displayname_with_spaces, {
    let s = setup_kanidm_1103().await;
    let name = "test-1103-spaces";

    let (person_api, _) = create_and_wait(
        &s,
        name,
        json!({
            "kanidmRef": {"name": KANIDM_NAME},
            "personAttributes": {
                "displayname": "  Eugene Marcotte  "
            }
        }),
    )
    .await;

    dump_kanidm_attrs(&s, name).await;

    let kanidm_person = s.kanidm_client.idm_person_account_get(name).await.unwrap();
    let dn = kanidm_person
        .as_ref()
        .unwrap()
        .attrs
        .get("displayname")
        .and_then(|v| v.first().cloned())
        .unwrap_or_default();
    eprintln!("  displayname from Kanidm: '{dn}'");
    eprintln!("  displayname from spec:   '  Eugene Marcotte  '");

    assert_updated_condition_true(&person_api, name).await;
    check_resource_version_stable(&person_api, name).await;

    cleanup(&person_api, name).await;
});

e2e_test!(kanidm_1103_single_mail_primary_order, {
    let s = setup_kanidm_1103().await;
    let name = "test-1103-mail-order";

    let (person_api, _) = create_and_wait(
        &s,
        name,
        json!({
            "kanidmRef": {"name": KANIDM_NAME},
            "personAttributes": {
                "displayname": "Mail Order",
                "mail": ["z-last@example.com", "a-first@example.com"]
            }
        }),
    )
    .await;

    dump_kanidm_attrs(&s, name).await;

    let kanidm_person = s.kanidm_client.idm_person_account_get(name).await.unwrap();
    let mail = kanidm_person
        .unwrap()
        .attrs
        .get("mail")
        .cloned()
        .unwrap_or_default();
    eprintln!("  mail from Kanidm: {mail:?}");
    eprintln!(
        "  mail from spec:   {:?}",
        vec![
            "z-last@example.com".to_string(),
            "a-first@example.com".to_string()
        ]
    );

    assert_updated_condition_true(&person_api, name).await;
    check_resource_version_stable(&person_api, name).await;

    cleanup(&person_api, name).await;
});

// ============================================================================
// Phase 5: Direct API comparison - dump what Kanidm returns vs what operator sees
// ============================================================================

e2e_test!(kanidm_1103_api_attribute_comparison, {
    let s = setup_kanidm_1103().await;
    let name = "test-1103-api-compare";

    let (person_api, _) = create_and_wait(
        &s,
        name,
        json!({
            "kanidmRef": {"name": KANIDM_NAME},
            "personAttributes": {
                "displayname": "API Compare User",
                "mail": ["apicompare@example.com"],
                "legalname": "API Compare Legal"
            }
        }),
    )
    .await;

    let kanidm_person = s
        .kanidm_client
        .idm_person_account_get(name)
        .await
        .unwrap()
        .unwrap();

    eprintln!("=== Direct API attribute comparison ===");
    eprintln!("Spec displayname:  'API Compare User'");
    eprintln!(
        "Kanidm displayname: '{}'",
        kanidm_person
            .attrs
            .get("displayname")
            .and_then(|v| v.first())
            .unwrap_or(&"<missing>".to_string())
    );
    eprintln!(
        "Spec mail:    {:?}",
        Some(vec!["apicompare@example.com".to_string()])
    );
    eprintln!("Kanidm mail:  {:?}", kanidm_person.attrs.get("mail"));
    eprintln!(
        "Spec legalname:    {:?}",
        Some("API Compare Legal".to_string())
    );
    eprintln!(
        "Kanidm legalname:  {:?}",
        kanidm_person.attrs.get("legalname").and_then(|v| v.first())
    );

    let expected_keys = [
        "displayname",
        "mail",
        "legalname",
        "class",
        "spn",
        "uuid",
        "name",
    ];
    eprintln!("All Kanidm attributes:");
    for (key, values) in kanidm_person.attrs.iter() {
        let known = expected_keys.contains(&key.as_str());
        eprintln!("  {key} (known={known}): {values:?}");
    }

    check_resource_version_stable(&person_api, name).await;
    cleanup(&person_api, name).await;
});

// ============================================================================
// Phase 6: Credential status comparison
// ============================================================================

e2e_test!(kanidm_1103_credential_status_empty, {
    let s = setup_kanidm_1103().await;
    let name = "test-1103-cred-status";

    let (person_api, person) = create_and_wait(
        &s,
        name,
        json!({
            "kanidmRef": {"name": KANIDM_NAME},
            "personAttributes": {
                "displayname": "Cred Status",
                "mail": ["credstatus@example.com"]
            }
        }),
    )
    .await;

    let cred_cond = person
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_ref())
        .and_then(|conds| conds.iter().find(|c| c.type_ == "Credential"));
    eprintln!(
        "Credential condition: status={}, reason={}, message={}",
        cred_cond.as_ref().map(|c| c.status.as_str()).unwrap_or("?"),
        cred_cond.as_ref().map(|c| c.reason.as_str()).unwrap_or("?"),
        cred_cond
            .as_ref()
            .map(|c| c.message.as_str())
            .unwrap_or("?")
    );

    match s
        .kanidm_client
        .idm_person_account_get_credential_status(name)
        .await
    {
        Ok(cs) => {
            eprintln!("Credential status response: {:?}", cs);
            eprintln!("  creds is empty: {}", cs.creds.is_empty());
        }
        Err(kanidm_client::ClientError::EmptyResponse) => {
            eprintln!("Credential status: EmptyResponse (no credentials configured)");
        }
        Err(e) => {
            eprintln!("Credential status error: {e:?}");
        }
    }

    check_resource_version_stable(&person_api, name).await;
    cleanup(&person_api, name).await;
});

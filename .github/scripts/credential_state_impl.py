from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    if old not in text:
        # A generated follow-up run is intentionally idempotent.
        if new in text:
            return text
        raise SystemExit(f"missing replacement anchor: {label}")
    return text.replace(old, new, 1)


# --- CRD: explicit credential observation state + persisted bootstrap expiry.
p = Path("libs/person/src/crd.rs")
s = p.read_text()
marker = "/// The status object of `KanidmPersonAccount`\n"
enum_def = '''/// Observed credential state for a Kanidm person account.
///
/// `Unknown` is deliberately distinct from `Absent`: failures while reading credential
/// attributes must never be interpreted as permission to issue a credential update token.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum CredentialState {
    Present,
    Absent,
    #[default]
    Unknown,
}

'''
if "pub enum CredentialState" not in s:
    s = replace_once(s, marker, enum_def + marker, "credential state enum")
old = '''    pub conditions: Option<Vec<Condition>>,
    pub ready: bool,
'''
new = '''    pub conditions: Option<Vec<Condition>>,
    /// Last credential state observed through Kanidm's read-only person attributes.
    #[serde(default)]
    pub credential_state: CredentialState,
    /// Unix timestamp at which the currently issued bootstrap credential token expires.
    /// Persisting this in status prevents operator restarts from minting duplicate tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials_token_expiry: Option<i64>,
    pub ready: bool,
'''
if "pub credential_state: CredentialState" not in s:
    s = replace_once(s, old, new, "status fields")
p.write_text(s)

# --- Reconciler.
p = Path("libs/person/src/reconcile.rs")
s = p.read_text()
s = s.replace(
    'use crate::crd::{KanidmPersonAccount, KanidmPersonAccountStatus, KanidmPersonAttributes};',
    'use crate::crd::{CredentialState, KanidmPersonAccount, KanidmPersonAccountStatus, KanidmPersonAttributes};',
)
s = s.replace('use kanidm_client::{ClientError, KanidmClient};', 'use kanidm_client::KanidmClient;')
s = s.replace(
    'use kanidm_proto::constants::{ATTR_ACCOUNT_EXPIRE, ATTR_ACCOUNT_VALID_FROM};\nuse kanidm_proto::internal::CUStatus;',
    'use kanidm_proto::constants::{\n    ATTR_ACCOUNT_EXPIRE, ATTR_ACCOUNT_VALID_FROM, ATTR_ATTESTED_PASSKEYS, ATTR_PASSKEYS,\n    ATTR_PRIMARY_CREDENTIAL,\n};',
)
s = s.replace('use kube::runtime::reflector::ObjectRef;\n', '')
old = '''const CONDITION_FALSE: &str = "False";

fn credential_update_status_has_credentials(status: &CUStatus) -> bool {
    status.primary.is_some()
        || status.passkeys.is_empty().not()
        || status.attested_passkeys.is_empty().not()
}
'''
new = '''const CONDITION_FALSE: &str = "False";
const CONDITION_UNKNOWN: &str = "Unknown";

async fn detect_credential_state(
    kanidm_client: &KanidmClient,
    name: &str,
    metrics: &kaniop_operator::metrics::ControllerMetrics,
) -> CredentialState {
    let started = tokio::time::Instant::now();

    // Kanidm's legacy credential-status endpoint only exposes the primary credential.
    // Query the actual credential-bearing attributes directly instead. These are read-only
    // requests, unlike credential_update_begin(), which creates a credential update session
    // and can invalidate a user's in-progress session.
    for attr in [
        ATTR_PRIMARY_CREDENTIAL,
        ATTR_PASSKEYS,
        ATTR_ATTESTED_PASSKEYS,
    ] {
        match kanidm_client.idm_person_account_get_attr(name, attr).await {
            Ok(Some(values)) if values.is_empty().not() => {
                trace!(credential_attr = attr, values_count = values.len(), "credential attribute present");
                metrics.record_kanidm_sdk_outcome(
                    KANIDM_RESOURCE_PERSON,
                    KANIDM_OP_GET_CREDENTIAL_STATUS,
                    KANIDM_OUTCOME_UNCHANGED,
                    started.elapsed(),
                );
                return CredentialState::Present;
            }
            Ok(_) => {}
            Err(e) => {
                warn!(credential_attr = attr, error = ?e, "credential attribute probe failed");
                metrics.record_kanidm_sdk_outcome(
                    KANIDM_RESOURCE_PERSON,
                    KANIDM_OP_GET_CREDENTIAL_STATUS,
                    KANIDM_OUTCOME_ERROR,
                    started.elapsed(),
                );
                return CredentialState::Unknown;
            }
        }
    }

    metrics.record_kanidm_sdk_outcome(
        KANIDM_RESOURCE_PERSON,
        KANIDM_OP_GET_CREDENTIAL_STATUS,
        KANIDM_OUTCOME_UNCHANGED,
        started.elapsed(),
    );
    CredentialState::Absent
}

fn should_create_credentials_token(
    credential_state: CredentialState,
    credentials_token_expiry: Option<i64>,
    now: i64,
) -> bool {
    credential_state == CredentialState::Absent
        && credentials_token_expiry.map_or(true, |expiry| expiry <= now)
}
'''
if "async fn detect_credential_state" not in s:
    s = replace_once(s, old, new, "credential detector")

old = '''        if is_person_false(TYPE_CREDENTIAL, status) {
            let create_token = match ctx.internal_cache.read().await.get(&ObjectRef::from(self)) {
                Some(expiry) if expiry > &OffsetDateTime::now_utc() => {
                    trace!("token not expired, skipping creation");
                    false
                }
                _ => true,
            };
            if create_token {
                debug!(
                    condition = TYPE_CREDENTIAL,
                    status = CONDITION_FALSE,
                    action = "create_reset_token",
                    "condition triggered action"
                );
                record_kanidm_sdk_call(
                    metrics,
                    KANIDM_RESOURCE_PERSON,
                    KANIDM_OP_CREDENTIAL_UPDATE_INTENT,
                    KANIDM_OUTCOME_CHANGED,
                    self.create_reset_token(&kanidm_client, name, ctx.clone()),
                )
                .await?;
                changed = true;
            };
        };
'''
new = '''        if should_create_credentials_token(
            status.credential_state,
            status.credentials_token_expiry,
            OffsetDateTime::now_utc().unix_timestamp(),
        ) {
            debug!(
                credential_state = ?status.credential_state,
                action = "create_reset_token",
                "credential bootstrap required"
            );
            let expiry = record_kanidm_sdk_call(
                metrics,
                KANIDM_RESOURCE_PERSON,
                KANIDM_OP_CREDENTIAL_UPDATE_INTENT,
                KANIDM_OUTCOME_CHANGED,
                self.create_reset_token(&kanidm_client, name, ctx.clone()),
            )
            .await?;
            self.patch_credentials_token_expiry(ctx.clone(), status.clone(), expiry)
                .await?;
            changed = true;
        }
'''
if "credential bootstrap required" not in s:
    s = replace_once(s, old, new, "bootstrap action")

# Scope this replacement to create_reset_token, not other Result<()> functions.
old = '''    async fn create_reset_token(
        &self,
        kanidm_client: &KanidmClient,
        name: &str,
        ctx: Arc<Context>,
    ) -> Result<()> {
'''
new = '''    async fn create_reset_token(
        &self,
        kanidm_client: &KanidmClient,
        name: &str,
        ctx: Arc<Context>,
    ) -> Result<i64> {
'''
if "async fn create_reset_token" in s and ") -> Result<i64>" not in s[s.index("async fn create_reset_token"):s.index("async fn create_reset_token") + 250]:
    s = replace_once(s, old, new, "create token result")
old = '''        ctx.internal_cache
            .write()
            .await
            .insert(ObjectRef::from(self), expiry_time);
        Ok(())
    }

    async fn cleanup(
'''
new = '''        Ok(expiry_time.unix_timestamp())
    }

    async fn patch_credentials_token_expiry(
        &self,
        ctx: Arc<Context>,
        mut status: KanidmPersonAccountStatus,
        expiry: i64,
    ) -> Result<()> {
        status.credentials_token_expiry = Some(expiry);
        let status_patch = Patch::Apply(KanidmPersonAccount {
            status: Some(status),
            ..KanidmPersonAccount::default()
        });
        let patch = PatchParams::apply(PERSON_OPERATOR_NAME).force();
        let api = Api::<KanidmPersonAccount>::namespaced(
            ctx.kaniop_ctx.client.clone(),
            &self.get_namespace(),
        );
        api.patch_status(&self.name_any(), &patch, &status_patch)
            .await
            .map_err(|e| {
                Error::kube_status_error(
                    "KanidmPersonAccount",
                    self.get_namespace(),
                    self.name_any(),
                    e,
                )
            })?;
        Ok(())
    }

    async fn cleanup(
'''
if "async fn patch_credentials_token_expiry" not in s:
    s = replace_once(s, old, new, "persist token expiry")
s = s.replace('''
            ctx.internal_cache
                .write()
                .await
                .remove(&ObjectRef::from(self));
''', '\n')

if "idm_person_account_get_credential_status" in s:
    start = s.index('        let cred_start = tokio::time::Instant::now();')
    end_marker = '        let status = self.generate_status(current_person, credential_present)?;'
    end = s.index(end_marker, start) + len(end_marker)
    replacement = '''        let credential_state = if current_person.is_some() {
            detect_credential_state(&kanidm_client, &name, metrics).await
        } else {
            CredentialState::Unknown
        };

        let status = self.generate_status(current_person, credential_state)?;'''
    s = s[:start] + replacement + s[end:]

s = s.replace(
    '        credential_present: Option<bool>,\n',
    '        credential_state: CredentialState,\n',
    1,
)
old = '''                let credentials_condition = credential_present.map(|c| {
                    if c {
                        Condition {
                            type_: TYPE_CREDENTIAL.to_string(),
                            status: CONDITION_TRUE.to_string(),
                            reason: "Present".to_string(),
                            message: "Credentials are present.".to_string(),
                            last_transition_time: last_transition_time(
                                current_conditions,
                                TYPE_CREDENTIAL,
                                CONDITION_TRUE,
                                "Present",
                            ),
                            observed_generation: self.metadata.generation,
                        }
                    } else {
                        Condition {
                            type_: TYPE_CREDENTIAL.to_string(),
                            status: CONDITION_FALSE.to_string(),
                            reason: "NotPresent".to_string(),
                            message: "Credentials are not present.".to_string(),
                            last_transition_time: last_transition_time(
                                current_conditions,
                                TYPE_CREDENTIAL,
                                CONDITION_FALSE,
                                "NotPresent",
                            ),
                            observed_generation: self.metadata.generation,
                        }
                    }
                });
'''
new = '''                let credentials_condition = match credential_state {
                    CredentialState::Present => Condition {
                        type_: TYPE_CREDENTIAL.to_string(),
                        status: CONDITION_TRUE.to_string(),
                        reason: "Present".to_string(),
                        message: "Credentials are present.".to_string(),
                        last_transition_time: last_transition_time(
                            current_conditions,
                            TYPE_CREDENTIAL,
                            CONDITION_TRUE,
                            "Present",
                        ),
                        observed_generation: self.metadata.generation,
                    },
                    CredentialState::Absent => Condition {
                        type_: TYPE_CREDENTIAL.to_string(),
                        status: CONDITION_FALSE.to_string(),
                        reason: "NotPresent".to_string(),
                        message: "Credentials are not present.".to_string(),
                        last_transition_time: last_transition_time(
                            current_conditions,
                            TYPE_CREDENTIAL,
                            CONDITION_FALSE,
                            "NotPresent",
                        ),
                        observed_generation: self.metadata.generation,
                    },
                    CredentialState::Unknown => Condition {
                        type_: TYPE_CREDENTIAL.to_string(),
                        status: CONDITION_UNKNOWN.to_string(),
                        reason: "Unknown".to_string(),
                        message: "Unable to determine credential state.".to_string(),
                        last_transition_time: last_transition_time(
                            current_conditions,
                            TYPE_CREDENTIAL,
                            CONDITION_UNKNOWN,
                            "Unknown",
                        ),
                        observed_generation: self.metadata.generation,
                    },
                };
'''
if "let credentials_condition = credential_present.map" in s:
    s = replace_once(s, old, new, "credential condition")
s = s.replace('.chain(credentials_condition)\n', '.chain([credentials_condition])\n', 1)
old = '''                Ok(KanidmPersonAccountStatus {
                    conditions: Some(conditions),
                    ready: status,
                    gid: current_person_posix.gidnumber,
                    kanidm_ref: self.kanidm_ref(),
                })
'''
new = '''                let credentials_token_expiry = match credential_state {
                    CredentialState::Present => None,
                    CredentialState::Absent | CredentialState::Unknown => self
                        .status
                        .as_ref()
                        .and_then(|status| status.credentials_token_expiry),
                };
                Ok(KanidmPersonAccountStatus {
                    conditions: Some(conditions),
                    credential_state,
                    credentials_token_expiry,
                    ready: status,
                    gid: current_person_posix.gidnumber,
                    kanidm_ref: self.kanidm_ref(),
                })
'''
if "credential_state," not in s[s.index("Ok(KanidmPersonAccountStatus {"):s.index("Ok(KanidmPersonAccountStatus {") + 400]:
    s = replace_once(s, old, new, "existing status fields")
old = '''                Ok(KanidmPersonAccountStatus {
                    conditions: Some(conditions),
                    ready: false,
                    gid: None,
                    kanidm_ref: self.kanidm_ref(),
                })
'''
new = '''                Ok(KanidmPersonAccountStatus {
                    conditions: Some(conditions),
                    credential_state: CredentialState::Unknown,
                    credentials_token_expiry: None,
                    ready: false,
                    gid: None,
                    kanidm_ref: self.kanidm_ref(),
                })
'''
if "credential_state: CredentialState::Unknown" not in s:
    s = replace_once(s, old, new, "missing status fields")

# Replace obsolete CUStatus unit tests with fail-closed action-gating tests.
if "passkey_only_credential_update_status_has_credentials" in s:
    test_idx = s.index('\n#[cfg(test)]\nmod tests {')
    s = s[:test_idx] + '''
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_token_is_only_created_for_absent_credentials() {
        let now = 1_000;
        assert!(should_create_credentials_token(
            CredentialState::Absent,
            None,
            now
        ));
        assert!(should_create_credentials_token(
            CredentialState::Absent,
            Some(now),
            now
        ));
        assert!(!should_create_credentials_token(
            CredentialState::Absent,
            Some(now + 1),
            now
        ));
        assert!(!should_create_credentials_token(
            CredentialState::Present,
            None,
            now
        ));
        assert!(!should_create_credentials_token(
            CredentialState::Unknown,
            None,
            now
        ));
    }
}
'''
p.write_text(s)

# --- WebAuthn test dependencies.
p = Path("Cargo.toml")
s = p.read_text()
anchor = 'kanidm_proto = "1.11.2"\n'
deps = 'webauthn-authenticator-rs = { version = "0.6.1-dev", default-features = false }\nwebauthn-rs = { version = "0.6.1-dev", features = ["preview-features"] }\n'
if "webauthn-authenticator-rs" not in s:
    s = replace_once(s, anchor, anchor + deps, "workspace webauthn deps")
p.write_text(s)

p = Path("tests/Cargo.toml")
s = p.read_text()
anchor = 'kanidm_proto = { workspace = true }\n'
deps = 'webauthn-authenticator-rs = { workspace = true, features = ["softpasskey", "softtoken"] }\nwebauthn-rs = { workspace = true }\n'
if "webauthn-authenticator-rs" not in s:
    s = replace_once(s, anchor, anchor + deps, "test webauthn deps")
p.write_text(s)

# --- E2E helpers and complete credential matrix.
p = Path("tests/e2e/test/person.rs")
s = p.read_text()
s = s.replace(
    'use kaniop_person::crd::KanidmPersonAccount;',
    'use kaniop_person::crd::{CredentialState, KanidmPersonAccount};',
)
if "webauthn_authenticator_rs::softpasskey" not in s:
    s = replace_once(
        s,
        'use kanidm_proto::internal::CURegState;\n',
        'use kanidm_proto::internal::CURegState;\nuse webauthn_authenticator_rs::softpasskey::SoftPasskey;\nuse webauthn_authenticator_rs::softtoken::{self, SoftToken};\nuse webauthn_authenticator_rs::WebauthnAuthenticator;\nuse webauthn_rs::prelude::AttestationCaListBuilder;\n',
        "webauthn imports",
    )
if "api::apps::v1::Deployment" not in s:
    s = replace_once(
        s,
        'use k8s_openapi::api::core::v1::Event;\n',
        'use k8s_openapi::api::apps::v1::Deployment;\nuse k8s_openapi::api::core::v1::Event;\n',
        "deployment import",
    )

if "fn has_credential_state" not in s:
    helper_anchor = 'fn is_person_ready() -> impl Condition<KanidmPersonAccount> {'
    helper_pos = s.index(helper_anchor)
    helper_end = s.index('\n}\n\n', helper_pos) + 3
    helpers = r'''

fn has_credential_state(state: CredentialState) -> impl Condition<KanidmPersonAccount> {
    move |obj: Option<&KanidmPersonAccount>| {
        obj.and_then(|person| person.status.as_ref())
            .is_some_and(|status| status.credential_state == state)
    }
}

async fn force_person_reconcile(api: &Api<KanidmPersonAccount>, name: &str) {
    api.patch(
        name,
        &PatchParams::default(),
        &Patch::Merge(&json!({
            "metadata": {"annotations": {"kaniop/e2e-force-update": Timestamp::now().to_string()}}
        })),
    )
    .await
    .unwrap();
}

async fn token_event_count(client: Client, namespace: &str, uid: &str) -> usize {
    let event_api = Api::<Event>::namespaced(client, namespace);
    let opts = ListParams::default().fields(&format!(
        "involvedObject.kind=KanidmPersonAccount,involvedObject.apiVersion=kaniop.rs/v1beta1,involvedObject.uid={uid}"
    ));
    event_api
        .list(&opts)
        .await
        .unwrap()
        .items
        .iter()
        .filter(|event| event.reason.as_deref() == Some("TokenCreated"))
        .count()
}

async fn create_person_for_credential_test(
    api: &Api<KanidmPersonAccount>,
    name: &str,
    displayname: &str,
) -> String {
    api.delete(name, &DeleteParams::default()).await.ok();
    let person_spec = json!({
        "kanidmRef": {"name": KANIDM_NAME},
        "personAttributes": {"displayname": displayname},
    });
    let person = KanidmPersonAccount::new(name, serde_json::from_value(person_spec).unwrap());
    let uid = api
        .create(&PostParams::default(), &person)
        .await
        .unwrap()
        .uid()
        .unwrap();
    wait_for(api.clone(), name, is_person("Exists")).await;
    wait_for(
        api.clone(),
        name,
        has_credential_state(CredentialState::Absent),
    )
    .await;
    poll_until("initial TokenCreated event", || {
        let client = api.clone().into_client();
        let uid = uid.clone();
        async move { (token_event_count(client, "default", &uid).await == 1).then_some(()) }
    })
    .await;
    uid
}

async fn setup_passkey(client: &kanidm_client::KanidmClient, name: &str) {
    let intent = client
        .idm_person_account_credential_update_intent(name, Some(1234))
        .await
        .unwrap();
    let session_client = client.new_session().unwrap();
    let (session_token, _) = session_client
        .idm_account_credential_update_exchange(intent.token)
        .await
        .unwrap();
    let status = session_client
        .idm_account_credential_update_passkey_init(&session_token)
        .await
        .unwrap();
    let challenge = match status.mfaregstate {
        CURegState::Passkey(challenge) => challenge,
        other => panic!("unexpected passkey registration state: {other:?}"),
    };
    let mut authenticator = SoftPasskey::new(true);
    let response = authenticator
        .do_registration(session_client.get_origin().clone(), challenge)
        .unwrap();
    session_client
        .idm_account_credential_update_passkey_finish(
            &session_token,
            "e2e-passkey".to_string(),
            response,
        )
        .await
        .unwrap();
    session_client
        .idm_account_credential_update_commit(&session_token)
        .await
        .unwrap();
}

async fn setup_attested_passkey(client: &kanidm_client::KanidmClient, name: &str) -> String {
    let group = format!("{name}-attested-policy");
    client.idm_group_create(&group, None).await.unwrap();
    client.group_account_policy_enable(&group).await.unwrap();
    client.idm_group_add_members(&group, &[name]).await.unwrap();

    let (mut authenticator, ca_root) = SoftToken::new(true).unwrap();
    let mut builder = AttestationCaListBuilder::new();
    builder
        .insert_device_x509(
            ca_root,
            softtoken::AAGUID,
            "e2e-softtoken".to_string(),
            Default::default(),
        )
        .unwrap();
    let attestation = serde_json::to_string(&builder.build()).unwrap();
    client
        .group_account_policy_webauthn_attestation_set(&group, &attestation)
        .await
        .unwrap();

    let intent = client
        .idm_person_account_credential_update_intent(name, Some(1234))
        .await
        .unwrap();
    let session_client = client.new_session().unwrap();
    let (session_token, _) = session_client
        .idm_account_credential_update_exchange(intent.token)
        .await
        .unwrap();
    let status = session_client
        .idm_account_credential_update_attested_passkey_init(&session_token)
        .await
        .unwrap();
    let challenge = match status.mfaregstate {
        CURegState::AttestedPasskey(challenge) => challenge,
        other => panic!("unexpected attested passkey registration state: {other:?}"),
    };
    let response = authenticator
        .do_registration(session_client.get_origin().clone(), challenge)
        .unwrap();
    session_client
        .idm_account_credential_update_attested_passkey_finish(
            &session_token,
            "e2e-attested-passkey".to_string(),
            response,
        )
        .await
        .unwrap();
    session_client
        .idm_account_credential_update_commit(&session_token)
        .await
        .unwrap();
    group
}

async fn restart_operator(client: Client) {
    let deployments = Api::<Deployment>::namespaced(client, "kaniop");
    let deployment = deployments.get("kaniop").await.unwrap();
    let previous_generation = deployment.metadata.generation.unwrap_or_default();
    deployments
        .patch(
            "kaniop",
            &PatchParams::default(),
            &Patch::Merge(&json!({
                "spec": {"template": {"metadata": {"annotations": {
                    "kaniop/e2e-restarted-at": Timestamp::now().to_string()
                }}}}
            })),
        )
        .await
        .unwrap();
    poll_until("operator deployment rollout", || {
        let deployments = deployments.clone();
        async move {
            let deployment = deployments.get("kaniop").await.ok()?;
            let status = deployment.status?;
            let generation = deployment.metadata.generation.unwrap_or_default();
            (generation > previous_generation
                && status.observed_generation.unwrap_or_default() >= generation
                && status.updated_replicas == status.replicas
                && status.available_replicas == status.replicas)
                .then_some(())
        }
    })
    .await;
}
'''
    s = s[:helper_end] + helpers + s[helper_end:]

if "person_credential_false_for_new_user" in s:
    test_start = s.index('e2e_test!(person_credential_false_for_new_user, {')
    test_end = s.index('e2e_test!(person_resource_version_stable_minimal, {', test_start)
    tests = r'''e2e_test!(person_credential_bootstrap_persists_token_expiry, {
    let name = "test-cred-bootstrap-state";
    let s = setup_kanidm_connection(KANIDM_NAME).await;
    let person_api = Api::<KanidmPersonAccount>::namespaced(s.client.clone(), "default");
    let uid = create_person_for_credential_test(&person_api, name, "Credential Bootstrap").await;

    let status = person_api.get(name).await.unwrap().status.unwrap();
    assert_eq!(status.credential_state, CredentialState::Absent);
    assert!(status.credentials_token_expiry.is_some());

    for _ in 0..2 {
        force_person_reconcile(&person_api, name).await;
        tokio::time::sleep(stabilization_delay()).await;
    }
    assert_eq!(token_event_count(s.client.clone(), "default", &uid).await, 1);

    person_api.delete(name, &DeleteParams::default()).await.unwrap();
});

e2e_test!(person_credential_present_with_password, {
    let name = "test-cred-password";
    let s = setup_kanidm_connection(KANIDM_NAME).await;
    let person_api = Api::<KanidmPersonAccount>::namespaced(s.client.clone(), "default");
    let uid = create_person_for_credential_test(&person_api, name, "Password Credential").await;

    s.kanidm_client
        .idm_person_account_primary_credential_set_password(name, "e2e-test-password-123")
        .await
        .unwrap();
    force_person_reconcile(&person_api, name).await;
    wait_for(
        person_api.clone(),
        name,
        has_credential_state(CredentialState::Present),
    )
    .await;
    wait_for(person_api.clone(), name, is_person("Credential")).await;
    assert_eq!(token_event_count(s.client.clone(), "default", &uid).await, 1);
    assert!(
        person_api
            .get(name)
            .await
            .unwrap()
            .status
            .unwrap()
            .credentials_token_expiry
            .is_none()
    );

    person_api.delete(name, &DeleteParams::default()).await.unwrap();
});

e2e_test!(person_credential_present_with_passkey, {
    let name = "test-cred-passkey";
    let s = setup_kanidm_connection(KANIDM_NAME).await;
    let person_api = Api::<KanidmPersonAccount>::namespaced(s.client.clone(), "default");
    let uid = create_person_for_credential_test(&person_api, name, "Passkey Credential").await;

    setup_passkey(&s.kanidm_client, name).await;
    force_person_reconcile(&person_api, name).await;
    wait_for(
        person_api.clone(),
        name,
        has_credential_state(CredentialState::Present),
    )
    .await;
    wait_for(person_api.clone(), name, is_person("Credential")).await;
    assert_eq!(token_event_count(s.client.clone(), "default", &uid).await, 1);

    person_api.delete(name, &DeleteParams::default()).await.unwrap();
});

e2e_test!(person_credential_present_with_attested_passkey, {
    let name = "test-cred-attested-passkey";
    let s = setup_kanidm_connection(KANIDM_NAME).await;
    let person_api = Api::<KanidmPersonAccount>::namespaced(s.client.clone(), "default");
    let uid = create_person_for_credential_test(
        &person_api,
        name,
        "Attested Passkey Credential",
    )
    .await;

    let policy_group = setup_attested_passkey(&s.kanidm_client, name).await;
    force_person_reconcile(&person_api, name).await;
    wait_for(
        person_api.clone(),
        name,
        has_credential_state(CredentialState::Present),
    )
    .await;
    wait_for(person_api.clone(), name, is_person("Credential")).await;
    assert_eq!(token_event_count(s.client.clone(), "default", &uid).await, 1);

    person_api.delete(name, &DeleteParams::default()).await.unwrap();
    s.kanidm_client.idm_group_delete(&policy_group).await.unwrap();
});

e2e_test!(person_reconcile_preserves_active_credential_update_session, {
    let name = "test-cred-active-session";
    let s = setup_kanidm_connection(KANIDM_NAME).await;
    let person_api = Api::<KanidmPersonAccount>::namespaced(s.client.clone(), "default");
    create_person_for_credential_test(&person_api, name, "Active Credential Session").await;
    setup_passkey(&s.kanidm_client, name).await;
    force_person_reconcile(&person_api, name).await;
    wait_for(
        person_api.clone(),
        name,
        has_credential_state(CredentialState::Present),
    )
    .await;

    let client = create_fresh_authenticated_client(KANIDM_NAME).await;
    let (session_token, _) = client.idm_account_credential_update_begin(name).await.unwrap();

    force_person_reconcile(&person_api, name).await;
    tokio::time::sleep(stabilization_delay()).await;

    client
        .idm_account_credential_update_status(&session_token)
        .await
        .expect("reconciliation must not invalidate an active credential update session");
    let _: std::result::Result<(), ClientError> = client
        .perform_post_request("/v1/credential/_cancel", &session_token)
        .await;

    person_api.delete(name, &DeleteParams::default()).await.unwrap();
});

e2e_test!(person_existing_credentials_survive_operator_restart_without_token, {
    let name = "test-cred-restart-present";
    let s = setup_kanidm_connection(KANIDM_NAME).await;
    let person_api = Api::<KanidmPersonAccount>::namespaced(s.client.clone(), "default");
    let uid = create_person_for_credential_test(
        &person_api,
        name,
        "Credential Restart Present",
    )
    .await;
    setup_passkey(&s.kanidm_client, name).await;
    force_person_reconcile(&person_api, name).await;
    wait_for(
        person_api.clone(),
        name,
        has_credential_state(CredentialState::Present),
    )
    .await;
    assert_eq!(token_event_count(s.client.clone(), "default", &uid).await, 1);

    restart_operator(s.client.clone()).await;
    force_person_reconcile(&person_api, name).await;
    wait_for(
        person_api.clone(),
        name,
        has_credential_state(CredentialState::Present),
    )
    .await;
    tokio::time::sleep(stabilization_delay()).await;
    assert_eq!(token_event_count(s.client.clone(), "default", &uid).await, 1);

    person_api.delete(name, &DeleteParams::default()).await.unwrap();
});

e2e_test!(person_bootstrap_state_survives_operator_restart, {
    let name = "test-cred-restart-bootstrap";
    let s = setup_kanidm_connection(KANIDM_NAME).await;
    let person_api = Api::<KanidmPersonAccount>::namespaced(s.client.clone(), "default");
    let uid = create_person_for_credential_test(
        &person_api,
        name,
        "Credential Restart Bootstrap",
    )
    .await;
    let expiry_before = person_api
        .get(name)
        .await
        .unwrap()
        .status
        .unwrap()
        .credentials_token_expiry;
    assert!(expiry_before.is_some());

    restart_operator(s.client.clone()).await;
    force_person_reconcile(&person_api, name).await;
    wait_for(
        person_api.clone(),
        name,
        has_credential_state(CredentialState::Absent),
    )
    .await;
    tokio::time::sleep(stabilization_delay()).await;

    let status_after = person_api.get(name).await.unwrap().status.unwrap();
    assert_eq!(status_after.credentials_token_expiry, expiry_before);
    assert_eq!(token_event_count(s.client.clone(), "default", &uid).await, 1);

    person_api.delete(name, &DeleteParams::default()).await.unwrap();
});

'''
    s = s[:test_start] + tests + s[test_end:]
p.write_text(s)

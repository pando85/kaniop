//! KanidmRadiusServer end-to-end tests with live RADIUS authentication.
//!
//! Connectivity design (plan D6, test-only; no CRD field):
//!
//! - One shared Kanidm `radius-e2e` with the e2e certificate's own name
//!   (`idm.example.com`) and `origin: https://idm.example.com:8443`, so the
//!   Kanidm URL derived by the controller carries the Service port and matches
//!   the certificate SAN.
//! - A CoreDNS exact rewrite `idm.example.com` -> the Kanidm Service. The
//!   `*.localhost` names the rest of the harness uses cannot be used: they
//!   resolve to `::1` inside the RADIUS image.
//! - A test-owned Ingress `radius-e2e.localhost` gives the host-side harness
//!   client access to the same Kanidm.
//! - NAS clients are pods running the RADIUS image itself (`radclient`),
//!   registered as `<podIP>/32`.
//!
//! Token revocation and config Secret garbage collection are held back by the
//! controller's 10 minute orphan grace, so tests asserting them wait that
//! long. They run in parallel with each other.

use super::kanidm::{CERT, KEY, is_kanidm, setup};
use super::{
    KANIDM_SETUP_LOCK, check_event_with_timeout, init_crypto_provider, poll_until_with_timeout,
    wait_for,
};

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, Event, Namespace, Pod, Secret, Service};
use k8s_openapi::api::networking::v1::Ingress;
use kanidm_client::{ClientError, KanidmClient, KanidmClientBuilder};
use kaniop_operator::kanidm::crd::Kanidm;
use kaniop_radius::crd::KanidmRadiusServer;
use kube::api::{
    Api, AttachParams, DeleteParams, ListParams, LogParams, Patch, PatchParams, PostParams,
};
use kube::runtime::conditions;
use kube::{Client, ResourceExt};
use serde_json::json;
use tokio::sync::Semaphore;
use tokio::time::sleep;

const KANIDM_NAME: &str = "radius-e2e";
const KANIDM_DOMAIN: &str = "idm.example.com";
const KANIDM_ORIGIN: &str = "https://idm.example.com:8443";
const KANIDM_INGRESS_HOST: &str = "radius-e2e.localhost";
const CA_CONFIG_MAP: &str = "radius-e2e-kanidm-ca";
const DNS_PROBE_POD: &str = "radius-e2e-dns-probe";
const RADIUS_GROUP: &str = "idm_radius_servers";
const E2E_LABEL: &str = "kaniop.rs/e2e";
const E2E_LABEL_VALUE: &str = "radius";
const RADIUS_UID_LABEL: &str = "kaniop.rs/radius-uid";
const ROTATE_ANNOTATION: &str = "kaniop.rs/radius-rotate-token";
const INPUTS_DIGEST_ANNOTATION: &str = "kaniop.rs/radius-inputs-digest";
const NAS_SECRET_KEY: &str = "nas";
const NAS_SECRET: &str = "nas-shared-secret-one";

/// Upper bound for anything gated by the controller's 10 minute orphan grace.
const GRACE_WAIT: Duration = Duration::from_secs(16 * 60);
const STEP_WAIT: Duration = Duration::from_secs(4 * 60);

static FIXTURE_LOCK: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(1));
static FIXTURE_READY: AtomicBool = AtomicBool::new(false);

fn radius_image() -> String {
    format!(
        "kanidm/radius:{}",
        crate::kanidm::get_dependency_version().unwrap()
    )
}

fn ca_pem() -> String {
    let cert = std::str::from_utf8(CERT).unwrap();
    let marker = "-----BEGIN CERTIFICATE-----";
    let second = cert.match_indices(marker).nth(1).unwrap().0;
    cert[second..].to_string()
}

fn e2e_labels() -> serde_json::Value {
    json!({ E2E_LABEL: E2E_LABEL_VALUE })
}

// ---------------------------------------------------------------------------
// Shared fixture: Kanidm, ingress, CA, CoreDNS rewrite
// ---------------------------------------------------------------------------

struct Env {
    client: Client,
    idm: KanidmClient,
}

async fn env() -> Env {
    init_crypto_provider();
    let client = Client::try_default().await.unwrap();
    ensure_fixture(&client).await;
    let idm = admin_client(&client).await;
    Env { client, idm }
}

async fn ensure_fixture(client: &Client) {
    let _permit = FIXTURE_LOCK.acquire().await.unwrap();
    if FIXTURE_READY.load(Ordering::SeqCst) {
        return;
    }
    ensure_kanidm(client).await;
    ensure_ingress(client).await;
    ensure_ca_config_map(client).await;
    ensure_dns_rewrite(client).await;
    FIXTURE_READY.store(true, Ordering::SeqCst);
}

async fn ensure_kanidm(client: &Client) {
    let api = Api::<Kanidm>::namespaced(client.clone(), "default");
    {
        // Same lock the generic Kanidm helpers use, so creation never races.
        let _guard = KANIDM_SETUP_LOCK.acquire().await.unwrap();
        if api.get_opt(KANIDM_NAME).await.unwrap().is_none() {
            setup(KANIDM_NAME, Some(json!({ "origin": KANIDM_ORIGIN }))).await;
        }
    }
    wait_for(api.clone(), KANIDM_NAME, is_kanidm("Available")).await;
    wait_for(api, KANIDM_NAME, is_kanidm("Initialized")).await;
}

async fn ensure_ingress(client: &Client) {
    let api = Api::<Ingress>::namespaced(client.clone(), "default");
    if api.get_opt(KANIDM_NAME).await.unwrap().is_some() {
        return;
    }
    let ingress: Ingress = serde_json::from_value(json!({
        "apiVersion": "networking.k8s.io/v1",
        "kind": "Ingress",
        "metadata": {
            "name": KANIDM_NAME,
            "annotations": {"nginx.ingress.kubernetes.io/backend-protocol": "HTTPS"},
        },
        "spec": {
            "ingressClassName": "nginx",
            "rules": [{
                "host": KANIDM_INGRESS_HOST,
                "http": {"paths": [{
                    "path": "/",
                    "pathType": "Prefix",
                    "backend": {"service": {"name": KANIDM_NAME, "port": {"number": 8443}}},
                }]},
            }],
        },
    }))
    .unwrap();
    api.create(&PostParams::default(), &ingress).await.unwrap();
}

async fn ensure_ca_config_map(client: &Client) {
    let api = Api::<ConfigMap>::namespaced(client.clone(), "default");
    if api.get_opt(CA_CONFIG_MAP).await.unwrap().is_some() {
        return;
    }
    let config_map: ConfigMap = serde_json::from_value(json!({
        "metadata": {"name": CA_CONFIG_MAP},
        "data": {"ca.crt": ca_pem()},
    }))
    .unwrap();
    api.create(&PostParams::default(), &config_map)
        .await
        .unwrap();
}

fn client_pod(name: &str, extra_volume: Option<&str>) -> Pod {
    let mut spec = json!({
        "terminationGracePeriodSeconds": 0,
        "containers": [{
            "name": "client",
            "image": radius_image(),
            "command": ["sleep", "infinity"],
        }],
    });
    if let Some(secret) = extra_volume {
        spec["volumes"] = json!([{"name": "config", "secret": {"secretName": secret}}]);
        spec["containers"][0]["volumeMounts"] =
            json!([{"name": "config", "mountPath": "/config", "readOnly": true}]);
    }
    serde_json::from_value(json!({
        "metadata": {"name": name, "labels": e2e_labels()},
        "spec": spec,
    }))
    .unwrap()
}

fn is_pod_ready(pod: &Pod) -> bool {
    pod.status
        .as_ref()
        .and_then(|s| s.conditions.as_ref())
        .is_some_and(|c| c.iter().any(|c| c.type_ == "Ready" && c.status == "True"))
}

async fn exec_sh(pods: &Api<Pod>, pod: &str, script: &str) -> String {
    let attached = match pods
        .exec(
            pod,
            vec!["sh", "-c", script],
            &AttachParams::default().container("client"),
        )
        .await
    {
        Ok(attached) => attached,
        Err(error) => return format!("exec failed: {error}"),
    };
    kaniop_k8s_util::client::get_output(attached)
        .await
        .unwrap_or_else(|error| format!("exec error: {error}"))
}

/// CoreDNS rewrite of the Kanidm domain to the Kanidm Service, so the
/// RADIUS pod verifies TLS against the name in the certificate.
async fn ensure_dns_rewrite(client: &Client) {
    let config_maps = Api::<ConfigMap>::namespaced(client.clone(), "kube-system");
    let corefile = config_maps
        .get("coredns")
        .await
        .unwrap()
        .data
        .unwrap()
        .get("Corefile")
        .cloned()
        .unwrap();
    let rule =
        format!("rewrite name exact {KANIDM_DOMAIN} {KANIDM_NAME}.default.svc.cluster.local");
    if !corefile.contains(&rule) {
        let patched = corefile.replacen(".:53 {\n", &format!(".:53 {{\n    {rule}\n"), 1);
        assert_ne!(patched, corefile, "unexpected Corefile layout:\n{corefile}");
        config_maps
            .patch(
                "coredns",
                &PatchParams::default(),
                &Patch::Merge(json!({"data": {"Corefile": patched}})),
            )
            .await
            .unwrap();
        let deployments = Api::<Deployment>::namespaced(client.clone(), "kube-system");
        deployments
            .patch(
                "coredns",
                &PatchParams::default(),
                &Patch::Merge(json!({"spec": {"template": {"metadata": {"annotations": {
                    "kubectl.kubernetes.io/restartedAt": jiff_now(),
                }}}}})),
            )
            .await
            .unwrap();
        poll_until_with_timeout("CoreDNS rollout", STEP_WAIT, || {
            let deployments = deployments.clone();
            async move {
                let d = deployments.get("coredns").await.ok()?;
                let want = d.spec.as_ref()?.replicas?;
                let s = d.status.as_ref()?;
                (s.observed_generation >= d.metadata.generation
                    && s.updated_replicas == Some(want)
                    && s.ready_replicas == Some(want)
                    && s.replicas == Some(want))
                .then_some(())
            }
        })
        .await;
    }

    // Prove resolution from inside the cluster, with the image the RADIUS
    // pods use, before any RADIUS server is created.
    let pods = Api::<Pod>::namespaced(client.clone(), "default");
    if pods.get_opt(DNS_PROBE_POD).await.unwrap().is_none() {
        pods.create(&PostParams::default(), &client_pod(DNS_PROBE_POD, None))
            .await
            .unwrap();
    }
    wait_for(pods.clone(), DNS_PROBE_POD, |p: Option<&Pod>| {
        p.is_some_and(is_pod_ready)
    })
    .await;
    let service_ip = Api::<Service>::namespaced(client.clone(), "default")
        .get(KANIDM_NAME)
        .await
        .unwrap()
        .spec
        .unwrap()
        .cluster_ip
        .unwrap();
    poll_until_with_timeout("idm.example.com resolves in cluster", STEP_WAIT, || {
        let pods = pods.clone();
        let service_ip = service_ip.clone();
        async move {
            let out = exec_sh(
                &pods,
                DNS_PROBE_POD,
                &format!("getent hosts {KANIDM_DOMAIN}"),
            )
            .await;
            out.contains(&service_ip).then_some(())
        }
    })
    .await;
}

fn jiff_now() -> String {
    k8s_openapi::jiff::Timestamp::now().to_string()
}

async fn admin_client(client: &Client) -> KanidmClient {
    let secrets = Api::<Secret>::namespaced(client.clone(), "default");
    let secret = secrets
        .get(&format!("{KANIDM_NAME}-admin-passwords"))
        .await
        .unwrap();
    let password = String::from_utf8(
        secret
            .data
            .unwrap()
            .get("IDM_ADMIN_PASSWORD")
            .unwrap()
            .0
            .clone(),
    )
    .unwrap();
    let idm = KanidmClientBuilder::new()
        .danger_accept_invalid_certs(true)
        .address(format!("https://{KANIDM_INGRESS_HOST}"))
        .connect_timeout(5)
        .build()
        .unwrap();
    poll_until_with_timeout("idm_admin login through ingress", STEP_WAIT, || {
        let idm = &idm;
        let password = password.clone();
        async move { idm.auth_simple_password("idm_admin", &password).await.ok() }
    })
    .await;
    idm
}

// ---------------------------------------------------------------------------
// Kanidm identities used to authenticate
// ---------------------------------------------------------------------------

struct Users {
    allowed_spn: String,
    allowed: String,
    allowed_secret: String,
    denied: String,
    denied_secret: String,
}

async fn create_users(idm: &KanidmClient, prefix: &str) -> Users {
    let group = format!("{prefix}-allowed");
    // Conflicts are tolerated so a test can be re-run against the same Kanidm.
    let created = |result: Result<(), ClientError>| match result {
        Err(ClientError::Http(status, _, _)) if status.as_u16() == 409 => (),
        other => other.unwrap(),
    };
    created(idm.idm_group_create(&group, None).await);
    let allowed = format!("{prefix}-user");
    let denied = format!("{prefix}-denied");
    for user in [&allowed, &denied] {
        created(idm.idm_person_account_create(user, user).await);
    }
    idm.idm_group_add_members(&group, &[&allowed])
        .await
        .unwrap();
    let allowed_secret = idm
        .idm_account_radius_credential_regenerate(&allowed)
        .await
        .unwrap();
    let denied_secret = idm
        .idm_account_radius_credential_regenerate(&denied)
        .await
        .unwrap();
    Users {
        allowed_spn: format!("{group}@{KANIDM_DOMAIN}"),
        allowed,
        allowed_secret,
        denied,
        denied_secret,
    }
}

// ---------------------------------------------------------------------------
// RADIUS requests from a NAS pod
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Method {
    Pap,
    Mschap,
}

#[derive(Debug)]
enum Outcome {
    Accept(String),
    Reject(String),
    NoReply(String),
    Other(String),
}

impl Outcome {
    fn is_accept(&self) -> bool {
        matches!(self, Outcome::Accept(_))
    }

    /// Raw `radclient` output, for failure messages.
    fn output(&self) -> &str {
        match self {
            Outcome::Accept(out)
            | Outcome::Reject(out)
            | Outcome::NoReply(out)
            | Outcome::Other(out) => out,
        }
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

struct Login<'a> {
    user: &'a str,
    password: &'a str,
    nas_secret: &'a str,
    method: Method,
}

/// One authentication request through `radclient` (what `radtest` runs).
/// `retries` is the number of re-sends after the 2 second timeout.
async fn radius_login(
    pods: &Api<Pod>,
    nas_pod: &str,
    server: &str,
    login: &Login<'_>,
    retries: u32,
) -> Outcome {
    let attribute = match login.method {
        Method::Pap => "User-Password",
        Method::Mschap => "MS-CHAP-Password",
    };
    let script = format!(
        "printf 'User-Name = \"%s\"\\n%s = \"%s\"\\nNAS-IP-Address = 127.0.0.1\\nNAS-Port = 0\\n\
         Message-Authenticator = 0x00\\n' {user} {attribute} {password} \
         | radclient -r {retries} -t 2 -x {server} auth {secret} 2>&1; echo rc=$?",
        user = shell_quote(login.user),
        password = shell_quote(login.password),
        secret = shell_quote(login.nas_secret),
    );
    let out = exec_sh(pods, nas_pod, &script).await;
    if out.contains("Received Access-Accept") {
        Outcome::Accept(out)
    } else if out.contains("Received Access-Reject") {
        Outcome::Reject(out)
    } else if out.contains("No reply from server") {
        Outcome::NoReply(out)
    } else {
        Outcome::Other(out)
    }
}

// ---------------------------------------------------------------------------
// One RADIUS server under test
// ---------------------------------------------------------------------------

struct Case {
    env: Env,
    name: String,
    users: Users,
    nas_pod: String,
    nas_ip: String,
    nas_secret: String,
    started: std::time::Instant,
    crs: Api<KanidmRadiusServer>,
    pods: Api<Pod>,
    secrets: Api<Secret>,
    deployments: Api<Deployment>,
}

fn is_ready(cr: Option<&KanidmRadiusServer>) -> bool {
    cr.and_then(|cr| cr.status.as_ref())
        .is_some_and(|s| s.ready && s.token_id.is_some() && s.endpoint.is_some())
}

fn has_condition(
    type_: &'static str,
    status: &'static str,
    reason: Option<&'static str>,
) -> impl Fn(Option<&KanidmRadiusServer>) -> bool {
    move |cr| {
        cr.and_then(|cr| cr.status.as_ref())
            .and_then(|s| s.conditions.as_ref())
            .is_some_and(|conditions| {
                conditions.iter().any(|c| {
                    c.type_ == type_
                        && c.status == status
                        && reason.is_none_or(|reason| c.reason == reason)
                })
            })
    }
}

async fn create_nas_pod(pods: &Api<Pod>, name: &str) -> String {
    pods.create(&PostParams::default(), &client_pod(name, None))
        .await
        .unwrap();
    wait_for(pods.clone(), name, |p: Option<&Pod>| {
        p.is_some_and(is_pod_ready)
    })
    .await;
    pods.get(name)
        .await
        .unwrap()
        .status
        .unwrap()
        .pod_ip
        .unwrap()
}

async fn create_input_secrets(secrets: &Api<Secret>, name: &str, nas_secret: &str) {
    let nas: Secret = serde_json::from_value(json!({
        "metadata": {"name": format!("{name}-nas"), "labels": e2e_labels()},
        "stringData": {NAS_SECRET_KEY: nas_secret},
    }))
    .unwrap();
    secrets.create(&PostParams::default(), &nas).await.unwrap();
    let eap: Secret = serde_json::from_value(json!({
        "metadata": {"name": format!("{name}-eap"), "labels": e2e_labels()},
        "type": "kubernetes.io/tls",
        "stringData": {
            "tls.crt": std::str::from_utf8(CERT).unwrap(),
            "tls.key": std::str::from_utf8(KEY).unwrap(),
        },
    }))
    .unwrap();
    secrets.create(&PostParams::default(), &eap).await.unwrap();
}

fn radius_spec(
    name: &str,
    replicas: i32,
    allowed_spn: &str,
    nas_ip: &str,
    kanidm_namespace: Option<&str>,
) -> serde_json::Value {
    let mut kanidm_ref = json!({"name": KANIDM_NAME});
    if let Some(namespace) = kanidm_namespace {
        kanidm_ref["namespace"] = json!(namespace);
    }
    json!({
        "kanidmRef": kanidm_ref,
        "replicas": replicas,
        "authorization": {
            "requiredGroups": [allowed_spn],
            "defaultVlan": 10,
            "groupVlans": [{"group": allowed_spn, "vlan": 20}],
        },
        "clients": [{
            "name": "nas",
            "ipaddr": format!("{nas_ip}/32"),
            "sharedSecretRef": {"name": format!("{name}-nas"), "key": NAS_SECRET_KEY},
        }],
        "radiusTls": {"secretRef": {"name": format!("{name}-eap")}},
        "kanidmTls": {"caBundleRef": {"kind": "ConfigMap", "name": CA_CONFIG_MAP, "key": "ca.crt"}},
    })
}

impl Case {
    /// Everything but the RADIUS CR: identities, input Secrets, NAS pod.
    async fn prepare(name: &str) -> Case {
        let started = std::time::Instant::now();
        let env = env().await;
        let users = create_users(&env.idm, name).await;
        let pods = Api::<Pod>::namespaced(env.client.clone(), "default");
        let secrets = Api::<Secret>::namespaced(env.client.clone(), "default");
        create_input_secrets(&secrets, name, NAS_SECRET).await;
        let nas_pod = format!("{name}-nas");
        let nas_ip = create_nas_pod(&pods, &nas_pod).await;
        Case {
            crs: Api::namespaced(env.client.clone(), "default"),
            deployments: Api::namespaced(env.client.clone(), "default"),
            env,
            name: name.to_string(),
            users,
            nas_pod,
            nas_ip,
            nas_secret: NAS_SECRET.to_string(),
            started,
            pods,
            secrets,
        }
    }

    async fn create_cr(&self, replicas: i32) {
        let spec = radius_spec(
            &self.name,
            replicas,
            &self.users.allowed_spn,
            &self.nas_ip,
            None,
        );
        let cr = KanidmRadiusServer::new(&self.name, serde_json::from_value(spec).unwrap());
        self.crs.create(&PostParams::default(), &cr).await.unwrap();
    }

    /// Prepare, create the CR, wait for Ready and a first Access-Accept.
    async fn start(name: &str, replicas: i32) -> Case {
        let case = Case::prepare(name).await;
        case.create_cr(replicas).await;
        case.wait_ready().await;
        case.wait_accept().await;
        case
    }

    async fn wait_ready(&self) {
        wait_for(self.crs.clone(), &self.name, is_ready).await;
    }

    async fn cr(&self) -> KanidmRadiusServer {
        self.crs.get(&self.name).await.unwrap()
    }

    async fn token_id(&self) -> String {
        self.cr().await.status.unwrap().token_id.unwrap()
    }

    async fn config_secret(&self) -> String {
        self.cr().await.status.unwrap().config_secret_name.unwrap()
    }

    fn account(cr: &KanidmRadiusServer) -> String {
        let uid = uuid::Uuid::parse_str(&cr.uid().unwrap()).unwrap();
        format!("kaniop_radius_{}", uid.simple())
    }

    async fn account_name(&self) -> String {
        Self::account(&self.cr().await)
    }

    async fn tokens(&self) -> Vec<String> {
        let account = self.account_name().await;
        let mut ids: Vec<String> = self
            .env
            .idm
            .idm_service_account_list_api_token(&account)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|t| t.token_id.to_string())
            .collect();
        ids.sort();
        ids
    }

    async fn server(&self) -> String {
        self.cr().await.status.unwrap().endpoint.unwrap()
    }

    async fn login_as(&self, login: &Login<'_>, retries: u32) -> Outcome {
        radius_login(
            &self.pods,
            &self.nas_pod,
            &self.server().await,
            login,
            retries,
        )
        .await
    }

    fn allowed_login(&self, method: Method) -> Login<'_> {
        Login {
            user: &self.users.allowed,
            password: &self.users.allowed_secret,
            nas_secret: &self.nas_secret,
            method,
        }
    }

    async fn wait_accept(&self) {
        let login = self.allowed_login(Method::Mschap);
        poll_until_with_timeout("RADIUS Access-Accept", STEP_WAIT, || async {
            self.login_as(&login, 1).await.is_accept().then_some(())
        })
        .await;
    }

    async fn nudge(&self) {
        self.crs
            .patch(
                &self.name,
                &PatchParams::default(),
                &Patch::Merge(
                    json!({"metadata": {"annotations": {"e2e.kaniop.rs/nudge": jiff_now()}}}),
                ),
            )
            .await
            .unwrap();
    }

    async fn patch_secret(&self, secret: &str, patch: serde_json::Value) {
        self.secrets
            .patch(secret, &PatchParams::default(), &Patch::Merge(patch))
            .await
            .unwrap();
    }

    /// Names of RADIUS pods of this CR, optionally only the ready ones.
    async fn radius_pods(&self, only_ready: bool) -> BTreeSet<String> {
        let uid = self.cr().await.uid().unwrap();
        self.pods
            .list(&ListParams::default().labels(&format!("{RADIUS_UID_LABEL}={uid}")))
            .await
            .unwrap()
            .items
            .into_iter()
            .filter(|p| !only_ready || (is_pod_ready(p) && p.metadata.deletion_timestamp.is_none()))
            .map(|p| p.name_any())
            .collect()
    }

    /// Wait until all ready pods are new and none of `old` is left.
    async fn wait_rolled(&self, old: &BTreeSet<String>, replicas: usize) {
        poll_until_with_timeout("pods rolled", STEP_WAIT, || async {
            let all = self.radius_pods(false).await;
            let ready = self.radius_pods(true).await;
            (ready.len() == replicas && all.is_disjoint(old)).then_some(())
        })
        .await;
    }

    async fn template_annotation(&self, key: &str) -> Option<String> {
        self.deployments
            .get(&self.name)
            .await
            .ok()?
            .spec?
            .template
            .metadata?
            .annotations?
            .get(key)
            .cloned()
    }

    async fn wait_tokens(&self, expected: Vec<String>, timeout: Duration) {
        let mut expected = expected;
        expected.sort();
        poll_until_with_timeout("Kanidm tokens converge", timeout, || async {
            (self.tokens().await == expected).then_some(())
        })
        .await;
    }

    /// Delete the CR and wait for the finalizer; failures here are reported
    /// but do not fail the test (the assertions come before).
    async fn cleanup(self) {
        let uid = self.cr().await.uid().unwrap();
        let _ = self.crs.delete(&self.name, &DeleteParams::default()).await;
        let _ = super::wait_for_result(self.crs.clone(), &self.name, conditions::is_deleted(&uid))
            .await;
        self.delete_inputs().await;
    }

    /// Remove the NAS pod and the input Secrets, so a test can be re-run in
    /// the same cluster.
    async fn delete_inputs(&self) {
        delete_pod_now(&self.pods, &self.nas_pod).await;
        for secret in [format!("{}-nas", self.name), format!("{}-eap", self.name)] {
            let _ = self.secrets.delete(&secret, &DeleteParams::default()).await;
        }
    }

    /// Fail if a credential shows up in status, events or operator logs.
    async fn assert_no_leaks(&self) {
        let config_secret = self.config_secret().await;
        let secret = self.secrets.get(&config_secret).await.unwrap();
        let bearer =
            String::from_utf8(secret.data.unwrap().get("auth_token").unwrap().0.clone()).unwrap();
        assert!(bearer.len() > 20, "unexpectedly short bearer");
        let needles = [
            ("bearer token", bearer.as_str()),
            ("NAS shared secret", self.nas_secret.as_str()),
            ("user RADIUS secret", self.users.allowed_secret.as_str()),
        ];

        let cr = self.cr().await;
        let status = serde_json::to_string(&cr.status).unwrap();
        let events = Api::<Event>::namespaced(self.env.client.clone(), "default")
            .list(
                &ListParams::default().fields(&format!("involvedObject.uid={}", cr.uid().unwrap())),
            )
            .await
            .unwrap()
            .items;
        let events = serde_json::to_string(&events).unwrap();
        let operator_pods = Api::<Pod>::namespaced(self.env.client.clone(), "kaniop");
        let mut logs = String::new();
        for pod in operator_pods
            .list(&ListParams::default().labels("app.kubernetes.io/instance=kaniop"))
            .await
            .unwrap()
        {
            let params = LogParams {
                since_seconds: Some(self.started.elapsed().as_secs() as i64 + 120),
                ..Default::default()
            };
            logs.push_str(
                &operator_pods
                    .logs(&pod.name_any(), &params)
                    .await
                    .unwrap_or_default(),
            );
        }
        assert!(
            logs.contains(&self.name),
            "operator logs do not mention the CR; wrong logs?"
        );
        for (what, needle) in needles {
            assert!(!status.contains(needle), "{what} found in status");
            assert!(!events.contains(needle), "{what} found in events");
            assert!(!logs.contains(needle), "{what} found in operator logs");
        }
    }
}

async fn group_members(idm: &KanidmClient, group: &str) -> Vec<String> {
    idm.idm_group_get_members(group)
        .await
        .unwrap()
        .unwrap_or_default()
}

fn member_of(members: &[String], account: &str) -> bool {
    members
        .iter()
        .any(|m| m == account || m.starts_with(&format!("{account}@")))
}

async fn delete_pod_now(pods: &Api<Pod>, name: &str) {
    let _ = pods
        .delete(name, &DeleteParams::default().grace_period(0))
        .await;
}

async fn restart_operator(client: &Client) {
    let pods = Api::<Pod>::namespaced(client.clone(), "kaniop");
    let selector = ListParams::default().labels("app.kubernetes.io/component=operator");
    let old: BTreeSet<String> = pods
        .list(&selector)
        .await
        .unwrap()
        .items
        .iter()
        .map(|p| p.name_any())
        .collect();
    for name in &old {
        delete_pod_now(&pods, name).await;
    }
    poll_until_with_timeout("operator restarted", STEP_WAIT, || {
        let pods = pods.clone();
        let selector = selector.clone();
        let old = old.clone();
        async move {
            let items = pods.list(&selector).await.ok()?.items;
            items
                .iter()
                .any(|p| !old.contains(&p.name_any()) && is_pod_ready(p))
                .then_some(())
        }
    })
    .await;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

e2e_test!(radius_lifecycle_authentication, {
    let name = "radius-lifecycle";
    let case = Case::prepare(name).await;
    let idm = &case.env.idm;

    // A pre-existing, unrelated member of the shared group must survive.
    let unrelated = format!("{name}-unrelated");
    idm.idm_service_account_create(&unrelated, &unrelated, "idm_admin")
        .await
        .unwrap();
    idm.idm_group_add_members(RADIUS_GROUP, &[&unrelated])
        .await
        .unwrap();

    // A NAS the CR does not list.
    let stranger = format!("{name}-stranger");
    case.pods
        .create(&PostParams::default(), &client_pod(&stranger, None))
        .await
        .unwrap();
    wait_for(case.pods.clone(), &stranger, |p: Option<&Pod>| {
        p.is_some_and(is_pod_ready)
    })
    .await;

    case.create_cr(2).await;
    case.wait_ready().await;
    case.wait_accept().await;

    let cr = case.cr().await;
    let status = cr.status.clone().unwrap();
    assert_eq!(status.deployment_name.as_deref(), Some(name));
    assert_eq!(status.service_name.as_deref(), Some(name));
    assert_eq!(status.replicas, Some(2));
    assert_eq!(status.ready_replicas, Some(2));
    assert_eq!(status.observed_generation, cr.metadata.generation);
    assert!(
        status
            .config_secret_name
            .as_deref()
            .unwrap()
            .starts_with(&format!("{name}-"))
    );
    assert!(status.token_issued_at.is_some());
    assert!(status.endpoint.as_deref().unwrap().ends_with(":1812"));
    assert!(status.retiring_token_ids.is_empty());
    for (type_, truth) in [
        ("IdentityReady", "True"),
        ("ConfigurationReady", "True"),
        ("Available", "True"),
        ("Exposed", "True"),
        ("Rotating", "False"),
    ] {
        assert!(
            has_condition(type_, truth, None)(Some(&cr)),
            "condition {type_} should be {truth}: {:?}",
            status.conditions
        );
    }

    // Account, membership, one token.
    let account = Case::account(&cr);
    let entry = idm
        .idm_service_account_get(&account)
        .await
        .unwrap()
        .unwrap();
    assert!(entry.attrs["displayname"][0].contains(&cr.uid().unwrap()));
    let members = group_members(idm, RADIUS_GROUP).await;
    assert!(member_of(&members, &account), "{members:?}");
    assert!(member_of(&members, &unrelated), "{members:?}");
    assert_eq!(case.tokens().await, vec![status.token_id.clone().unwrap()]);

    // Live authentication: PAP and MS-CHAP accepted with the VLAN attribute.
    for method in [Method::Pap, Method::Mschap] {
        let outcome = case.login_as(&case.allowed_login(method), 2).await;
        let Outcome::Accept(out) = outcome else {
            panic!("{method:?} login not accepted: {outcome:?}");
        };
        assert!(
            out.contains("Tunnel-Private-Group-Id") && out.contains("\"20\""),
            "missing VLAN 20 in {out}"
        );
    }
    // Wrong user password and a user outside the required group are rejected.
    for (login, why) in [
        (
            Login {
                user: &case.users.allowed,
                password: "definitely-wrong",
                nas_secret: NAS_SECRET,
                method: Method::Pap,
            },
            "wrong password",
        ),
        (
            Login {
                user: &case.users.denied,
                password: &case.users.denied_secret,
                nas_secret: NAS_SECRET,
                method: Method::Pap,
            },
            "user outside required groups",
        ),
        (
            Login {
                user: &case.users.denied,
                password: &case.users.denied_secret,
                nas_secret: NAS_SECRET,
                method: Method::Mschap,
            },
            "user outside required groups (MS-CHAP)",
        ),
    ] {
        let outcome = case.login_as(&login, 2).await;
        assert!(matches!(outcome, Outcome::Reject(_)), "{why}: {outcome:?}");
    }
    // Wrong NAS secret and an unknown NAS get no reply at all.
    let wrong_secret = Login {
        nas_secret: "not-the-nas-secret",
        ..case.allowed_login(Method::Pap)
    };
    let outcome = case.login_as(&wrong_secret, 1).await;
    assert!(
        matches!(outcome, Outcome::NoReply(_)),
        "wrong NAS secret: {outcome:?}"
    );
    let outcome = radius_login(
        &case.pods,
        &stranger,
        &case.server().await,
        &case.allowed_login(Method::Pap),
        1,
    )
    .await;
    assert!(
        matches!(outcome, Outcome::NoReply(_)),
        "unknown NAS: {outcome:?}"
    );

    case.assert_no_leaks().await;

    // Clean up the unrelated member.
    idm.idm_group_remove_members(RADIUS_GROUP, &[&unrelated])
        .await
        .unwrap();
    idm.idm_service_account_delete(&unrelated).await.unwrap();
    delete_pod_now(&case.pods, &stranger).await;
    case.cleanup().await;
});

e2e_test!(radius_nas_secret_change_rolls_pods_without_new_token, {
    let case = Case::start("radius-nas-change", 1).await;
    let token = case.token_id().await;
    let old_secret = case.config_secret().await;
    let old_pods = case.radius_pods(false).await;
    let tokens = case.tokens().await;

    let new_nas_secret = "nas-shared-secret-two";
    case.patch_secret(
        "radius-nas-change-nas",
        json!({"stringData": {NAS_SECRET_KEY: new_nas_secret}}),
    )
    .await;
    case.nudge().await;

    poll_until_with_timeout("new config Secret in status", STEP_WAIT, || async {
        let status = case.cr().await.status?;
        (status.config_secret_name.as_deref() != Some(old_secret.as_str()) && status.ready)
            .then_some(())
    })
    .await;
    case.wait_rolled(&old_pods, 1).await;
    assert_eq!(
        case.token_id().await,
        token,
        "NAS secret change must not mint"
    );
    assert_eq!(case.tokens().await, tokens);

    // The new secret authenticates, the old one is no longer served.
    let new_login = Login {
        nas_secret: new_nas_secret,
        ..case.allowed_login(Method::Pap)
    };
    poll_until_with_timeout("login with new NAS secret", STEP_WAIT, || async {
        case.login_as(&new_login, 1).await.is_accept().then_some(())
    })
    .await;
    let outcome = case.login_as(&case.allowed_login(Method::Pap), 1).await;
    assert!(
        matches!(outcome, Outcome::NoReply(_)),
        "old NAS secret: {outcome:?}"
    );

    // The superseded config Secret goes once its token is past the grace.
    poll_until_with_timeout("old config Secret collected", GRACE_WAIT, || async {
        case.secrets
            .get_opt(&old_secret)
            .await
            .ok()?
            .is_none()
            .then_some(())
    })
    .await;
    assert_eq!(case.tokens().await, tokens);
    case.cleanup().await;
});

e2e_test!(radius_tls_renewal_rolls_pods_via_digest, {
    let case = Case::start("radius-tls-renewal", 1).await;
    let token = case.token_id().await;
    let config_secret = case.config_secret().await;
    let old_pods = case.radius_pods(false).await;
    let digest = case.template_annotation(INPUTS_DIGEST_ANNOTATION).await;
    assert!(digest.is_some(), "digest annotation missing");

    // A renewed certificate: same key, different bytes.
    let renewed = format!("{}\n", std::str::from_utf8(CERT).unwrap());
    case.patch_secret(
        "radius-tls-renewal-eap",
        json!({"stringData": {"tls.crt": renewed}}),
    )
    .await;
    case.nudge().await;

    poll_until_with_timeout("digest annotation changes", STEP_WAIT, || async {
        let now = case.template_annotation(INPUTS_DIGEST_ANNOTATION).await;
        (now.is_some() && now != digest).then_some(())
    })
    .await;
    case.wait_rolled(&old_pods, 1).await;
    case.wait_ready().await;
    assert_eq!(case.token_id().await, token);
    assert_eq!(case.config_secret().await, config_secret);
    case.wait_accept().await;
    case.cleanup().await;
});

e2e_test!(radius_annotation_rotation_has_no_auth_failures, {
    let case = Case::start("radius-rotation", 2).await;
    let old_token = case.token_id().await;
    let old_pods = case.radius_pods(false).await;

    // Authenticate once a second for the whole rollout.
    let stop = Arc::new(AtomicBool::new(false));
    let attempts = Arc::new(AtomicUsize::new(0));
    let failures = Arc::new(Mutex::new(Vec::<String>::new()));
    let looper = tokio::spawn({
        let (stop, attempts, failures) = (stop.clone(), attempts.clone(), failures.clone());
        let pods = case.pods.clone();
        let selector = format!("{RADIUS_UID_LABEL}={}", case.cr().await.uid().unwrap());
        let nas_pod = case.nas_pod.clone();
        let server = case.server().await;
        let user = case.users.allowed.clone();
        let password = case.users.allowed_secret.clone();
        async move {
            let login = Login {
                user: &user,
                password: &password,
                nas_secret: NAS_SECRET,
                method: Method::Mschap,
            };
            while !stop.load(Ordering::SeqCst) {
                let outcome = radius_login(&pods, &nas_pod, &server, &login, 1).await;
                attempts.fetch_add(1, Ordering::SeqCst);
                if !outcome.is_accept() {
                    // Pod states right after the failure help tell a rollout
                    // gap from a data-path problem.
                    let state: Vec<String> = pods
                        .list(&ListParams::default().labels(&selector))
                        .await
                        .map(|l| {
                            l.items
                                .iter()
                                .map(|p| {
                                    format!(
                                        "{}(ready={},terminating={},ip={:?})",
                                        p.name_any(),
                                        is_pod_ready(p),
                                        p.metadata.deletion_timestamp.is_some(),
                                        p.status.as_ref().and_then(|s| s.pod_ip.clone()),
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    failures
                        .lock()
                        .unwrap()
                        .push(format!("{}\npods: {state:?}", outcome.output()));
                }
                sleep(Duration::from_secs(1)).await;
            }
        }
    });
    sleep(Duration::from_secs(5)).await;

    case.crs
        .patch(
            &case.name,
            &PatchParams::default(),
            &Patch::Merge(json!({"metadata": {"annotations": {ROTATE_ANNOTATION: "rotation-1"}}})),
        )
        .await
        .unwrap();

    poll_until_with_timeout("tokenId changes", STEP_WAIT, || async {
        let status = case.cr().await.status?;
        (status.token_id.as_deref() != Some(old_token.as_str())).then_some(())
    })
    .await;
    let new_token = case.token_id().await;
    case.wait_rolled(&old_pods, 2).await;
    sleep(Duration::from_secs(5)).await;
    stop.store(true, Ordering::SeqCst);
    looper.await.unwrap();
    let failures = failures.lock().unwrap().clone();
    let attempts = attempts.load(Ordering::SeqCst);
    assert!(attempts >= 10, "loop ran only {attempts} times");
    assert!(
        failures.is_empty(),
        "{} of {attempts} logins failed during rotation: {failures:#?}",
        failures.len()
    );

    // Both tokens exist until the old one is unused and past the grace.
    let mut both = vec![old_token.clone(), new_token.clone()];
    both.sort();
    assert_eq!(case.tokens().await, both);
    poll_until_with_timeout("old token listed as retiring", STEP_WAIT, || async {
        let status = case.cr().await.status?;
        status.retiring_token_ids.contains(&old_token).then_some(())
    })
    .await;
    case.wait_tokens(vec![new_token.clone()], GRACE_WAIT).await;
    assert_eq!(case.token_id().await, new_token);
    case.wait_accept().await;
    case.cleanup().await;
});

e2e_test!(radius_operator_restart_mid_rotation_converges, {
    let case = Case::start("radius-restart-rotation", 1).await;
    let old_token = case.token_id().await;
    let old_pods = case.radius_pods(false).await;

    case.crs
        .patch(
            &case.name,
            &PatchParams::default(),
            &Patch::Merge(json!({"metadata": {"annotations": {ROTATE_ANNOTATION: "rotation-1"}}})),
        )
        .await
        .unwrap();
    restart_operator(&case.env.client).await;

    poll_until_with_timeout("tokenId changes", STEP_WAIT, || async {
        let status = case.cr().await.status?;
        (status.token_id.as_deref() != Some(old_token.as_str())).then_some(())
    })
    .await;
    let new_token = case.token_id().await;
    case.wait_rolled(&old_pods, 1).await;
    case.wait_tokens(vec![new_token.clone()], GRACE_WAIT).await;
    assert_eq!(case.token_id().await, new_token);
    case.wait_accept().await;
    case.cleanup().await;
});

e2e_test!(radius_broken_input_keeps_running_deployment, {
    let case = Case::start("radius-broken-input", 1).await;
    let token = case.token_id().await;
    let config_secret = case.config_secret().await;
    let pods = case.radius_pods(false).await;
    let tokens = case.tokens().await;

    case.patch_secret(
        "radius-broken-input-nas",
        json!({"data": {NAS_SECRET_KEY: null}}),
    )
    .await;
    case.nudge().await;
    wait_for(
        case.crs.clone(),
        &case.name,
        has_condition("ConfigurationReady", "False", Some("NasSecretKeyInvalid")),
    )
    .await;

    // Deployment, token and authentication are untouched.
    let cr = case.cr().await;
    assert!(has_condition("Available", "True", None)(Some(&cr)));
    assert_eq!(
        cr.status.as_ref().unwrap().token_id.as_deref(),
        Some(token.as_str())
    );
    assert_eq!(case.config_secret().await, config_secret);
    assert_eq!(case.radius_pods(false).await, pods);
    assert_eq!(case.tokens().await, tokens);
    case.wait_accept().await;

    // Restoring the key recovers without any change to the workload.
    case.patch_secret(
        "radius-broken-input-nas",
        json!({"stringData": {NAS_SECRET_KEY: NAS_SECRET}}),
    )
    .await;
    case.nudge().await;
    wait_for(
        case.crs.clone(),
        &case.name,
        has_condition("ConfigurationReady", "True", None),
    )
    .await;
    assert_eq!(case.config_secret().await, config_secret);
    assert_eq!(case.radius_pods(false).await, pods);
    assert_eq!(case.tokens().await, tokens);
    case.cleanup().await;
});

e2e_test!(radius_status_wiped_does_not_mint_new_token, {
    let case = Case::start("radius-status-wiped", 1).await;
    let token = case.token_id().await;
    let config_secret = case.config_secret().await;
    let tokens = case.tokens().await;

    case.crs
        .patch_status(
            &case.name,
            &PatchParams::default(),
            &Patch::Merge(json!({"status": null})),
        )
        .await
        .unwrap();
    case.nudge().await;

    // Status is rebuilt from Kanidm and Kubernetes, with the same token.
    poll_until_with_timeout("status rebuilt", STEP_WAIT, || async {
        let status = case.cr().await.status?;
        (status.ready && status.token_id.is_some()).then_some(())
    })
    .await;
    assert_eq!(case.token_id().await, token);
    assert_eq!(case.config_secret().await, config_secret);
    assert_eq!(case.tokens().await, tokens);
    case.wait_accept().await;
    case.cleanup().await;
});

e2e_test!(
    radius_deleted_config_secret_mints_and_revokes_after_drain,
    {
        let case = Case::start("radius-config-deleted", 1).await;
        let old_token = case.token_id().await;
        let old_secret = case.config_secret().await;
        let old_pods = case.radius_pods(false).await;

        case.secrets
            .delete(&old_secret, &DeleteParams::default())
            .await
            .unwrap();
        case.nudge().await;

        poll_until_with_timeout("new token minted", STEP_WAIT, || async {
            let status = case.cr().await.status?;
            (status.token_id.as_deref() != Some(old_token.as_str())
                && status.config_secret_name.as_deref() != Some(old_secret.as_str()))
            .then_some(())
        })
        .await;
        let new_token = case.token_id().await;
        case.wait_rolled(&old_pods, 1).await;
        case.wait_accept().await;
        // The old token is revoked once its pods are gone and the grace passed.
        case.wait_tokens(vec![new_token.clone()], GRACE_WAIT).await;
        assert_eq!(case.token_id().await, new_token);
        case.wait_accept().await;
        case.cleanup().await;
    }
);

e2e_test!(radius_too_many_tokens_clears_after_grace, {
    let case = Case::start("radius-too-many-tokens", 1).await;
    let old_token = case.token_id().await;
    let account = case.account_name().await;

    // Three stray tokens bring the account to the bound of four.
    for i in 0..3 {
        case.env
            .idm
            .idm_service_account_generate_api_token(
                &account,
                &format!("e2e-stray-{i}"),
                None,
                false,
                true,
            )
            .await
            .unwrap();
    }
    assert_eq!(case.tokens().await.len(), 4);

    case.crs
        .patch(
            &case.name,
            &PatchParams::default(),
            &Patch::Merge(json!({"metadata": {"annotations": {ROTATE_ANNOTATION: "rotation-1"}}})),
        )
        .await
        .unwrap();
    wait_for(
        case.crs.clone(),
        &case.name,
        has_condition("Rotating", "True", Some("TooManyTokens")),
    )
    .await;
    assert_eq!(case.token_id().await, old_token);
    assert_eq!(
        case.tokens().await.len(),
        4,
        "a blocked mint must not add tokens"
    );
    case.wait_accept().await;

    // The strays are unused orphans: revoked after the grace, then the
    // requested rotation goes through.
    poll_until_with_timeout("rotation after strays are revoked", GRACE_WAIT, || async {
        let status = case.cr().await.status?;
        (status.token_id.as_deref() != Some(old_token.as_str())).then_some(())
    })
    .await;
    let new_token = case.token_id().await;
    case.wait_tokens(vec![new_token.clone()], GRACE_WAIT).await;
    case.wait_accept().await;
    case.cleanup().await;
});

e2e_test!(radius_two_servers_delete_one_keeps_other_membership, {
    let first = Case::start("radius-pair-a", 1).await;
    let second = Case::start("radius-pair-b", 1).await;
    let idm = &second.env.idm;

    let unrelated = "radius-pair-unrelated";
    idm.idm_service_account_create(unrelated, unrelated, "idm_admin")
        .await
        .unwrap();
    idm.idm_group_add_members(RADIUS_GROUP, &[unrelated])
        .await
        .unwrap();

    let first_account = first.account_name().await;
    let second_account = second.account_name().await;
    let members = group_members(idm, RADIUS_GROUP).await;
    assert!(member_of(&members, &first_account) && member_of(&members, &second_account));

    first.cleanup().await;
    poll_until_with_timeout("first account removed", STEP_WAIT, || async {
        idm.idm_service_account_get(&first_account)
            .await
            .ok()?
            .is_none()
            .then_some(())
    })
    .await;

    let members = group_members(idm, RADIUS_GROUP).await;
    assert!(!member_of(&members, &first_account), "{members:?}");
    assert!(member_of(&members, &second_account), "{members:?}");
    assert!(member_of(&members, unrelated), "{members:?}");
    assert_eq!(second.tokens().await.len(), 1);
    second.wait_accept().await;

    idm.idm_group_remove_members(RADIUS_GROUP, &[unrelated])
        .await
        .unwrap();
    idm.idm_service_account_delete(unrelated).await.unwrap();
    second.cleanup().await;
});

e2e_test!(radius_deletion_waits_for_drain_and_cleans_up, {
    let case = Case::start("radius-deletion", 2).await;
    let cr = case.cr().await;
    let uid = cr.uid().unwrap();
    let account = Case::account(&cr);
    let config_secret = case.config_secret().await;
    assert_eq!(case.tokens().await.len(), 1);

    // A pod that is not ours but mounts our config Secret holds the bearer:
    // the account (and its token) must stay until it is gone.
    let stray = "radius-deletion-stray";
    case.pods
        .create(
            &PostParams::default(),
            &client_pod(stray, Some(&config_secret)),
        )
        .await
        .unwrap();
    wait_for(case.pods.clone(), stray, |p: Option<&Pod>| {
        p.is_some_and(is_pod_ready)
    })
    .await;

    case.crs
        .delete(&case.name, &DeleteParams::default())
        .await
        .unwrap();
    poll_until_with_timeout("RADIUS pods drained", STEP_WAIT, || async {
        case.radius_pods(false).await.is_empty().then_some(())
    })
    .await;
    // Stays blocked on the stray pod across many drain retries (5 s each).
    sleep(Duration::from_secs(25)).await;
    let cr = case.cr().await;
    assert!(cr.metadata.deletion_timestamp.is_some());
    assert!(
        case.env
            .idm
            .idm_service_account_get(&account)
            .await
            .unwrap()
            .is_some(),
        "account deleted while a pod still mounts the config Secret"
    );
    assert_eq!(case.tokens().await.len(), 1);

    delete_pod_now(&case.pods, stray).await;
    wait_for(case.crs.clone(), &case.name, conditions::is_deleted(&uid)).await;

    assert!(
        case.env
            .idm
            .idm_service_account_get(&account)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        case.env
            .idm
            .idm_service_account_list_api_token(&account)
            .await
            .map(|t| t.is_empty())
            .unwrap_or(true)
    );
    let members = group_members(&case.env.idm, RADIUS_GROUP).await;
    assert!(!member_of(&members, &account), "{members:?}");
    poll_until_with_timeout("generated objects removed", STEP_WAIT, || async {
        let deployment = case.deployments.get_opt(&case.name).await.ok()?;
        let service = Api::<Service>::namespaced(case.env.client.clone(), "default")
            .get_opt(&case.name)
            .await
            .ok()?;
        let secret = case.secrets.get_opt(&config_secret).await.ok()?;
        (deployment.is_none() && service.is_none() && secret.is_none()).then_some(())
    })
    .await;
    // User-managed inputs stay.
    for secret in ["radius-deletion-nas", "radius-deletion-eap"] {
        assert!(
            case.secrets.get_opt(secret).await.unwrap().is_some(),
            "{secret}"
        );
    }
    let config_maps = Api::<ConfigMap>::namespaced(case.env.client.clone(), "default");
    assert!(config_maps.get_opt(CA_CONFIG_MAP).await.unwrap().is_some());
    case.delete_inputs().await;
});

e2e_test!(radius_deletion_right_after_operator_restart, {
    let case = Case::start("radius-delete-restart", 1).await;
    let cr = case.cr().await;
    let uid = cr.uid().unwrap();
    let account = Case::account(&cr);

    restart_operator(&case.env.client).await;
    case.crs
        .delete(&case.name, &DeleteParams::default())
        .await
        .unwrap();
    wait_for(case.crs.clone(), &case.name, conditions::is_deleted(&uid)).await;
    assert!(
        case.env
            .idm
            .idm_service_account_get(&account)
            .await
            .unwrap()
            .is_none(),
        "account leaked after deletion following an operator restart"
    );
    let members = group_members(&case.env.idm, RADIUS_GROUP).await;
    assert!(!member_of(&members, &account), "{members:?}");
    case.delete_inputs().await;
});

e2e_test!(radius_unauthorized_namespace_gets_event_and_no_finalizer, {
    let env = env().await;
    let namespace = "radius-e2e-unauthorized";
    let name = "radius-unauthorized";
    let namespaces = Api::<Namespace>::all(env.client.clone());
    if namespaces.get_opt(namespace).await.unwrap().is_none() {
        let ns: Namespace =
            serde_json::from_value(json!({"metadata": {"name": namespace}})).unwrap();
        namespaces
            .create(&PostParams::default(), &ns)
            .await
            .unwrap();
    }

    // Authorization is checked before any input is read, so nothing else
    // needs to exist in the namespace.
    let spec = radius_spec(
        name,
        1,
        &format!("any@{KANIDM_DOMAIN}"),
        "192.0.2.1",
        Some("default"),
    );
    let crs = Api::<KanidmRadiusServer>::namespaced(env.client.clone(), namespace);
    let cr = crs
        .create(
            &PostParams::default(),
            &KanidmRadiusServer::new(name, serde_json::from_value(spec).unwrap()),
        )
        .await
        .unwrap();
    let uid = cr.uid().unwrap();

    let events = Api::<Event>::namespaced(env.client.clone(), namespace);
    let opts = ListParams::default().fields(&format!(
        "involvedObject.kind=KanidmRadiusServer,involvedObject.apiVersion=kaniop.rs/v1beta1,involvedObject.uid={uid}"
    ));
    check_event_with_timeout(&events, &opts).await;
    let not_watched: Vec<_> = events
        .list(&opts)
        .await
        .unwrap()
        .items
        .into_iter()
        .filter(|e| e.reason.as_deref() == Some("ResourceNotWatched"))
        .collect();
    assert!(!not_watched.is_empty());
    assert!(
        not_watched[0]
            .message
            .as_deref()
            .unwrap()
            .contains("configure `radiusServerNamespaceSelector` on the Kanidm resource")
    );

    // Not adopted: no finalizer, no generated objects, no Kanidm account.
    sleep(Duration::from_secs(5)).await;
    let cr = crs.get(name).await.unwrap();
    assert!(cr.finalizers().is_empty(), "{:?}", cr.finalizers());
    assert!(
        Api::<Deployment>::namespaced(env.client.clone(), namespace)
            .get_opt(name)
            .await
            .unwrap()
            .is_none()
    );
    let account = Case::account(&cr);
    assert!(
        env.idm
            .idm_service_account_get(&account)
            .await
            .unwrap()
            .is_none()
    );
    assert!(has_condition(
        "IdentityReady",
        "False",
        Some("NamespaceNotWatched")
    )(Some(&cr)));

    crs.delete(name, &DeleteParams::default()).await.unwrap();
    wait_for(crs.clone(), name, conditions::is_deleted(&uid)).await;
});

e2e_test!(radius_foreign_deployment_is_a_conflict_and_not_adopted, {
    let name = "radius-foreign";
    let case = Case::prepare(name).await;

    let foreign: Deployment = serde_json::from_value(json!({
        "metadata": {"name": name, "labels": e2e_labels()},
        "spec": {
            "replicas": 0,
            "selector": {"matchLabels": {"app": "radius-foreign"}},
            "template": {
                "metadata": {"labels": {"app": "radius-foreign"}},
                "spec": {"containers": [{
                    "name": "other",
                    "image": radius_image(),
                    "command": ["sleep", "infinity"],
                }]},
            },
        },
    }))
    .unwrap();
    let before = case
        .deployments
        .create(&PostParams::default(), &foreign)
        .await
        .unwrap();

    case.create_cr(1).await;
    wait_for(
        case.crs.clone(),
        name,
        has_condition("Available", "False", Some("ForeignObject")),
    )
    .await;
    // Give the controller several passes to (wrongly) adopt it.
    sleep(Duration::from_secs(10)).await;
    let after = case.deployments.get(name).await.unwrap();
    assert!(
        after.metadata.owner_references.is_none(),
        "foreign Deployment adopted"
    );
    assert_eq!(after.metadata.generation, before.metadata.generation);
    assert_eq!(after.spec, before.spec);
    assert!(!case.cr().await.status.unwrap().ready);

    // Deleting the CR must leave the foreign Deployment alone.
    let uid = case.cr().await.uid().unwrap();
    case.crs
        .delete(name, &DeleteParams::default())
        .await
        .unwrap();
    wait_for(case.crs.clone(), name, conditions::is_deleted(&uid)).await;
    let remaining = case.deployments.get_opt(name).await.unwrap();
    assert_eq!(remaining.unwrap().metadata.uid, before.metadata.uid);
    case.deployments
        .delete(name, &DeleteParams::default())
        .await
        .unwrap();
    case.delete_inputs().await;
});

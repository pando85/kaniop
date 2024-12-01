//! Publishes events for objects for kubernetes >= 1.19
use std::hash::{Hash, Hasher};
use std::{collections::HashMap, sync::Arc};

use k8s_openapi::{
    api::{
        core::v1::ObjectReference,
        events::v1::{Event as K8sEvent, EventSeries},
    },
    apimachinery::pkg::apis::meta::v1::{MicroTime, ObjectMeta},
    chrono::{Duration, Utc},
};
use kube::ResourceExt;
use kube::{
    api::{Api, Patch, PatchParams, PostParams},
    Client,
};
use tokio::sync::RwLock;

const EVENT_FINISH_TIME: Duration = Duration::minutes(6);

/// Minimal event type for publishing through [`Recorder::publish`].
///
/// All string fields must be human readable.
pub struct Event {
    /// The event severity.
    ///
    /// Shows up in `kubectl describe` as `Type`.
    pub type_: EventType,

    /// The short reason explaining why the `action` was taken.
    ///
    /// This must be at most 128 characters, generally in `PascalCase`. Shows up in `kubectl describe` as `Reason`.
    pub reason: String,

    /// A optional description of the status of the `action`.
    ///
    /// This must be at most 1kB in size. Shows up in `kubectl describe` as `Message`.
    pub note: Option<String>,

    /// The action that was taken (either successfully or unsuccessfully) against main object
    ///
    /// This must be at most 128 characters. It does not currently show up in `kubectl describe`.
    /// A common convention is a short identifier of the action that caused the outcome described in `reason`.
    /// Usually denoted in `PascalCase`.
    pub action: String,

    /// Optional secondary object related to the main object
    ///
    /// Some events are emitted for actions that affect multiple objects.
    /// `secondary` can be populated to capture this detail.
    ///
    /// For example: the event concerns a `Deployment` and it affects the current `ReplicaSet` underneath it.
    /// You would therefore populate `events` using the object reference of the `ReplicaSet`.
    ///
    /// Set `secondary` to `None`, instead, if the event affects only the object whose reference
    /// you passed to [`Recorder::new`].
    ///
    /// # Naming note
    ///
    /// `secondary` is mapped to `related` in
    /// [`Events API`](https://kubernetes.io/docs/reference/generated/kubernetes-api/v1.22/#event-v1-events-k8s-io).
    ///
    /// [`Recorder::new`]: crate::events::Recorder::new
    pub secondary: Option<ObjectReference>,
}

/// The event severity or type.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum EventType {
    /// An event took place - nothing to worry about.
    Normal,
    /// Something is not working as expected - it might be worth to have a look.
    Warning,
}

/// ObjectReference with Hash and Eq implementations
#[derive(Clone, Debug, PartialEq)]
pub struct Reference(ObjectReference);

impl Eq for Reference {}

impl Hash for Reference {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.api_version.hash(state);
        self.0.kind.hash(state);
        self.0.name.hash(state);
        self.0.namespace.hash(state);
        self.0.uid.hash(state);
    }
}

/// Isomorphic key for caching similar events
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EventKey {
    pub event_type: EventType,
    pub action: String,
    pub reason: String,
    pub reporting_controller: String,
    pub reporting_instance: Option<String>,
    pub regarding: Reference,
    pub related: Option<Reference>,
}

/// Information about the reporting controller.
///
/// ```
/// use kube::runtime::events::Reporter;
///
/// let reporter = Reporter {
///     controller: "my-awesome-controller".into(),
///     instance: std::env::var("CONTROLLER_POD_NAME").ok(),
/// };
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Reporter {
    /// The name of the reporting controller that is publishing the event.
    ///
    /// This is likely your deployment.metadata.name.
    pub controller: String,

    /// The id of the controller publishing the event. Likely your pod name.
    ///
    /// Useful when running more than one replica on your controller and you need to disambiguate
    /// where events came from.
    ///
    /// The name of the controller pod can be retrieved using Kubernetes' API or
    /// it can be injected as an environment variable using
    ///
    /// ```yaml
    /// env:
    ///   - name: CONTROLLER_POD_NAME
    ///     valueFrom:
    ///       fieldRef:
    ///         fieldPath: metadata.name
    /// ```
    ///
    /// in the manifest of your controller.
    ///
    /// Note: If `instance` is not provided, the hostname is used. If the hostname is also
    /// unavailable, `reporting_instance` defaults to `reporting_controller` in the `Event`.
    pub instance: Option<String>,
}

// simple conversions for when instance == controller
impl From<String> for Reporter {
    fn from(es: String) -> Self {
        Self {
            controller: es,
            instance: None,
        }
    }
}

impl From<&str> for Reporter {
    fn from(es: &str) -> Self {
        let instance = hostname::get().ok().and_then(|h| h.into_string().ok());
        Self {
            controller: es.into(),
            instance,
        }
    }
}

/// A publisher abstraction to emit Kubernetes' events.
///
/// All events emitted by an `Recorder` are attached to the [`ObjectReference`]
/// specified when building the recorder using [`Recorder::new`].
///
/// ```
/// use kube::runtime::events::{Reporter, Recorder, Event, EventType};
/// use k8s_openapi::api::core::v1::ObjectReference;
///
/// # async fn wrapper() -> Result<(), Box<dyn std::error::Error>> {
/// # let client: kube::Client = todo!();
/// let reporter = Reporter {
///     controller: "my-awesome-controller".into(),
///     instance: std::env::var("CONTROLLER_POD_NAME").ok(),
/// };
///
/// // references can be made manually using `ObjectMeta` and `ApiResource`/`Resource` info
/// let reference = ObjectReference {
///     // [...]
///     ..Default::default()
/// };
/// // or for k8s-openapi / kube-derive types, use Resource::object_ref:
/// // let reference = myobject.object_ref();
///
/// let recorder = Recorder::new(client, reporter, reference);
/// recorder.publish(Event {
///     action: "Scheduling".into(),
///     reason: "Pulling".into(),
///     note: Some("Pulling image `nginx`".into()),
///     type_: EventType::Normal,
///     secondary: None,
/// }).await?;
/// # Ok(())
/// # }
/// ```
///
/// Events attached to an object will be shown in the `Events` section of the output of
/// of `kubectl describe` for that object.
///
/// ## RBAC
///
/// Note that usage of the event recorder minimally requires the following RBAC rules:
///
/// ```yaml
/// - apiGroups: ["events.k8s.io"]
///   resources: ["events"]
///   verbs: ["create", "patch"]
/// ```
#[derive(Clone)]
pub struct Recorder {
    client: Client,
    reporter: Reporter,
    events_cache: Arc<RwLock<HashMap<EventKey, K8sEvent>>>,
}

impl Recorder {
    /// Create a new recorder that can publish events for one specific object
    ///
    /// This is intended to be created at the start of your controller's reconcile fn.
    ///
    /// Cluster scoped objects will publish events in the "default" namespace.
    #[must_use]
    pub fn new(client: Client, reporter: Reporter) -> Self {
        let events_cache = Arc::new(RwLock::new(HashMap::new()));
        Self {
            client,
            reporter,
            events_cache,
        }
    }

    /// Builds unique event key based on reportingController, reportingInstance, regarding, reason
    ///  and note
    fn get_event_key(&self, ev: &Event, regarding: &ObjectReference) -> EventKey {
        EventKey {
            event_type: ev.type_,
            action: ev.action.clone(),
            reason: ev.reason.clone(),
            reporting_controller: self.reporter.controller.clone(),
            reporting_instance: self.reporter.instance.clone(),
            regarding: Reference(regarding.clone()),
            related: ev.secondary.clone().map(Reference),
        }
    }

    // See https://kubernetes.io/docs/reference/generated/kubernetes-api/v1.22/#event-v1-events-k8s-io
    // for more detail on the fields
    // and what's expected: https://kubernetes.io/docs/reference/using-api/deprecation-guide/#event-v125
    fn generate_event(&self, ev: &Event, reference: &ObjectReference) -> K8sEvent {
        let now = Utc::now();
        K8sEvent {
            action: Some(ev.action.clone()),
            reason: Some(ev.reason.clone()),
            deprecated_count: None,
            deprecated_first_timestamp: None,
            deprecated_last_timestamp: None,
            deprecated_source: None,
            event_time: Some(MicroTime(now)),
            regarding: Some(reference.clone()),
            note: ev.note.clone().map(Into::into),
            metadata: ObjectMeta {
                namespace: reference.namespace.clone(),
                name: Some(format!(
                    "{}.{}",
                    reference.name.as_ref().unwrap_or(&self.reporter.controller),
                    now.timestamp()
                )),
                ..Default::default()
            },
            reporting_controller: Some(self.reporter.controller.clone()),
            reporting_instance: Some(
                self.reporter
                    .instance
                    .clone()
                    .unwrap_or_else(|| self.reporter.controller.clone()),
            ),
            series: None,
            type_: match ev.type_ {
                EventType::Normal => Some("Normal".into()),
                EventType::Warning => Some("Warning".into()),
            },
            related: ev.secondary.clone(),
        }
    }

    /// Publish a new Kubernetes' event.
    ///
    /// # Access control
    ///
    /// The event object is created in the same namespace of the [`ObjectReference`].
    /// Make sure that your controller has `create` permissions in the required namespaces
    /// for the `event` resource in the API group `events.k8s.io`.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`](`kube_client::Error`) if the event is rejected by Kubernetes.
    pub async fn publish(&self, ev: Event, reference: &ObjectReference) -> Result<(), kube::Error> {
        let now = Utc::now();

        let key = self.get_event_key(&ev, reference);
        let event = match self.events_cache.read().await.get(&key) {
            Some(e) => {
                let series = match &e.series {
                    Some(series) => EventSeries {
                        count: series.count + 1,
                        last_observed_time: MicroTime(now),
                    },
                    None => EventSeries {
                        count: 2,
                        last_observed_time: MicroTime(now),
                    },
                };
                let mut event = e.clone();
                event.series = Some(series);
                event
            }
            None => self.generate_event(&ev, reference),
        };

        let events = Api::namespaced(
            self.client.clone(),
            reference
                .namespace
                .as_ref()
                .unwrap_or(&"default".to_string()),
        );
        if event.series.is_some() {
            events
                .patch(
                    &event.name_any(),
                    &PatchParams::default(),
                    &Patch::Merge(&event),
                )
                .await?;
        } else {
            events.create(&PostParams::default(), &event).await?;
        };

        {
            let mut cache = self.events_cache.write().await;
            cache.insert(key, event);
            cache.clone().iter().for_each(|(k, v)| {
                // TODO: in 2024 edition change to:
                // if let Some(series) = v.series.as_ref() && series.last_observed_time.0 < now - EVENT_FINISH_TIME {
                if let Some(series) = v.series.as_ref() {
                    if series.last_observed_time.0 < now - EVENT_FINISH_TIME {
                        cache.remove(k);
                    }
                } else if let Some(event_time) = v.event_time.as_ref() {
                    if event_time.0 < now - EVENT_FINISH_TIME {
                        cache.remove(k);
                    }
                }
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use k8s_openapi::api::{
        core::v1::Service, events::v1::Event as K8sEvent, rbac::v1::ClusterRole,
    };
    use kube::{Api, Client, Resource};

    use super::{Event, EventType, Recorder};

    #[tokio::test]
    #[ignore = "needs cluster (creates an event for the default kubernetes service)"]
    async fn event_recorder_attaches_events() -> Result<(), Box<dyn std::error::Error>> {
        let client = Client::try_default().await?;

        let svcs: Api<Service> = Api::namespaced(client.clone(), "default");
        let s = svcs.get("kubernetes").await?; // always a kubernetes service in default
        let recorder = Recorder::new(client.clone(), "kube".into());
        recorder
            .publish(
                Event {
                    type_: EventType::Normal,
                    reason: "VeryCoolService".into(),
                    note: Some("Sending kubernetes to detention".into()),
                    action: "Test event - plz ignore".into(),
                    secondary: None,
                },
                &s.object_ref(&()),
            )
            .await?;
        let events: Api<K8sEvent> = Api::namespaced(client, "default");

        let event_list = events.list(&Default::default()).await?;
        let found_event = event_list
            .into_iter()
            .find(|e| std::matches!(e.reason.as_deref(), Some("VeryCoolService")))
            .unwrap();
        assert_eq!(found_event.note.unwrap(), "Sending kubernetes to detention");

        Ok(())
    }

    #[tokio::test]
    #[ignore = "needs cluster (creates an event for the default kubernetes service)"]
    async fn event_recorder_attaches_events_without_namespace(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let client = Client::try_default().await?;

        let svcs: Api<ClusterRole> = Api::all(client.clone());
        let s = svcs.get("system:basic-user").await?; // always get this default ClusterRole
        let recorder = Recorder::new(client.clone(), "kube".into());
        recorder
            .publish(
                Event {
                    type_: EventType::Normal,
                    reason: "VeryCoolServiceNoNamespace".into(),
                    note: Some("Sending kubernetes to detention without namespace".into()),
                    action: "Test event - plz ignore".into(),
                    secondary: None,
                },
                &s.object_ref(&()),
            )
            .await?;
        let events: Api<K8sEvent> = Api::namespaced(client, "kube-system");

        let event_list = events.list(&Default::default()).await?;
        let found_event = event_list
            .into_iter()
            .find(|e| std::matches!(e.reason.as_deref(), Some("VeryCoolServiceNoNamespace")))
            .unwrap();
        assert_eq!(
            found_event.note.unwrap(),
            "Sending kubernetes to detention without namespace"
        );

        Ok(())
    }
}
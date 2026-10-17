//! Test-only scaffolding shared by the backup controller status tests.
//!
//! Builds a [`Context`] over a `tower_test` mock service so a single status-writing helper can be
//! exercised without an API server, following the harness already used by
//! `kaniop_operator::kanidm::reconcile::test`.

use http::{Request, Response};
use kaniop_operator::controller::ControllerId;
use kaniop_operator::controller::State;
use kaniop_operator::controller::context::Context;
use kaniop_operator::metrics::Metrics;
use kube::client::{Body, Client};
use kube::runtime::reflector::Lookup;
use kube::runtime::reflector::store::Writer;
use opentelemetry::metrics::MeterProvider;
use opentelemetry_sdk::metrics::SdkMeterProvider;

/// Handle yielding the requests issued through the mocked client.
pub(crate) type MockHandle = tower_test::mock::Handle<Request<Body>, Response<Body>>;

/// A [`Context`] for `controller_id` whose client talks to a mock API server, the handle that
/// yields every request it receives, and the meter provider backing the metrics the context
/// carries. Keep the provider alive for as long as the context is used.
pub(crate) fn status_context<K>(
    controller_id: ControllerId,
) -> (Context<K>, MockHandle, SdkMeterProvider)
where
    K: kube::Resource + Lookup + Clone + 'static,
    <K as Lookup>::DynamicType: Default + Eq + std::hash::Hash + Clone,
{
    let provider = SdkMeterProvider::builder().build();
    let meter = provider.meter("test");
    // `State::to_context` looks the controller id up in its metrics, so it has to be registered.
    let state = State::new(
        Metrics::new(&meter, &[controller_id]),
        Writer::default().as_reader(),
        Writer::default().as_reader(),
        None,
    );
    let (mock_service, handle) = tower_test::mock::pair::<Request<Body>, Response<Body>>();
    (
        state.to_context(Client::new(mock_service, "default"), controller_id),
        handle,
        provider,
    )
}

#[cfg(all(test, feature = "e2e-test"))]
mod test {
    use std::time::Duration;

    use k8s_openapi::api::apps::v1::Deployment;
    use kaniop_operator::crd::kanidm::{Kanidm, KanidmSpec};
    use kube::api::{Api, Patch, PatchParams, PostParams};
    use kube::client::Client;
    use kube::runtime::wait::{await_condition, conditions, Condition};
    use kube::ResourceExt;
    use serde_json::json;
    use tokio::time::timeout;

    fn is_kanidm_ready() -> impl Condition<Kanidm> {
        |obj: Option<&Kanidm>| {
            if let Some(kanidm) = &obj {
                if let Some(status) = &kanidm.status {
                    if let Some(conditions) = &status.conditions {
                        return conditions.iter().any(|c| c.type_ == "Ready");
                    }
                }
            }
            false
        }
    }

    fn is_kanidm_not_ready() -> impl Condition<Kanidm> {
        |obj: Option<&Kanidm>| {
            if let Some(kanidm) = &obj {
                if let Some(status) = &kanidm.status {
                    if let Some(conditions) = &status.conditions {
                        return conditions.iter().all(|c| c.type_ != "Ready");
                    }
                }
            }
            true
        }
    }

    fn is_deployment_ready() -> impl Condition<Deployment> {
        |obj: Option<&Deployment>| {
            if let Some(deployment) = &obj {
                if let Some(status) = &deployment.status {
                    return status.replicas == status.updated_replicas
                        && status.replicas == status.ready_replicas;
                }
            }
            false
        }
    }

    async fn wait_for<R, C>(api: Api<R>, name: &str, condition: C)
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
            Duration::from_secs(30),
            await_condition(api, name, condition),
        )
        .await
        .unwrap()
        .unwrap();
    }

    async fn setup(name: &str) -> (Api<Kanidm>, Api<Deployment>) {
        let kanidm = Kanidm::new(name, KanidmSpec { replicas: 1 });

        let client = Client::try_default().await.unwrap();
        let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), "default");

        kanidm_api
            .create(&PostParams::default(), &kanidm)
            .await
            .unwrap();

        let deployment_api = Api::<Deployment>::namespaced(client.clone(), "default");
        wait_for(deployment_api.clone(), name, is_deployment_ready()).await;
        wait_for(kanidm_api.clone(), name, is_kanidm_ready()).await;
        (kanidm_api, deployment_api)
    }

    #[tokio::test]
    async fn kanidm_create() {
        let name = "test-create";
        setup(name).await;
    }

    #[tokio::test]
    async fn kanidm_delete_deployment() {
        let name = "test-delete-deployment";
        let (kanidm_api, deployment_api) = setup(name).await;

        let deploy = deployment_api.get(name).await.unwrap();
        deployment_api
            .delete(name, &Default::default())
            .await
            .unwrap();

        wait_for(
            deployment_api.clone(),
            name,
            conditions::is_deleted(&deploy.uid().unwrap()),
        )
        .await;
        wait_for(deployment_api.clone(), name, is_deployment_ready()).await;
        wait_for(kanidm_api.clone(), name, is_kanidm_ready()).await;

        let check_deploy_deleted = deployment_api.get(name).await.unwrap();

        kanidm_api.delete(name, &Default::default()).await.unwrap();

        wait_for(
            deployment_api,
            name,
            conditions::is_deleted(&check_deploy_deleted.uid().unwrap()),
        )
        .await;
    }

    #[tokio::test]
    async fn kanidm_delete_kanidm() {
        let name = "test-delete-kanidm";
        let (kanidm_api, deployment_api) = setup(name).await;

        let deploy = deployment_api.get(name).await.unwrap();
        let kanidm = kanidm_api.get(name).await.unwrap();
        kanidm_api.delete(name, &Default::default()).await.unwrap();

        wait_for(
            kanidm_api.clone(),
            name,
            conditions::is_deleted(&kanidm.uid().unwrap()),
        )
        .await;

        wait_for(
            deployment_api.clone(),
            name,
            conditions::is_deleted(&deploy.uid().unwrap()),
        )
        .await;
    }

    #[tokio::test]
    async fn kanidm_change_deployment() {
        let name = "test-change-deployment";
        let (kanidm_api, deployment_api) = setup(name).await;

        let mut deploy = deployment_api.get(name).await.unwrap();
        deploy.spec.as_mut().unwrap().replicas = Some(2);
        deploy.metadata.managed_fields = None;
        deployment_api
            .patch(
                name,
                &PatchParams::apply("e2e-test").force(),
                &Patch::Apply(&deploy),
            )
            .await
            .unwrap();

        wait_for(kanidm_api.clone(), name, is_kanidm_not_ready()).await;
        wait_for(kanidm_api.clone(), name, is_kanidm_ready()).await;

        let check_deploy_replicas = deployment_api.get(name).await.unwrap();

        assert_eq!(check_deploy_replicas.spec.unwrap().replicas.unwrap(), 1);
    }

    #[tokio::test]
    async fn kanidm_change_kanidm() {
        let name = "test-change-kanidm";
        let (kanidm_api, deployment_api) = setup(name).await;

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

        wait_for(kanidm_api.clone(), name, is_kanidm_not_ready()).await;
        wait_for(kanidm_api.clone(), name, is_kanidm_ready()).await;

        let check_deploy_replicas = deployment_api.get(name).await.unwrap();

        assert_eq!(check_deploy_replicas.spec.unwrap().replicas.unwrap(), 2);
    }

    #[tokio::test]
    async fn kanidm_deployment_already_exists() {
        let name = "test-deployment-already-exists";
        let deployment = json!({
            "apiVersion": "apps/v1",
            "kind": "Deployment",
            "metadata": {
                "name": name
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
                                "image": "inanimate/kanidm-server:latest"
                            }
                        ]
                    }
                }
            }
        });
        let deployment_api =
            Api::<Deployment>::namespaced(Client::try_default().await.unwrap(), "default");
        deployment_api
            .create(
                &PostParams::default(),
                &serde_json::from_value(deployment).unwrap(),
            )
            .await
            .unwrap();

        setup(name).await;
    }
}

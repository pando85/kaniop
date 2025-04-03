GH_ORG ?= pando85
VERSION ?= $(shell git rev-parse --short HEAD)
KUBERNETES_VERSION = 1.32
KIND_CLUSTER_NAME = chart-testing
KUBE_CONTEXT := kind-$(KIND_CLUSTER_NAME)
KANIOP_NAMESPACE := kaniop
export CARGO_TARGET_DIR ?= target-$(CARGO_TARGET)
CARGO_TARGET ?= x86_64-unknown-linux-gnu
CARGO_BUILD_PARAMS = --target=$(CARGO_TARGET)
# use cargo if same target or cross if not
CARGO += $(if $(filter $(shell uname -m)-unknown-linux-gnu,$(CARGO_TARGET)),cargo,cross)
ifeq ($(CARGO),cross)
	CARGO_BUILD_PARAMS += --target-dir $(shell pwd)/$(CARGO_TARGET_DIR)
endif
CARGO_RELEASE_PROFILE ?= release
DOCKER_IMAGE ?= ghcr.io/$(GH_ORG)/kaniop:$(VERSION)
DOCKER_BUILD_PARAMS = --build-arg "CARGO_TARGET_DIR=$(CARGO_TARGET_DIR)" \
		--build-arg "CARGO_BUILD_TARGET=$(CARGO_TARGET)" \
		--build-arg "CARGO_RELEASE_PROFILE=$(CARGO_RELEASE_PROFILE)" \
		-t $(DOCKER_IMAGE) .
E2E_LOGGING_LEVEL ?= 'info\,kaniop=debug'
# set KANIDM_DEV_YOLO=1 to avoid Kanidm client exiting silently when dev derived profile is used
HELM_PARAMS = --namespace $(KANIOP_NAMESPACE) \
		--set-string image.tag=$(VERSION) \
		--set 'env[0].name=KANIDM_DEV_YOLO' \
		--set-string 'env[0].value=1' \
		--set logging.level=$(E2E_LOGGING_LEVEL)

.DEFAULT: help
.PHONY: help
help:	## Show this help menu.
	@echo "Usage: make [TARGET ...]"
	@echo ""
	@@egrep -h "#[#]" $(MAKEFILE_LIST) | sed -e 's/\\$$//' | awk 'BEGIN {FS = "[:=].*?#[#] "}; {printf "\033[36m%-20s\033[0m %s\n", $$1, $$2}'
	@echo ""

.PHONY: crdgen
crdgen: CRD_DIR := charts/kaniop/crds
crdgen: ## Generate CRDs
	@if [ ! -d $(CRD_DIR) ]; then \
		mkdir -p $(CRD_DIR); \
	fi
	@cargo run --bin crdgen > $(CRD_DIR)/crds.yaml

.PHONY: lint
lint:	## lint code
	cargo clippy --locked --all-targets --all-features -- -D warnings
	cargo fmt -- --check

.PHONY: cross
cross:	## install cross if needed
	@if [ "$(CARGO)" != "cargo" ]; then  \
		if [ "$${CARGO_TARGET_DIR}" != "$${CARGO_TARGET_DIR#/}" ]; then  \
			echo CARGO_TARGET_DIR should be relative for cross compiling; \
			exit 1; \
		fi; \
		cargo install cross; \
	fi

.PHONY: test
test: lint cross
test:	## run tests
	$(CARGO) test  $(CARGO_BUILD_PARAMS)

.PHONY: build
build: cross
build: CARGO_BUILD_PARAMS += --bin kaniop
build:	## compile kaniop
	$(CARGO) build $(CARGO_BUILD_PARAMS)
	@if echo $(CARGO_BUILD_PARAMS) | grep -q 'release'; then \
		echo "binary is in $(CARGO_TARGET_DIR)/$(CARGO_TARGET)/$(CARGO_RELEASE_PROFILE)/kaniop"; \
	else \
		echo "binary is in $(CARGO_TARGET_DIR)/$(CARGO_TARGET)/debug/kaniop"; \
	fi

.PHONY: release
release: CARGO_BUILD_PARAMS += --locked --profile $(CARGO_RELEASE_PROFILE)
release: build
release:	## compile release binary

.PHONY: update-version
update-version: ## update version from VERSION file in all Cargo.toml manifests
	@VERSION=$$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1); \
	sed -i -E "s/^(kaniop\_.*version\s=\s)\"(.*)\"/\1\"$$VERSION\"/gm" */*/Cargo.toml && \
	sed -i -E "s/^(\s+tag:\s)(.*)/\1$$VERSION/gm" charts/kaniop/values.yaml && \
	cargo update -p kaniop_operator && \
	echo updated to version "$$VERSION" cargo and helm files

.PHONY: update-changelog
update-changelog:	## automatically update changelog based on commits
	git cliff -t v$(VERSION) -u -p CHANGELOG.md

.PHONY: publish
publish:	## publish crates
	@for package in $(shell find . -mindepth 2 -not -path './tests/e2e/*' -name Cargo.toml -exec dirname {} \; | sort -r );do \
		cd $$package; \
		cargo publish; \
		cd -; \
	done;

.PHONY: image
image: release
image:	## build image
	@$(SUDO) docker buildx build --load $(DOCKER_BUILD_PARAMS)

push-image-%:
	# force multiple release targets
	$(MAKE) CARGO_TARGET=$(CARGO_TARGET) release
	echo $(SUDO) docker buildx build --push --no-cache --platform linux/$* $(DOCKER_BUILD_PARAMS)

IMAGE_ARCHITECTURES := amd64 arm64

push-image-amd64: CARGO_TARGET=x86_64-unknown-linux-gnu
push-image-arm64: CARGO_TARGET=aarch64-unknown-linux-gnu

.PHONY: push-images
push-images: $(IMAGE_ARCHITECTURES:%=push-image-%)
push-images:	## push images for all architectures

.PHONY: integration-test
integration-test: OPENTELEMETY_ENVAR_DEFINITION := OPENTELEMETRY_ENDPOINT_URL=localhost:4317
integration-test: cross
integration-test:	## run integration tests
	@docker run -d --name tempo \
		-v $(shell pwd)/tests/integration/config/tempo.yaml:/etc/tempo.yaml \
		-p 4317:4317 \
		grafana/tempo:latest -config.file=/etc/tempo.yaml
	@if [ "$(CARGO)" = "cargo" ]; then  \
		export $(OPENTELEMETY_ENVAR_DEFINITION); \
	else \
		export CROSS_CONTAINER_OPTS="--env $(OPENTELEMETY_ENVAR_DEFINITION)"; \
	fi; \
	$(CARGO) test $(CARGO_BUILD_PARAMS) --features integration-test integration; \
		STATUS=$$?; \
		docker rm -f tempo >/dev/null 2>&1; \
		exit $$STATUS

.PHONY: e2e
e2e: image crdgen
e2e:	## prepare e2e tests environment
	@if kind get clusters | grep -q $(KIND_CLUSTER_NAME); then \
		echo "e2e environment already running"; \
		exit 0; \
	fi; \
	kind create cluster --name $(KIND_CLUSTER_NAME) --config .github/kind-cluster-$(KUBERNETES_VERSION).yaml; \
	kind load --name $(KIND_CLUSTER_NAME) docker-image $(DOCKER_IMAGE); \
	if [ "$$(kubectl config current-context)" != "$(KUBE_CONTEXT)" ]; then \
		echo "ERROR: switch to kind context: kubectl config use-context $(KUBE_CONTEXT)"; \
		exit 1; \
	fi; \
	kubectl apply -f https://kind.sigs.k8s.io/examples/ingress/deploy-ingress-nginx.yaml; \
	kubectl create namespace $(KANIOP_NAMESPACE); \
	helm install kaniop ./charts/kaniop $(HELM_PARAMS); \
	ITERATION=1; \
	while [ $$ITERATION -le 20 ]; do \
		if kubectl -n $(KANIOP_NAMESPACE) get deploy $(KANIOP_NAMESPACE) | grep -q '1/1'; then \
			echo "Kaniop deployment is ready"; \
			break; \
		else \
			echo "Retrying in 5 seconds..."; \
			sleep 5; \
		fi; \
		ITERATION=$$((ITERATION + 1)); \
	done; \
	kubectl wait --namespace ingress-nginx \
		--for=condition=ready pod \
		--selector=app.kubernetes.io/component=controller \
		--timeout=90s

.PHONY: e2e-test
e2e-test: e2e
e2e-test: export KANIDM_DEV_YOLO=1 # avoid Kanidm client exiting silently
e2e-test:	## run end to end tests
	@if [ "$$(kubectl config current-context)" != "$(KUBE_CONTEXT)" ]; then \
		echo "ERROR: switch to kind context: kubectl config use-context $(KUBE_CONTEXT)"; \
		exit 1; \
	fi
	kubectl get -A pods -o wide
	kubectl -n $(KANIOP_NAMESPACE) describe pod -l app.kubernetes.io/instance=kaniop
	cargo test $(CARGO_BUILD_PARAMS) -p tests --features e2e-test || \
		(kubectl -n $(KANIOP_NAMESPACE) logs -l app.kubernetes.io/instance=kaniop && exit 2)

.PHONY: clean-e2e
clean-e2e:	## clean end to end environment: delete all created resources in kind
	@if [ "$$(kubectl config current-context)" != "$(KUBE_CONTEXT)" ]; then \
		echo "switch to the kind context only if deletion is necessary: kubectl config use-context $(KUBE_CONTEXT)"; \
		exit 0; \
	fi; \
	for resource in kanidmgroup person oauth2 kanidm secrets pvc statefulset; do \
		kubectl -n default delete $$resource --all --timeout=2s; \
		kubectl -n default get $$resource -o name | \
			xargs -I{} kubectl -n default patch {} -p '{"metadata":{"finalizers":[]}}' --type=merge; \
	done; \
	kubectl -n kaniop delete oauth2 --all --timeout=2s; \
	kubectl -n kaniop get oauth2 -o name | \
		xargs -I{} kubectl -n kaniop patch {} -p '{"metadata":{"finalizers":[]}}' --type=merge; \
	kubectl -n kaniop delete oauth2 --all

.PHONY: update-e2e-kaniop
update-e2e-kaniop: image crdgen
update-e2e-kaniop: ## update kaniop deployment in end to end tests with current code
	if [ "$$(kubectl config current-context)" != "$(KUBE_CONTEXT)" ]; then \
		echo "ERROR: switch to kind context: kubectl config use-context $(KUBE_CONTEXT)"; \
		exit 1; \
	fi; \
	kind load --name $(KIND_CLUSTER_NAME) docker-image $(DOCKER_IMAGE); \
	helm upgrade kaniop ./charts/kaniop $(HELM_PARAMS); \
	kubectl -n $(KANIOP_NAMESPACE) rollout restart deploy $(KANIOP_NAMESPACE)

.PHONY: delete-kind
delete-kind:	## delete kind K8s cluster. It will delete e2e environment.
	kind delete cluster --name $(KIND_CLUSTER_NAME)

.PHONY: examples
examples:
examples: ## generate examples
	@cargo run --bin examples-gen

GH_ORG ?= pando85
VERSION ?= $(shell git rev-parse --short HEAD)
PROJECT_VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
# renovate: datasource=docker depName=kindest/node
KIND_IMAGE_TAG ?= v1.33.4
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
DOCKER_BASE_IMAGE_NAME ?= kaniop
DOCKER_IMAGE_NAME ?= ghcr.io/$(GH_ORG)/$(DOCKER_BASE_IMAGE_NAME)
WEBHOOK_DOCKER_IMAGE_NAME ?= $(DOCKER_IMAGE_NAME)-webhook
DOCKER_IMAGE ?= $(DOCKER_IMAGE_NAME):$(VERSION)
WEBHOOK_DOCKER_IMAGE ?= $(WEBHOOK_DOCKER_IMAGE_NAME):$(VERSION)
TMP_DIR ?= /tmp
DOCKER_METADATA_FILE_BASE ?= $(TMP_DIR)/$(DOCKER_BASE_IMAGE_NAME)-$(VERSION)
DOCKER_BUILD_PARAMS = --build-arg "CARGO_TARGET_DIR=$(CARGO_TARGET_DIR)" \
		--build-arg "CARGO_BUILD_TARGET=$(CARGO_TARGET)" \
		--build-arg "CARGO_RELEASE_PROFILE=$(CARGO_RELEASE_PROFILE)"
E2E_LOGGING_LEVEL ?= 'info\,kaniop=debug\,kaniop_webhook=debug'
# set KANIDM_DEV_YOLO=1 to avoid Kanidm client exiting silently when dev derived profile is used
HELM_PARAMS = --namespace $(KANIOP_NAMESPACE) \
		--set-string image.tag=$(VERSION) \
		--set 'env[0].name=KANIDM_DEV_YOLO' \
		--set-string 'env[0].value=1' \
		--set logging.level=$(E2E_LOGGING_LEVEL) \
		--set webhook.enabled=true \
		--set-string webhook.image.tag=$(VERSION) \
		--set webhook.logging.level=$(E2E_LOGGING_LEVEL)

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
build: CARGO_BUILD_PARAMS += --bin kaniop --bin kaniop-webhook
build:	## compile kaniop and kaniop-webhook
	$(CARGO) build $(CARGO_BUILD_PARAMS)
	@if echo $(CARGO_BUILD_PARAMS) | grep -q 'release'; then \
		echo "binaries are in $(CARGO_TARGET_DIR)/$(CARGO_TARGET)/$(CARGO_RELEASE_PROFILE)/"; \
	else \
		echo "binaries are in $(CARGO_TARGET_DIR)/$(CARGO_TARGET)/debug/"; \
	fi

.PHONY: release
release: CARGO_BUILD_PARAMS += --locked --profile $(CARGO_RELEASE_PROFILE)
release: build
release:	## compile release binary

.PHONY: update-version
update-version: ## update version from VERSION file in all Cargo.toml manifests
update-version: */Cargo.toml
	@VERSION=$$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1); \
	sed -i -E "s/(kaniop-[a-z0-9-]+ = \{ path = \"[^\"]+\", version = )\"[^\"]+\"/\1\"$$VERSION\"/g" Cargo.toml && \
	cargo update --workspace ; \
	echo updated to version "$$VERSION" cargo files ; \
	if [ -z "$$VERSION" ]; then \
	  VERSION=$$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1); \
	fi; \
	echo "-> Using version $$VERSION"; \
	sed -i -E "s/^(version: ).*/\1\"$$VERSION\"/" charts/kaniop/Chart.yaml; \
	sed -i -E "s/^(appVersion: ).*/\1\"$$VERSION\"/" charts/kaniop/Chart.yaml; \
	sed -i -E "s|(      image: ghcr.io/pando85/kaniop:).*|\1$$VERSION|" charts/kaniop/Chart.yaml; \
	sed -i -E "s|(      image: ghcr.io/pando85/kaniop-webhook:).*|\1$$VERSION|" charts/kaniop/Chart.yaml; \
	if echo "$$VERSION" | grep -q '-' ; then FLAG=true; else FLAG=false; fi; \
	sed -i -E "s|(artifacthub.io/prerelease: ).*|\1\"$$FLAG\"|" charts/kaniop/Chart.yaml; \
	if [ -f charts/kaniop/crds/crds.yaml ]; then \
		echo "Updating CRD versions in Chart.yaml..."; \
		CRD_INFO=$$(awk '/^kind: CustomResourceDefinition/,/^---/{if(/^  name:/) crd_name=$$2; if(/^    kind:/) kind=$$2; if(/^    name: v[0-9]/) {version=$$2; print kind ";" version ";" crd_name}}' charts/kaniop/crds/crds.yaml); \
		for info in $$CRD_INFO; do \
			KIND=$$(echo $$info | cut -d';' -f1); \
			VERSION=$$(echo $$info | cut -d';' -f2); \
			NAME=$$(echo $$info | cut -d';' -f3); \
			sed -i -E "/- kind: $$KIND$$/,/- kind:/ s|(      version: ).*|\1$$VERSION|" charts/kaniop/Chart.yaml; \
		done; \
		echo "Updated CRD versions in artifacthub.io/crds annotation"; \
	fi; \
	cargo update -p kaniop_operator >/dev/null 2>&1 || true; \
	echo "Updating chart changes from git-cliff..."; \
	LAST_TAG=$$(git describe --tags --abbrev=0 2>/dev/null || echo ""); \
	if [ -n "$$LAST_TAG" ]; then \
		CHANGES=$$(git-cliff --config .ci/cliff-chart.toml -t v$(PROJECT_VERSION) --strip all $$LAST_TAG..HEAD 2>/dev/null || echo "    - No changes in the chart for this Kaniop version."); \
	else \
		CHANGES=$$(git-cliff --config .ci/cliff-chart.toml -t v$(PROJECT_VERSION) --strip all 2>/dev/null || echo "    - No changes in the chart for this Kaniop version."); \
	fi; \
	TMP_FILE=$$(mktemp); \
	awk -v changes="$$CHANGES" ' \
		/artifacthub.io\/changes:/ { print; print changes; in_changes=1; next } \
		in_changes && /^  [a-z]/ { in_changes=0 } \
		!in_changes { print } \
	' charts/kaniop/Chart.yaml > "$$TMP_FILE" && mv "$$TMP_FILE" charts/kaniop/Chart.yaml || echo "Warning: Could not update chart changes"; \
	echo "Updated: Cargo crates, values.yaml, Chart.yaml (version/appVersion/image/prerelease=$$FLAG/changes)"; \
	grep -E '^(version:|appVersion:)' charts/kaniop/Chart.yaml

.PHONY: update-changelog
update-changelog:	## automatically update changelog based on commits
	git cliff -t v$(PROJECT_VERSION) -u -p CHANGELOG.md

.PHONY: publish
publish:	## publish crates
	cargo publish --workspace --exclude kaniop-e2e-tests

IMAGE_ARCHITECTURES := amd64 arm64
IMAGE_COMPONENTS := kaniop kaniop-webhook

# Build local images for each component
image-kaniop:
	@$(SUDO) docker build --load $(DOCKER_BUILD_PARAMS) --target kaniop -t $(DOCKER_IMAGE) .

image-kaniop-webhook:
	@$(SUDO) docker build --load $(DOCKER_BUILD_PARAMS) --target kaniop-webhook -t $(WEBHOOK_DOCKER_IMAGE) .

.PHONY: images
images: release $(IMAGE_COMPONENTS:%=image-%)
images:	## build image

# Push images for specific architecture and component
push-image-%-kaniop:
	# force multiple release targets
	@$(MAKE) CARGO_TARGET=$(CARGO_TARGET) release
	@$(SUDO) docker buildx build \
		-o type=image,push-by-digest=true,name-canonical=true,push=true \
		--metadata-file $(DOCKER_METADATA_FILE_BASE)-$*-kaniop.json \
		--no-cache --platform linux/$* --target kaniop $(DOCKER_BUILD_PARAMS) -t $(DOCKER_IMAGE_NAME) .

push-image-%-kaniop-webhook:
	# force multiple release targets
	@$(MAKE) CARGO_TARGET=$(CARGO_TARGET) release
	@$(SUDO) docker buildx build \
		-o type=image,push-by-digest=true,name-canonical=true,push=true \
		--metadata-file $(DOCKER_METADATA_FILE_BASE)-$*-webhook.json \
		--no-cache --platform linux/$* --target kaniop-webhook $(DOCKER_BUILD_PARAMS) -t $(WEBHOOK_DOCKER_IMAGE_NAME) .

# Generate all combinations of architecture and component targets
push-image-amd64-kaniop push-image-amd64-kaniop-webhook: CARGO_TARGET=x86_64-unknown-linux-gnu
push-image-arm64-kaniop push-image-arm64-kaniop-webhook: CARGO_TARGET=aarch64-unknown-linux-gnu

# Force release build before pushing any image
$(foreach arch,$(IMAGE_ARCHITECTURES),$(foreach comp,$(IMAGE_COMPONENTS),push-image-$(arch)-$(comp))): release

.PHONY: push-images
push-images: $(foreach arch,$(IMAGE_ARCHITECTURES),$(foreach comp,$(IMAGE_COMPONENTS),push-image-$(arch)-$(comp)))
push-images: IMAGE_DIGESTS = $(shell jq -r '"$(DOCKER_IMAGE_NAME)@" +.["containerimage.digest"]' $(DOCKER_METADATA_FILE_BASE)-*-kaniop.json | xargs)
push-images: WEBHOOK_IMAGE_DIGESTS = $(shell jq -r '"$(WEBHOOK_DOCKER_IMAGE_NAME)@" +.["containerimage.digest"]' $(DOCKER_METADATA_FILE_BASE)-*-webhook.json | xargs)
push-images:	## push images for all architectures
	@$(SUDO) docker buildx imagetools create $(IMAGE_DIGESTS) -t $(DOCKER_IMAGE)
	@$(SUDO) docker buildx imagetools create $(WEBHOOK_IMAGE_DIGESTS) -t $(WEBHOOK_DOCKER_IMAGE)

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
e2e: images crdgen
e2e:	## prepare e2e tests environment
	@if kind get clusters | grep -q $(KIND_CLUSTER_NAME); then \
		echo "e2e environment already running"; \
		exit 0; \
	fi; \
	kind create cluster --name $(KIND_CLUSTER_NAME) --image kindest/node:$(KIND_IMAGE_TAG) --config .github/kind-cluster.yaml; \
	kind load --name $(KIND_CLUSTER_NAME) docker-image $(DOCKER_IMAGE); \
	kind load --name $(KIND_CLUSTER_NAME) docker-image $(WEBHOOK_DOCKER_IMAGE); \
	if [ "$$(kubectl config current-context)" != "$(KUBE_CONTEXT)" ]; then \
		echo "ERROR: switch to kind context: kubectl config use-context $(KUBE_CONTEXT)"; \
		exit 1; \
	fi; \
	docker run -d --name cloud-provider-kind --network kind \
		-v /var/run/docker.sock:/var/run/docker.sock \
		--restart=always \
		registry.k8s.io/cloud-provider-kind/cloud-controller-manager:v0.8.0; \
	kubectl apply -f https://kind.sigs.k8s.io/examples/ingress/deploy-ingress-nginx.yaml; \
	kubectl create namespace $(KANIOP_NAMESPACE); \
	helm install kaniop ./charts/kaniop $(HELM_PARAMS); \
	ITERATION=1; \
	while [ $$ITERATION -le 20 ]; do \
		if kubectl -n $(KANIOP_NAMESPACE) get deploy $(KANIOP_NAMESPACE) | grep -q '1/1' && kubectl -n $(KANIOP_NAMESPACE) get deploy $(KANIOP_NAMESPACE)-webhook | grep -q '1/1'; then \
			echo "Kaniop and webhook deployments are ready"; \
			break; \
		else \
			echo "Retrying in 5 seconds..."; \
			sleep 5; \
		fi; \
		if [ $$ITERATION -eq 20 ]; then \
			echo "Kaniop or webhook deployment is not ready"; \
			kubectl -n $(KANIOP_NAMESPACE) describe pod -l app.kubernetes.io/instance=kaniop; \
			kubectl -n $(KANIOP_NAMESPACE) logs -l app.kubernetes.io/instance=kaniop; \
			kubectl -n $(KANIOP_NAMESPACE) describe pod -l app.kubernetes.io/component=webhook; \
			kubectl -n $(KANIOP_NAMESPACE) logs -l app.kubernetes.io/component=webhook; \
			exit 1; \
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
e2e-test: RUST_TEST_THREADS ?= 1000
e2e-test:	## run end to end tests
	@if [ "$$(kubectl config current-context)" != "$(KUBE_CONTEXT)" ]; then \
		echo "ERROR: switch to kind context: kubectl config use-context $(KUBE_CONTEXT)"; \
		exit 1; \
	fi
	kubectl get -A pods -o wide
	kubectl -n $(KANIOP_NAMESPACE) describe pod -l app.kubernetes.io/instance=kaniop
	RUST_TEST_THREADS=$(RUST_TEST_THREADS) cargo test $(CARGO_BUILD_PARAMS) -p kaniop-e2e-tests --features e2e-test || \
		(kubectl -n $(KANIOP_NAMESPACE) logs -l app.kubernetes.io/instance=kaniop && exit 2)

.PHONY: clean-e2e
clean-e2e:	## clean end to end environment: delete all created resources in kind
	@if [ "$$(kubectl config current-context)" != "$(KUBE_CONTEXT)" ]; then \
		echo "switch to the kind context only if deletion is necessary: kubectl config use-context $(KUBE_CONTEXT)"; \
		exit 0; \
	fi; \
	for resource in kanidmgroup person oauth2 kanidmserviceaccount kanidm secrets pvc statefulset; do \
		kubectl -n default delete $$resource --all --timeout=2s; \
		kubectl -n default get $$resource -o name 2>/dev/null | \
			xargs -I{} kubectl -n default patch {} -p '{"metadata":{"finalizers":[]}}' --type=merge || true; \
		kubectl -n default delete $$resource --all --force --grace-period=0 2>/dev/null; \
		kubectl -n kaniop delete $$resource -l owner!=helm --timeout=2s; \
		kubectl -n kaniop get $$resource -l owner!=helm -o name 2>/dev/null | \
			xargs -I{} kubectl -n kaniop patch {} -p '{"metadata":{"finalizers":[]}}' --type=merge || true; \
	done;

.PHONY: update-e2e-kaniop
update-e2e-kaniop: images crdgen
update-e2e-kaniop: ## update kaniop deployment in end to end tests with current code
	if [ "$$(kubectl config current-context)" != "$(KUBE_CONTEXT)" ]; then \
		echo "ERROR: switch to kind context: kubectl config use-context $(KUBE_CONTEXT)"; \
		exit 1; \
	fi; \
	kind load --name $(KIND_CLUSTER_NAME) docker-image $(DOCKER_IMAGE); \
	kind load --name $(KIND_CLUSTER_NAME) docker-image $(WEBHOOK_DOCKER_IMAGE); \
	helm upgrade kaniop ./charts/kaniop $(HELM_PARAMS); \
	kubectl -n $(KANIOP_NAMESPACE) rollout restart deploy $(KANIOP_NAMESPACE); \
	kubectl -n $(KANIOP_NAMESPACE) rollout restart deploy $(KANIOP_NAMESPACE)-webhook

.PHONY: delete-kind
delete-kind:	## delete kind K8s cluster. It will delete e2e environment.
	kind delete cluster --name $(KIND_CLUSTER_NAME); \
	docker rm -f cloud-provider-kind >/dev/null 2>&1

.PHONY: examples
examples: ## generate examples
	@cargo run --bin examples-gen

.PHONY: book
book: BOOK_DIR ?= .
book: MDBOOK_BUILD__BUILD_DIR ?= $(BOOK_DIR)/pando85.github.io/docs/kaniop/$(VERSION)
book:	## create book under Documentation/pando85.github.io
	BRANCH=$$(echo "$(VERSION)" | sed 's/latest/master/'); \
	MDBOOK_BUILD__BUILD_DIR=$(MDBOOK_BUILD__BUILD_DIR) mdbook build Documentation && \
	find Documentation/$(MDBOOK_BUILD__BUILD_DIR) -type f -name "*.md" -exec sed -i "s|{{KANIOP_VERSION}}|$$BRANCH|g" {} +

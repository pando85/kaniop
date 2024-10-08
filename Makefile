GH_ORG ?= pando85
VERSION ?= $(shell git rev-parse --short HEAD)
KUBERNETES_VERSION = 1.30
KIND_CLUSTER_NAME = chart-testing
KUBE_CONTEXT := kind-$(KIND_CLUSTER_NAME)
KANIOP_NAMESPACE := kaniop
KOPIUM_PATH ?= kopium
export CARGO_TARGET_DIR ?= target-$(CARGO_TARGET)
CARGO_TARGET ?= x86_64-unknown-linux-gnu
CARGO_BUILD_PARAMS = --target=$(CARGO_TARGET) --release
DOCKER_IMAGE ?= ghcr.io/$(GH_ORG)/kaniop:$(VERSION)
DOCKER_BUILD_PARAMS = --build-arg "CARGO_TARGET_DIR=$(CARGO_TARGET_DIR)" \
		--build-arg "CARGO_BUILD_TARGET=$(CARGO_TARGET)" \
		-t $(DOCKER_IMAGE) .
IMAGE_ARCHITECTURES := amd64 arm64
# build images in parallel
MAKEFLAGS += -j2
CRD_DIR := charts/kaniop/crds

.DEFAULT: help
.PHONY: help
help:	## Show this help menu.
	@echo "Usage: make [TARGET ...]"
	@echo ""
	@@egrep -h "#[#]" $(MAKEFILE_LIST) | sed -e 's/\\$$//' | awk 'BEGIN {FS = "[:=].*?#[#] "}; {printf "\033[36m%-20s\033[0m %s\n", $$1, $$2}'
	@echo ""

.NOTPARALLEL: crdgen
.PHONY: crdgen
crdgen: ## Generate CRDs
	@cargo run --bin crdgen > $(CRD_DIR)/crds.yaml

.PHONY: lint
lint:	## lint code
	cargo clippy --locked --all-targets --all-features -- -D warnings
	cargo fmt -- --check

.PHONY: test
test:	## run tests
test: lint
	cargo test

.PHONY: build
build:	## compile kaniop
build: release

.PHONY: release
release: CARGO_BUILD_PARAMS += --locked
release:	## compile release binary
	@if [ "$(CARGO_TARGET)" != "$(shell uname -m)-unknown-linux-gnu" ]; then  \
		if [ "$${CARGO_TARGET_DIR}" != "$${CARGO_TARGET_DIR#/}" ]; then  \
			echo CARGO_TARGET_DIR should be relative for cross compiling; \
			exit 1; \
		fi; \
		cargo install cross; \
		cross build --target-dir $(shell pwd)/$(CARGO_TARGET_DIR) $(CARGO_BUILD_PARAMS); \
	else \
		cargo build $(CARGO_BUILD_PARAMS); \
	fi
	@echo "binary is in $(CARGO_TARGET_DIR)/$(CARGO_TARGET)/release/kaniop"

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

push-image-%: crdgen
	# force multiple release targets
	$(MAKE) CARGO_TARGET=$(CARGO_TARGET) release
	$(SUDO) docker buildx build --push --no-cache --platform linux/$* $(DOCKER_BUILD_PARAMS)

push-image-amd64: CARGO_TARGET=x86_64-unknown-linux-gnu
push-image-arm64: CARGO_TARGET=aarch64-unknown-linux-gnu

.PHONY: push-images
push-images: $(IMAGE_ARCHITECTURES:%=push-image-%)
push-images:	## push images for all architectures

.PHONY: integration-test
integration-test:	## run integration tests
	@docker run -d --name tempo \
		-v $(shell pwd)/tests/integration/config/tempo.yaml:/etc/tempo.yaml \
		-p 4317:4317 \
		grafana/tempo:latest -config.file=/etc/tempo.yaml
	OPENTELEMETRY_ENDPOINT_URL=localhost:4317 cargo test --features integration-test integration; \
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
	kubectl create namespace $(KANIOP_NAMESPACE); \
	helm install kaniop ./charts/kaniop \
		--namespace $(KANIOP_NAMESPACE) \
		--set image.tag=$(VERSION) \
		--set logging.level='info\,kaniop=trace'; \
	for i in {1..20}; do \
		if kubectl -n $(KANIOP_NAMESPACE) get deploy $(KANIOP_NAMESPACE) | grep -q '1/1'; then \
			echo "Kaniop deployment is ready"; \
			break; \
		else \
			echo "Retrying in 5 seconds..."; \
			sleep 5; \
		fi; \
	done

.PHONY: e2e-test
e2e-test: e2e
e2e-test:	## run e2e tests
	@if [ "$$(kubectl config current-context)" != "$(KUBE_CONTEXT)" ]; then \
		echo "ERROR: switch to kind context: kubectl config use-context $(KUBE_CONTEXT)"; \
		exit 1; \
	fi
	cargo test -p tests --features e2e-test

.PHONY: clean-e2e
clean-e2e:	## clean e2e environment
	@if [ "$$(kubectl config current-context)" != "$(KUBE_CONTEXT)" ]; then \
		echo "switch to the kind context only if deletion is necessary: kubectl config use-context $(KUBE_CONTEXT)"; \
		exit 0; \
	fi; \
	kubectl -n default delete kanidm --all; \
	kubectl -n default delete deployment --all

.PHONY: delete-kind
delete-kind:
	kind delete cluster --name $(KIND_CLUSTER_NAME)

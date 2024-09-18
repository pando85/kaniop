GH_ORG ?= pando85
VERSION ?= $(shell git rev-parse --short HEAD)
KUBERNETES_VERSION = 1.30
KIND_CLUSTER_NAME = chart-testing
KOPIUM_PATH ?= kopium
export CARGO_TARGET_DIR ?= target-$(CARGO_TARGET)
CARGO_TARGET ?= x86_64-unknown-linux-gnu
CARGO_BUILD_PARAMS = --target=$(CARGO_TARGET) --release
DOCKER_BUILD_PARAMS = --build-arg "CARGO_TARGET_DIR=$(CARGO_TARGET_DIR)" \
		--build-arg "CARGO_BUILD_TARGET=$(CARGO_TARGET)" \
		-t ghcr.io/$(GH_ORG)/kaniop:$(VERSION) .
IMAGE_ARCHITECTURES := amd64 arm64
# build images in parallel
MAKEFLAGS += -j2

.DEFAULT: help
.PHONY: help
help:	## Show this help menu.
	@echo "Usage: make [TARGET ...]"
	@echo ""
	@@egrep -h "#[#]" $(MAKEFILE_LIST) | sed -e 's/\\$$//' | awk 'BEGIN {FS = "[:=].*?#[#] "}; {printf "\033[36m%-20s\033[0m %s\n", $$1, $$2}'
	@echo ""

.PHONY: kopium
kopium:	## install kopium
kopium:
	@if ! command -v $(KOPIUM_PATH) >/dev/null 2>&1; then \
		echo "$(KOPIUM_PATH) not found. Installing..."; \
		cargo install kopium; \
	else \
		echo "$(KOPIUM_PATH) is already installed."; \
	fi

TARGET_CRD_DIR := libs/operator/src/crd
CRD_DIR := crd
CRD_FILES := $(wildcard $(CRD_DIR)/*.yaml)

.PHONY: $(TARGET_CRD_DIR)/%.rs
$(TARGET_CRD_DIR)/%.rs: $(CRD_DIR)/%.yaml
	@echo "Generating $@ from $<"
	@kopium -f $< > $@

.PHONY: crd-code
crd-code: ## Generate code from CRD definitions
crd-code: kopium $(CRD_FILES:$(CRD_DIR)/%.yaml=$(TARGET_CRD_DIR)/%.rs)
	@echo "CRDs code generation complete."

.PHONY: lint
lint:	## lint code
lint: crd-code
	cargo clippy --locked --all-targets --all-features -- -D warnings
	cargo fmt -- --check

.PHONY: test
test:	## run tests
test: lint
	cargo test

.PHONY: build
build:	## compile kaniop
build: crd-code release

.PHONY: release
release: crd-code
release: CARGO_BUILD_PARAMS += --frozen
release:	## generate $(PKG_BASE_NAME).tar.gz with binary
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
update-version: */Cargo.toml
	@VERSION=$$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1); \
	sed -i -E "s/^(kaniop\_.*version\s=\s)\"(.*)\"/\1\"$$VERSION\"/gm" */Cargo.toml && \
	cargo update -p kaniop_operator && \
	echo updated to version "$$VERSION" cargo files

.PHONY: update-changelog
update-changelog:	## automatically update changelog based on commits
	git cliff -t v$(VERSION) -u -p CHANGELOG.md

.PHONY: publish
publish: crd-code
publish:	## publish crates
	@for package in $(shell find . -mindepth 2 -not -path './vendor/*' -name Cargo.toml -exec dirname {} \; | sort -r);do \
		cd $$package; \
		cargo publish; \
		cd -; \
	done;

.PHONY: image
image: crd-code release
image:	## build image
	@$(SUDO) docker buildx build --load $(DOCKER_BUILD_PARAMS)

push-image-%: crd-code
	# force multiple release targets
	$(MAKE) CARGO_TARGET=$(CARGO_TARGET) release
	$(SUDO) docker buildx build --push --no-cache --platform linux/$* $(DOCKER_BUILD_PARAMS)

push-image-amd64: CARGO_TARGET=x86_64-unknown-linux-gnu
push-image-arm64: CARGO_TARGET=aarch64-unknown-linux-gnu

.PHONY: push-images
push-images: crd-code $(IMAGE_ARCHITECTURES:%=push-image-%)
push-images:	## push images for all architectures

.PHONY: manifest
manifest:	## replace manifest variables
	sed -i "s#image: ghcr.io/.*kaniop.*#image: ghcr.io/$(GH_ORG)/kaniop:$(VERSION)#g" \
		install/kubernetes/deployment.yaml

.PHONY: kind
e2e: image manifest ## run e2e tests
	kind create cluster --name $(KIND_CLUSTER_NAME) --config .github/kind-cluster-$(KUBERNETES_VERSION).yaml
	kind load --name $(KIND_CLUSTER_NAME) docker-image ghcr.io/$(GH_ORG)/kaniop:$(VERSION)
	kubectl apply -f crd/echo.yaml
	kubectl apply -f install/kubernetes/rbac.yaml
	kubectl apply -f install/kubernetes/deployment.yaml
	for i in {1..20}; do \
		if kubectl get deploy kaniop | grep -E 'kaniop.*1/1'; then \
			echo "Kanio deployment is ready"; \
			break; \
		else \
			echo "Retrying in 3 seconds..."; \
			sleep 3; \
		fi \
	done
.PHONY: delete-kind
delete-kind:
	kind delete cluster --name $(KIND_CLUSTER_NAME)

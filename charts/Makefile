HELM_PATH ?= helm
# The plugin version is pinned instead of being resolved from api.github.com on every run: that
# endpoint answers 503 and rate limits requests, and a version lookup that can fail makes
# `make lint` and `make test` non deterministic. Renovate bumps the pin (see renovate.json5).
# renovate: datasource=github-releases depName=helm-unittest/helm-unittest
HELM_UNITTEST_VERSION ?= 1.2.1
# Release tags carry a "v" prefix while the asset names do not, so normalize whichever the pin uses.
HELM_UNITTEST_SEMVER := $(patsubst v%,%,$(HELM_UNITTEST_VERSION))

.DEFAULT: help
.PHONY: help
help:	## Show this help menu.
	@echo "Usage: make [TARGET ...]"
	@echo ""
	@@egrep -h "#[#]" $(MAKEFILE_LIST) | sed -e 's/\\$$//' | awk 'BEGIN {FS = "[:=].*?#[#] "}; {printf "\033[36m%-20s\033[0m %s\n", $$1, $$2}'
	@echo ""

.PHONY: dependencies
dependencies:	## install dependencies
dependencies:
	@if ! command -v $(HELM_PATH) >/dev/null 2>&1; then \
		echo "$(HELM_PATH) not found. Installing..."; \
		curl -fsSL --retry 5 --retry-delay 5 --retry-all-errors https://raw.githubusercontent.com/helm/helm/main/scripts/get-helm-3 | bash; \
	else \
		echo "$(HELM_PATH) is already installed."; \
	fi
	@if ! $(HELM_PATH) plugin list | grep -q 'unittest' >/dev/null 2>&1; then \
		echo "$(HELM_PATH) unittests not found. Installing v$(HELM_UNITTEST_SEMVER)..."; \
		HELM_PLUGIN_DIR=$$($(HELM_PATH) env HELM_PLUGINS); \
		ASSET="helm-unittest-linux-amd64-$(HELM_UNITTEST_SEMVER).tgz"; \
		ASSET_URL="https://github.com/helm-unittest/helm-unittest/releases/download/v$(HELM_UNITTEST_SEMVER)/$$ASSET"; \
		TMP_DIR=$$(mktemp -d); \
		for attempt in 1 2 3; do \
			[ $$attempt -eq 1 ] || sleep $$((attempt * 20)); \
			echo "Attempt $$attempt to download $$ASSET..."; \
			if curl -fsSL --connect-timeout 10 --max-time 600 --retry 3 --retry-delay 10 --retry-all-errors --retry-max-time 90 -o "$$TMP_DIR/helm-unittest.tgz" "$$ASSET_URL"; then \
				break; \
			fi; \
			if [ $$attempt -eq 3 ]; then \
				echo "ERROR: Failed to download $$ASSET from $$ASSET_URL"; \
				rm -rf "$$TMP_DIR"; \
				exit 1; \
			fi; \
		done; \
		mkdir -p "$$HELM_PLUGIN_DIR/unittest"; \
		tar -xzf "$$TMP_DIR/helm-unittest.tgz" -C "$$HELM_PLUGIN_DIR/unittest" || { \
			echo "ERROR: Failed to extract helm-unittest"; \
			rm -rf "$$TMP_DIR"; \
			exit 1; \
		}; \
		rm -rf "$$TMP_DIR"; \
		chmod +x "$$HELM_PLUGIN_DIR/unittest/untt-linux-amd64"; \
		echo "name: \"unittest\"" > "$$HELM_PLUGIN_DIR/unittest/plugin.yaml"; \
		echo "version: \"$(HELM_UNITTEST_SEMVER)\"" >> "$$HELM_PLUGIN_DIR/unittest/plugin.yaml"; \
		echo "usage: \"unittest for Helm charts\"" >> "$$HELM_PLUGIN_DIR/unittest/plugin.yaml"; \
		echo "description: \"A Helm plugin for unit testing charts\"" >> "$$HELM_PLUGIN_DIR/unittest/plugin.yaml"; \
		echo "command: \"$$HELM_PLUGIN_DIR/unittest/untt-linux-amd64\"" >> "$$HELM_PLUGIN_DIR/unittest/plugin.yaml"; \
		echo "helm-unittest installed successfully"; \
	else \
		echo "$(HELM_PATH) unittests is already installed."; \
	fi

.PHONY: lint
lint: dependencies
lint:	## lint charts
	@$(HELM_PATH) lint */

.PHONY: test
test: dependencies
test:	## test charts
	@$(HELM_PATH) unittest */

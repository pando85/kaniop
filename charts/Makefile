HELM_PATH ?= helm

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
		echo "$(HELM_PATH) unittests not found. Installing..."; \
		HELM_PLUGIN_DIR=$$($(HELM_PATH) env HELM_PLUGINS); \
		REPO_API="https://api.github.com/repos/helm-unittest/helm-unittest"; \
		echo "Fetching latest helm-unittest release..."; \
		LATEST_RELEASE=$$(curl -fsSL --retry 5 --retry-delay 5 --retry-all-errors "$$REPO_API/releases/latest"); \
		UNUNITTEST_VERSION=$$(echo "$$LATEST_RELEASE" | grep -oP '"tag_name":\s*"v\K[^"]+'); \
		if [ -z "$$UNUNITTEST_VERSION" ]; then \
			echo "ERROR: Failed to fetch latest helm-unittest version"; \
			exit 1; \
		fi; \
		echo "Latest version: $$UNUNITTEST_VERSION"; \
		ASSET="helm-unittest-linux-amd64-$${UNUNITTEST_VERSION}.tgz"; \
		ASSET_ID=$$(echo "$$LATEST_RELEASE" | tr -d ' \n\t' | sed -n "s/.*\"id\":\([0-9]\+\),\"node_id\":\"[^\"]*\",\"name\":\"$$ASSET\".*/\1/p"); \
		TMP_DIR=$$(mktemp -d); \
		echo "Downloading helm-unittest v$${UNUNITTEST_VERSION}..."; \
		for attempt in 1 2 3; do \
			echo "Attempt $$attempt to download $$ASSET..."; \
			if curl -fsSL --retry 3 --retry-delay 5 --retry-all-errors -o "$$TMP_DIR/helm-unittest.tgz" "https://github.com/helm-unittest/helm-unittest/releases/download/v$${UNUNITTEST_VERSION}/$$ASSET"; then \
				echo "helm-unittest downloaded from GitHub releases"; \
				break; \
			fi; \
			echo "Release download failed, trying the release asset API..."; \
			if [ -n "$$ASSET_ID" ] && \
				curl -fsSL --retry 3 --retry-delay 5 --retry-all-errors -H 'Accept: application/octet-stream' -o "$$TMP_DIR/helm-unittest.tgz" "$$REPO_API/releases/assets/$$ASSET_ID"; then \
				echo "helm-unittest downloaded from the release asset API"; \
				break; \
			fi; \
			if [ $$attempt -eq 3 ]; then \
				echo "ERROR: Failed to download $$ASSET from GitHub releases and the asset API"; \
				rm -rf "$$TMP_DIR"; \
				exit 1; \
			fi; \
			sleep $$((attempt * 10)); \
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
		echo "version: \"$$UNUNITTEST_VERSION\"" >> "$$HELM_PLUGIN_DIR/unittest/plugin.yaml"; \
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

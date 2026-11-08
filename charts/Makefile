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
		curl -fsSL https://raw.githubusercontent.com/helm/helm/main/scripts/get-helm-3 | bash; \
	else \
		echo "$(HELM_PATH) is already installed."; \
	fi
	@if ! $(HELM_PATH) plugin list | grep -q 'unittest' >/dev/null 2>&1; then \
		echo "$(HELM_PATH) unittests not found. Installing..."; \
		HELM_PLUGIN_DIR=$$($(HELM_PATH) env HELM_PLUGINS); \
		echo "Fetching latest helm-unittest release..."; \
		LATEST_RELEASE=$$(curl -fsSL https://api.github.com/repos/helm-unittest/helm-unittest/releases/latest); \
		UNUNITTEST_VERSION=$$(echo "$$LATEST_RELEASE" | grep -oP '"tag_name":\s*"v\K[^"]+'); \
		if [ -z "$$UNUNITTEST_VERSION" ]; then \
			echo "ERROR: Failed to fetch latest helm-unittest version"; \
			exit 1; \
		fi; \
		echo "Latest version: $$UNUNITTEST_VERSION"; \
		UNUNITTEST_URL="https://github.com/helm-unittest/helm-unittest/releases/download/v$${UNUNITTEST_VERSION}/helm-unittest-linux-amd64-$${UNUNITTEST_VERSION}.tgz"; \
		TMP_DIR=$$(mktemp -d); \
		echo "Downloading helm-unittest v$${UNUNITTEST_VERSION}..."; \
		curl -fsSL -o "$$TMP_DIR/helm-unittest.tgz" "$$UNUNITTEST_URL" || { \
			echo "ERROR: Failed to download helm-unittest from $$UNUNITTEST_URL"; \
			rm -rf "$$TMP_DIR"; \
			exit 1; \
		}; \
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

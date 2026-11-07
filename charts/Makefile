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
# Helm 4 requires --verify=false for Git-based plugins; Helm 3 does not support it.
# Feature-detect the option to work with either Helm major version.
	@if ! $(HELM_PATH) plugin list | grep -q 'unittest' >/dev/null 2>&1; then \
		echo "$(HELM_PATH) unittests not found. Installing..."; \
		VERIFY_FLAG=""; \
		if $(HELM_PATH) plugin install --help 2>&1 | grep -q -- '--verify'; then \
			VERIFY_FLAG="--verify=false"; \
		fi; \
		$(HELM_PATH) plugin install https://github.com/helm-unittest/helm-unittest.git $$VERIFY_FLAG; \
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

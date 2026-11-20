# open-pr-agent — developer entry points.
#
# Run `make help` for the target list. CI calls the same targets, so a green
# local run means a green pipeline.

SHELL := /bin/bash
.DEFAULT_GOAL := help

CARGO      ?= cargo
DOCKER     ?= docker
ZENSICAL   ?= zensical
PYTHON     ?= python3

RUST_DIR   := rust
TARGET_DIR := $(RUST_DIR)/target
MANIFEST   := $(RUST_DIR)/Cargo.toml
BINS       := pr_agent pr_agent_server

IMAGE      ?= open-pr-agent
TAG        ?= latest
SITE_DIR   := site

.PHONY: help all build test format fmt lint check ci docker-build docker-push \
        docs-install docs-serve docs-build run server doctor clean

all: build

help: ## Show this help
	@awk 'BEGIN {FS = ":.*##"; printf "\nUsage:\n  make \033[36m<target>\033[0m\n\nTargets:\n"} \
	     /^[a-zA-Z0-9_-]+:.*?##/ { printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2 } \
	     END { print "" }' $(MAKEFILE_LIST)

build: ## Build release binaries
	$(CARGO) build --release --manifest-path $(MANIFEST) $(addprefix --bin ,$(BINS))

test: ## Run unit and CLI integration tests
	$(CARGO) test --manifest-path $(MANIFEST)

format: ## Format the Rust workspace
	$(CARGO) fmt --manifest-path $(MANIFEST) --all

fmt: format ## Alias for `format`

lint: ## Check formatting and run clippy
	$(CARGO) fmt --manifest-path $(MANIFEST) --all -- --check
	$(CARGO) clippy --manifest-path $(MANIFEST) --all-targets -- -D warnings

check: lint test ## Run lint and tests

ci: lint test build ## Run the CI checks locally

docker-build: ## Build the container image (IMAGE and TAG variables)
	$(DOCKER) build -f docker/Dockerfile.rust -t $(IMAGE):$(TAG) .

docker-push: ## Push the container image (IMAGE and TAG variables)
	$(DOCKER) push $(IMAGE):$(TAG)

docs-install: ## Install the docs builder
	$(PYTHON) -m pip install -r requirements-docs.txt

docs-serve: ## Serve the docs with live reload
	$(ZENSICAL) serve

docs-build: ## Build the static docs site into site/
	$(ZENSICAL) build --clean

run: build ## Run a review (PR_URL=... [COMMAND=review])
	@test -n "$(PR_URL)" || { echo "PR_URL is required: make run PR_URL=<pull request url>" >&2; exit 1; }
	$(TARGET_DIR)/release/pr_agent --pr_url "$(PR_URL)" $(or $(COMMAND),review)

server: build ## Run the webhook server
	$(TARGET_DIR)/release/pr_agent_server

doctor: build ## Check the engine (binary + model ping)
	$(TARGET_DIR)/release/pr_agent doctor

clean: ## Remove build artifacts and the docs site
	$(CARGO) clean --manifest-path $(MANIFEST)
	rm -rf $(SITE_DIR)

.PHONY: help build test check fmt clippy doc clean

help: ## Show this help
	@awk 'BEGIN {FS = ":.*?## "} /^[a-zA-Z_-]+:.*?##/ {printf "\033[36m%-15s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)

build: ## Build the crate
	cargo build

test: ## Run tests (no capture, for visual verification)
	cargo test -- --nocapture

check: ## Type-check without building
	cargo check

fmt: ## Format code
	cargo fmt

clippy: ## Run clippy
	cargo clippy --all-targets --all-features -- -D warnings

doc: ## Build documentation
	cargo doc --no-deps

clean: ## Remove build artifacts
	cargo clean

test-captured: ## Run tests with captured output
	cargo test

test-external: ## Run ignored external/network tests
	cargo test -- --ignored

test-all: ## Run all tests including ignored (nocapture), then build all examples
	cargo test -- --include-ignored --nocapture
	cargo build --examples

demos: demo-backpressure demo-panic ## Run safe (non-hanging) demos
	@echo "Safe demos completed."

proofs: ## Run nested proof example
	cargo run --example nested_proofs

stress: ## Run stress test example
	cargo run --example stress_test

demo-backpressure: ## Run backpressure demo
	cargo run --example demo_backpressure

demo-panic: ## Run panic demo
	cargo run --example demo_panic

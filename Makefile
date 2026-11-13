CARGO ?= cargo
FIELDKIT_TEST_BINARY ?= $(abspath $(if $(CARGO_TARGET_DIR),$(CARGO_TARGET_DIR),target))/debug/fieldkit

.PHONY: test test-inkling test-historical-replay test-source-reconstruction build doctor
test:
	$(CARGO) build --locked -p fieldkit
	FIELDKIT_TEST_BINARY="$(FIELDKIT_TEST_BINARY)" $(CARGO) test --workspace --locked
	node tests/frontend_test.mjs
	$(CARGO) run --locked -p fieldkit-agent --bin fieldkit-agent-benchmark -- --mode replay

test-historical-replay:
	$(CARGO) test --locked -p fieldkit-benchmarks south_africa

test-source-reconstruction:
	$(CARGO) test --locked -p fieldkit-benchmarks event_stream

test-inkling:
	$(CARGO) build --locked -p fieldkit
	FIELDKIT_TEST_BINARY="$(FIELDKIT_TEST_BINARY)" $(CARGO) test --locked -p fieldkit-agent
	$(CARGO) run --locked -p fieldkit-agent --bin fieldkit-agent-benchmark -- --mode replay

build:
	$(CARGO) build --workspace --release --locked

doctor:
	./fieldkit doctor

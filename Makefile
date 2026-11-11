CARGO ?= cargo

.PHONY: test build doctor
test:
	$(CARGO) test --locked
	node tests/frontend_test.mjs

build:
	$(CARGO) build --release --locked

doctor:
	./fieldkit doctor

CARGO ?= cargo
PYTHON ?= python3

.PHONY: test test-inkling test-historical-replay test-source-reconstruction build doctor
test:
	$(CARGO) test --locked
	node tests/frontend_test.mjs
	$(MAKE) test-historical-replay
	$(MAKE) test-source-reconstruction
	$(MAKE) test-inkling

test-historical-replay:
	$(PYTHON) -I benchmarks/south-africa-2017/test_run.py

test-source-reconstruction:
	$(PYTHON) -I benchmarks/event-stream-2018/test_run.py

test-inkling:
	$(PYTHON) -I -m unittest discover -s integrations/inkling -p 'test_*.py' -v
	$(PYTHON) -I integrations/inkling/benchmark.py --mode replay

build:
	$(CARGO) build --release --locked

doctor:
	./fieldkit doctor

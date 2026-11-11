PYTHON ?= python3

.PHONY: test native doctor
test:
	$(PYTHON) -m unittest discover -s tests -v

native:
	$(MAKE) -C native/handlecheck

doctor:
	./fieldkit doctor

CXX ?= c++
CPPFLAGS += -Iinclude -Ithird_party
CXXFLAGS ?= -O2
CXXFLAGS += -std=c++17 -Wall -Wextra -Wpedantic -pthread -MMD -MP
LDLIBS += -lcurl -pthread

CORE = build/catalog.o build/classifier.o
APP = $(CORE) build/http.o build/main.o

.PHONY: all test clean
all: handlecheck

handlecheck: $(APP)
	$(CXX) $(CXXFLAGS) $^ $(LDLIBS) -o $@

build/%.o: src/%.cpp include/handlecheck.hpp
	@mkdir -p build
	$(CXX) $(CPPFLAGS) $(CXXFLAGS) -c $< -o $@

build/classifier_test.o: tests/classifier_test.cpp include/handlecheck.hpp
	@mkdir -p build
	$(CXX) $(CPPFLAGS) $(CXXFLAGS) -c $< -o $@

build/classifier_test: $(CORE) build/classifier_test.o
	$(CXX) $(CXXFLAGS) $^ -pthread -o $@

build/fake_http.o: tests/fake_http.cpp include/handlecheck.hpp
	@mkdir -p build
	$(CXX) $(CPPFLAGS) $(CXXFLAGS) -c $< -o $@

build/handlecheck_fixture: $(CORE) build/main.o build/fake_http.o
	$(CXX) $(CXXFLAGS) $^ -pthread -o $@

test: build/classifier_test build/handlecheck_fixture handlecheck
	./build/classifier_test
	python3 tests/cli_test.py build/handlecheck_fixture

clean:
	rm -rf build handlecheck

-include $(wildcard build/*.d)

.PHONY: build test clean

build:
	cargo build --release

test:
	./scripts/test.sh

clean:
	cargo clean
	rm -rf build


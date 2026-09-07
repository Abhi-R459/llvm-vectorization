.PHONY: build test lint differential differential-full benchmark clean

build:
	cargo build --release

test:
	./scripts/test.sh

lint:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings

differential:
	./scripts/differential_test.py

differential-full:
	./scripts/differential_test.py --full

benchmark:
	./scripts/benchmark.py

clean:
	cargo clean
	rm -rf build

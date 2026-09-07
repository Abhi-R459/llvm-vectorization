# Rust Loop Vectorizer

This repository implements a small, correctness-first automatic loop
vectorizer as an LLVM 21 New Pass Manager plugin. The registration adapter is
C++; loop recognition, affine dependence tests, profitability policy, and IR
widening are implemented in Rust through LLVM's C API.

The project intentionally handles a documented subset of canonical loops and
emits a diagnostic when it cannot prove legality. It is a research and teaching
implementation, not a replacement for LLVM's production Loop Vectorizer.

## Toolchain

- Rust 1.85 or newer
- LLVM 21.x with `llvm-config`, `opt`, and `clang`
- A C++17 compiler

On a Homebrew LLVM installation:

```sh
export LLVM_SYS_211_PREFIX="$(brew --prefix llvm)"
export PATH="$LLVM_SYS_211_PREFIX/bin:$PATH"
cargo build --release
```

The research rationale, supported loop subset, examples, verification suite,
and benchmark protocol will be filled in alongside the implementation.


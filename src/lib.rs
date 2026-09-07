//! Rust implementation of a deliberately bounded LLVM loop-vectorization pass.
//!
//! The C++ file in `native/` only registers the pass with LLVM's New Pass
//! Manager. Legality, dependence analysis, planning, and IR transformation live
//! in Rust.

mod config;
mod cost;
mod dependence;

use std::ffi::{c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};

use config::{Heuristic, PassConfig};

const PLUGIN_API_VERSION: u32 = 1;
const PLUGIN_NAME: &[u8] = b"rust-loop-vectorizer\0";
const PLUGIN_VERSION: &[u8] = concat!(env!("CARGO_PKG_VERSION"), "\0").as_bytes();

#[repr(C)]
struct PassPluginLibraryInfo {
    api_version: u32,
    plugin_name: *const c_char,
    plugin_version: *const c_char,
    register_pass_builder_callbacks: unsafe extern "C" fn(*mut c_void),
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RVPassConfig {
    heuristic: u32,
    forced_vf: u32,
    emit_remarks: u32,
}

unsafe extern "C" {
    fn rv_register_pass_builder_callbacks(builder: *mut c_void);
}

/// LLVM's stable dynamic pass-plugin entry point.
#[unsafe(no_mangle)]
extern "C" fn llvmGetPassPluginInfo() -> PassPluginLibraryInfo {
    PassPluginLibraryInfo {
        api_version: PLUGIN_API_VERSION,
        plugin_name: PLUGIN_NAME.as_ptr().cast(),
        plugin_version: PLUGIN_VERSION.as_ptr().cast(),
        register_pass_builder_callbacks: rv_register_pass_builder_callbacks,
    }
}

/// Entry called by the C++ pass adapter. Panics never cross the FFI boundary.
#[unsafe(no_mangle)]
pub extern "C" fn rv_run_module(raw_module: *mut c_void, raw_config: *const RVPassConfig) -> bool {
    if raw_module.is_null() || raw_config.is_null() {
        return false;
    }

    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: LLVM invokes the adapter with a live Module and the adapter
        // passes a pointer to its by-value configuration for this call.
        let ffi_config = unsafe { *raw_config };
        let config = PassConfig::new(
            Heuristic::from_ffi(ffi_config.heuristic),
            ffi_config.forced_vf,
            ffi_config.emit_remarks != 0,
        );
        run_module(raw_module, config)
    }))
    .unwrap_or(false)
}

fn run_module(_raw_module: *mut c_void, _config: PassConfig) -> bool {
    // The registration milestone intentionally performs no mutation. The next
    // milestone wires this entry point to the Rust legality and widening code.
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffi_configuration_is_total() {
        for raw in [0, 1, 2, u32::MAX] {
            let config = PassConfig::new(Heuristic::from_ffi(raw), 0, false);
            assert!(config.vector_bits() >= 128);
        }
    }
}

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=native/pass_plugin.cpp");
    println!("cargo:rerun-if-env-changed=LLVM_CONFIG_PATH");
    println!("cargo:rerun-if-env-changed=LLVM_SYS_211_PREFIX");

    let llvm_config = find_llvm_config().unwrap_or_else(|| {
        panic!(
            "LLVM 21 was not found. Set LLVM_CONFIG_PATH to llvm-config or \
             LLVM_SYS_211_PREFIX to the LLVM 21 installation prefix."
        )
    });

    let version = query(&llvm_config, &["--version"]);
    let major = version
        .trim()
        .split('.')
        .next()
        .and_then(|part| part.parse::<u32>().ok());
    assert_eq!(
        major,
        Some(21),
        "rust-loop-vectorizer requires LLVM 21.x, but {} reports {}",
        llvm_config.display(),
        version.trim(),
    );

    let includedir = query(&llvm_config, &["--includedir"]);
    let libdir = query(&llvm_config, &["--libdir"]);
    let cxxflags = query(&llvm_config, &["--cxxflags"]);

    let mut bridge = cc::Build::new();
    bridge
        .cpp(true)
        .file("native/pass_plugin.cpp")
        .include(includedir.trim())
        .flag_if_supported("-std=c++17")
        // LLVM's headers intentionally contain unused parameters in template
        // interfaces. The end-to-end test script separately compiles this
        // bridge with warnings as errors, while this build suppresses noise
        // originating in dependency headers.
        .warnings(false);

    for flag in cxxflags.split_whitespace() {
        if flag.starts_with("-D") || flag == "-fno-exceptions" || flag == "-funwind-tables" {
            bridge.flag(flag);
        }
    }
    bridge.compile("rv_pass_bridge");

    println!("cargo:rustc-link-search=native={}", libdir.trim());
    let link_flags = query(
        &llvm_config,
        &["--link-shared", "--libs", "core", "passes", "support"],
    );
    for flag in link_flags.split_whitespace() {
        if let Some(lib) = flag.strip_prefix("-l") {
            println!("cargo:rustc-link-lib=dylib={lib}");
        }
    }

    let system_libs = query(&llvm_config, &["--link-shared", "--system-libs"]);
    for flag in system_libs.split_whitespace() {
        if let Some(lib) = flag.strip_prefix("-l") {
            println!("cargo:rustc-link-lib={lib}");
        }
    }

    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", libdir.trim());
    println!("cargo:rustc-env=RV_LLVM_VERSION={}", version.trim());
}

fn find_llvm_config() -> Option<PathBuf> {
    if let Some(path) = env::var_os("LLVM_CONFIG_PATH") {
        let path = PathBuf::from(path);
        if is_llvm_config(&path) {
            return Some(path);
        }
    }

    if let Some(prefix) = env::var_os("LLVM_SYS_211_PREFIX") {
        let path = PathBuf::from(prefix).join("bin/llvm-config");
        if is_llvm_config(&path) {
            return Some(path);
        }
    }

    for executable in ["llvm-config-21", "llvm-config"] {
        if let Some(path) = find_on_path(executable) {
            if is_llvm_config(&path) {
                return Some(path);
            }
        }
    }

    for path in [
        "/opt/homebrew/opt/llvm/bin/llvm-config",
        "/usr/local/opt/llvm/bin/llvm-config",
        "/usr/lib/llvm-21/bin/llvm-config",
    ] {
        let path = PathBuf::from(path);
        if is_llvm_config(&path) {
            return Some(path);
        }
    }
    None
}

fn find_on_path(executable: &str) -> Option<PathBuf> {
    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths)
            .map(|path| path.join(executable))
            .find(|path| path.is_file())
    })
}

fn is_llvm_config(path: &Path) -> bool {
    path.is_file()
        && Command::new(path)
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
}

fn query(llvm_config: &Path, arguments: &[&str]) -> String {
    let output = Command::new(llvm_config)
        .args(arguments)
        .output()
        .unwrap_or_else(|error| panic!("failed to execute {}: {error}", llvm_config.display()));
    assert!(
        output.status.success(),
        "{} {arguments:?} failed: {}",
        llvm_config.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap_or_else(|error| panic!("llvm-config returned non-UTF-8 output: {error}"))
}

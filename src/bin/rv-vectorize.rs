use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const HELP: &str = r"rv-vectorize - run the Rust LLVM loop-vectorization pass

USAGE:
    rv-vectorize [OPTIONS] <INPUT.ll|INPUT.bc>

OPTIONS:
    -o, --output <PATH>       Output path (default: stdout)
        --policy <POLICY>     balanced, conservative, or aggressive
        --vf <WIDTH>          auto, 2, 4, 8, or 16
        --report              Print per-loop decisions to stderr
        --emit-bitcode        Emit LLVM bitcode instead of textual IR
        --no-verify           Do not run LLVM's verifier after the pass
        --llvm-prefix <PATH>  LLVM 21 installation prefix
        --opt <PATH>          Path to LLVM 21 opt
        --plugin <PATH>       Path to the vectorizer pass plugin
        --dry-run             Print the resolved opt command without running it
    -h, --help                Print help
    -V, --version             Print version

ENVIRONMENT:
    LLVM_SYS_211_PREFIX       LLVM 21 installation prefix
    LLVM_CONFIG_PATH         Path to llvm-config (its sibling opt is used)
    RV_PLUGIN_PATH           Path to the vectorizer pass plugin

The input must be LLVM IR or bitcode. To compile C first:
    clang -O1 -S -emit-llvm -fno-vectorize -fno-slp-vectorize input.c -o input.ll
";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Policy {
    #[default]
    Balanced,
    Conservative,
    Aggressive,
}

impl Policy {
    fn parse(value: &OsStr) -> Result<Self, String> {
        match value.to_str() {
            Some("balanced") => Ok(Self::Balanced),
            Some("conservative") => Ok(Self::Conservative),
            Some("aggressive") => Ok(Self::Aggressive),
            _ => Err(format!(
                "invalid policy {value:?}; expected balanced, conservative, or aggressive"
            )),
        }
    }

    const fn pass_stem(self) -> &'static str {
        match self {
            Self::Balanced => "rust-loop-vectorize",
            Self::Conservative => "rust-loop-vectorize-conservative",
            Self::Aggressive => "rust-loop-vectorize-aggressive",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum VectorWidth {
    #[default]
    Auto,
    Fixed(u8),
}

impl VectorWidth {
    fn parse(value: &OsStr) -> Result<Self, String> {
        match value.to_str() {
            Some("auto") => Ok(Self::Auto),
            Some("2") => Ok(Self::Fixed(2)),
            Some("4") => Ok(Self::Fixed(4)),
            Some("8") => Ok(Self::Fixed(8)),
            Some("16") => Ok(Self::Fixed(16)),
            _ => Err(format!(
                "invalid vector width {value:?}; expected auto, 2, 4, 8, or 16"
            )),
        }
    }
}

#[derive(Debug, Default, Eq, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
struct Options {
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    policy: Policy,
    vector_width: VectorWidth,
    report: bool,
    emit_bitcode: bool,
    verify: bool,
    llvm_prefix: Option<PathBuf>,
    opt: Option<PathBuf>,
    plugin: Option<PathBuf>,
    dry_run: bool,
}

enum Parsed {
    Run(Options),
    Help,
    Version,
}

fn take_value(arguments: &[OsString], index: &mut usize, option: &str) -> Result<OsString, String> {
    *index += 1;
    arguments
        .get(*index)
        .cloned()
        .ok_or_else(|| format!("{option} requires a value"))
}

fn set_input(input: &mut Option<PathBuf>, value: &OsStr) -> Result<(), String> {
    if input.is_some() {
        return Err(format!("unexpected extra input {value:?}"));
    }
    *input = Some(PathBuf::from(value));
    Ok(())
}

fn parse_args(arguments: &[OsString]) -> Result<Parsed, String> {
    let mut options = Options {
        verify: true,
        ..Options::default()
    };
    let mut index = 0;
    let mut positional_only = false;

    while index < arguments.len() {
        let argument = &arguments[index];
        let text = argument.to_str();

        if positional_only {
            set_input(&mut options.input, argument)?;
        } else if text == Some("--") {
            positional_only = true;
        } else if matches!(text, Some("-h" | "--help")) {
            return Ok(Parsed::Help);
        } else if matches!(text, Some("-V" | "--version")) {
            return Ok(Parsed::Version);
        } else if matches!(text, Some("-o" | "--output")) {
            options.output = Some(PathBuf::from(take_value(
                arguments,
                &mut index,
                text.unwrap_or("--output"),
            )?));
        } else if text == Some("--policy") {
            options.policy = Policy::parse(&take_value(arguments, &mut index, "--policy")?)?;
        } else if text == Some("--vf") {
            options.vector_width = VectorWidth::parse(&take_value(arguments, &mut index, "--vf")?)?;
        } else if text == Some("--llvm-prefix") {
            options.llvm_prefix = Some(PathBuf::from(take_value(
                arguments,
                &mut index,
                "--llvm-prefix",
            )?));
        } else if text == Some("--opt") {
            options.opt = Some(PathBuf::from(take_value(arguments, &mut index, "--opt")?));
        } else if text == Some("--plugin") {
            options.plugin = Some(PathBuf::from(take_value(
                arguments, &mut index, "--plugin",
            )?));
        } else if text == Some("--report") {
            options.report = true;
        } else if text == Some("--emit-bitcode") {
            options.emit_bitcode = true;
        } else if text == Some("--no-verify") {
            options.verify = false;
        } else if text == Some("--dry-run") {
            options.dry_run = true;
        } else if let Some(value) = text.and_then(|value| value.strip_prefix("--output=")) {
            options.output = Some(PathBuf::from(value));
        } else if let Some(value) = text.and_then(|value| value.strip_prefix("--policy=")) {
            options.policy = Policy::parse(OsStr::new(value))?;
        } else if let Some(value) = text.and_then(|value| value.strip_prefix("--vf=")) {
            options.vector_width = VectorWidth::parse(OsStr::new(value))?;
        } else if let Some(value) = text.and_then(|value| value.strip_prefix("--llvm-prefix=")) {
            options.llvm_prefix = Some(PathBuf::from(value));
        } else if let Some(value) = text.and_then(|value| value.strip_prefix("--opt=")) {
            options.opt = Some(PathBuf::from(value));
        } else if let Some(value) = text.and_then(|value| value.strip_prefix("--plugin=")) {
            options.plugin = Some(PathBuf::from(value));
        } else if text.is_some_and(|value| value.starts_with('-')) {
            return Err(format!("unknown option {argument:?}"));
        } else {
            set_input(&mut options.input, argument)?;
        }
        index += 1;
    }

    if options.input.is_none() {
        return Err("missing input LLVM IR or bitcode file".to_owned());
    }
    if options.policy != Policy::Balanced && options.vector_width != VectorWidth::Auto {
        return Err("--policy and a fixed --vf cannot be combined".to_owned());
    }
    if let (Some(input), Some(output)) = (&options.input, &options.output) {
        if input == output {
            return Err("input and output paths must differ".to_owned());
        }
    }
    Ok(Parsed::Run(options))
}

fn pass_pipeline(options: &Options) -> String {
    let mut pass = match options.vector_width {
        VectorWidth::Auto => options.policy.pass_stem().to_owned(),
        VectorWidth::Fixed(width) => format!("rust-loop-vectorize-force-vf{width}"),
    };
    if options.report {
        pass.push_str("-report");
    }
    if options.verify {
        pass.push_str(",verify");
    }
    pass
}

fn llvm_prefix_candidates(options: &Options) -> Vec<PathBuf> {
    if let Some(prefix) = &options.llvm_prefix {
        return vec![prefix.clone()];
    }

    let mut candidates = Vec::new();
    if let Some(prefix) = env::var_os("LLVM_SYS_211_PREFIX") {
        candidates.push(PathBuf::from(prefix));
    }
    if let Some(config) = env::var_os("LLVM_CONFIG_PATH") {
        let config = PathBuf::from(config);
        if let Some(prefix) = config.parent().and_then(Path::parent) {
            candidates.push(prefix.to_owned());
        }
    }
    candidates.extend(
        [
            "/opt/homebrew/opt/llvm",
            "/usr/local/opt/llvm",
            "/usr/lib/llvm-21",
        ]
        .map(PathBuf::from),
    );
    candidates
}

fn is_llvm_21_opt(path: &Path) -> bool {
    Command::new(path)
        .arg("--version")
        .output()
        .is_ok_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("LLVM version 21.")
        })
}

fn resolve_opt(options: &Options) -> Result<PathBuf, String> {
    if let Some(path) = &options.opt {
        if is_llvm_21_opt(path) {
            return Ok(path.clone());
        }
        return Err(format!(
            "{} is not an executable LLVM 21 opt",
            path.display()
        ));
    }
    for prefix in llvm_prefix_candidates(options) {
        let candidate = prefix.join("bin/opt");
        if is_llvm_21_opt(&candidate) {
            return Ok(candidate);
        }
    }
    Err("LLVM 21 opt was not found; use --opt, --llvm-prefix, or LLVM_SYS_211_PREFIX".to_owned())
}

fn plugin_filename() -> &'static str {
    if cfg!(target_os = "macos") {
        "librust_loop_vectorizer.dylib"
    } else if cfg!(target_os = "windows") {
        "rust_loop_vectorizer.dll"
    } else {
        "librust_loop_vectorizer.so"
    }
}

fn resolve_plugin(options: &Options) -> Result<PathBuf, String> {
    if let Some(path) = &options.plugin {
        return path
            .is_file()
            .then(|| path.clone())
            .ok_or_else(|| format!("pass plugin not found at {}", path.display()));
    }
    if let Some(path) = env::var_os("RV_PLUGIN_PATH") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!(
            "pass plugin from RV_PLUGIN_PATH not found at {}",
            path.display()
        ));
    }

    let mut candidates = Vec::new();
    if let Ok(executable) = env::current_exe() {
        if let Some(directory) = executable.parent() {
            candidates.push(directory.join(plugin_filename()));
        }
    }
    if let Ok(directory) = env::current_dir() {
        candidates.push(directory.join("target/release").join(plugin_filename()));
        candidates.push(directory.join("target/debug").join(plugin_filename()));
    }
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| {
            "pass plugin was not found; run cargo build --release or use --plugin".to_owned()
        })
}

fn shell_quote(value: &OsStr) -> String {
    let text = value.to_string_lossy();
    if !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_+-.,/:=".contains(&byte))
    {
        return text.into_owned();
    }
    let mut quoted = String::from("'");
    for character in text.chars() {
        if character == '\'' {
            quoted.push_str("'\\''");
        } else {
            let _ = quoted.write_char(character);
        }
    }
    quoted.push('\'');
    quoted
}

fn command_display(command: &Command) -> String {
    std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(shell_quote)
        .collect::<Vec<_>>()
        .join(" ")
}

fn build_command(options: &Options, opt: &Path, plugin: &Path) -> Command {
    let mut command = Command::new(opt);
    command.arg(format!("-load-pass-plugin={}", plugin.display()));
    command.arg(format!("-passes={}", pass_pipeline(options)));
    if !options.emit_bitcode {
        command.arg("-S");
    }
    command.arg(options.input.as_ref().expect("validated input"));
    command.arg("-o");
    command.arg(options.output.as_deref().unwrap_or_else(|| Path::new("-")));
    command
}

fn run(options: &Options) -> Result<(), String> {
    let input = options.input.as_ref().expect("validated input");
    if !input.is_file() {
        return Err(format!("input file not found at {}", input.display()));
    }
    let opt = resolve_opt(options)?;
    let plugin = resolve_plugin(options)?;
    let mut command = build_command(options, &opt, &plugin);

    if options.dry_run {
        println!("{}", command_display(&command));
        return Ok(());
    }

    let status = command
        .status()
        .map_err(|error| format!("failed to execute {}: {error}", opt.display()))?;
    if status.success() {
        Ok(())
    } else {
        Err(match status.code() {
            Some(code) => format!("LLVM opt exited with status {code}"),
            None => "LLVM opt was terminated by a signal".to_owned(),
        })
    }
}

fn main() -> ExitCode {
    let arguments: Vec<_> = env::args_os().skip(1).collect();
    match parse_args(&arguments) {
        Ok(Parsed::Help) => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Ok(Parsed::Version) => {
            println!("rv-vectorize {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Ok(Parsed::Run(options)) => match run(&options) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("rv-vectorize: error: {error}");
                ExitCode::from(1)
            }
        },
        Err(error) => {
            eprintln!("rv-vectorize: error: {error}\n\nTry 'rv-vectorize --help' for usage.");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_normal_invocation() {
        let Parsed::Run(options) = parse_args(&args(&[
            "--policy",
            "aggressive",
            "--report",
            "-o",
            "out.ll",
            "in.ll",
        ]))
        .expect("valid arguments") else {
            panic!("expected runnable options");
        };
        assert_eq!(options.input, Some(PathBuf::from("in.ll")));
        assert_eq!(options.output, Some(PathBuf::from("out.ll")));
        assert_eq!(options.policy, Policy::Aggressive);
        assert!(options.report);
    }

    #[test]
    fn builds_balanced_pipeline_with_verifier() {
        let options = Options {
            verify: true,
            report: true,
            ..Options::default()
        };
        assert_eq!(pass_pipeline(&options), "rust-loop-vectorize-report,verify");
    }

    #[test]
    fn builds_forced_width_pipeline() {
        let options = Options {
            vector_width: VectorWidth::Fixed(8),
            verify: false,
            ..Options::default()
        };
        assert_eq!(pass_pipeline(&options), "rust-loop-vectorize-force-vf8");
    }

    #[test]
    fn rejects_policy_with_fixed_width() {
        let result = parse_args(&args(&["--policy=aggressive", "--vf=4", "in.ll"]));
        assert!(matches!(result, Err(message) if message.contains("cannot be combined")));
    }

    #[test]
    fn quotes_shell_metacharacters() {
        assert_eq!(shell_quote(OsStr::new("plain.ll")), "plain.ll");
        assert_eq!(
            shell_quote(OsStr::new("it's here.ll")),
            "'it'\\''s here.ll'"
        );
    }
}

use std::path::{Path, PathBuf};

use ut_compare::adapters::executable_version;
use ut_compare::config::{profile, Config};
use ut_compare::{privacy, report, runner};

fn main() {
    std::panic::set_hook(Box::new(|_| {
        eprintln!("ut-compare: unexpected failure; diagnostic details omitted for privacy")
    }));
    let code = match dispatch(std::env::args().skip(1).collect()) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("ut-compare: {}", privacy::public_error(&error));
            1
        }
    };
    std::process::exit(code);
}

fn dispatch(args: Vec<String>) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("run") => run_command(&args[1..]),
        Some("report") => report_command(&args[1..]),
        Some("sanitize") => sanitize_command(&args[1..]),
        Some("doctor") => doctor_command(&args[1..]),
        Some("--version" | "-V") => {
            println!("ut-compare {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("--help" | "-h") | None => {
            print_help();
            Ok(())
        }
        Some(other) => Err(format!("unknown command {other:?}; run with --help")),
    }
}

fn run_command(args: &[String]) -> Result<(), String> {
    let config_path = option_path(args, "--config").unwrap_or_else(|| "comparison.toml".into());
    let profile_name = option(args, "--profile").unwrap_or("standard");
    let output_path = option_path(args, "--output").unwrap_or_else(|| "reports/run.json".into());
    let markdown_path = option_path(args, "--markdown");
    let seed_override = option(args, "--seed");
    reject_unknown_options(
        args,
        &["--config", "--profile", "--output", "--markdown", "--seed"],
        false,
    )?;
    let mut config = Config::load(&config_path)?;
    if let Some(seed) = seed_override {
        config.comparison.seed = seed
            .parse()
            .map_err(|_| format!("--seed expects a non-negative integer, got {seed:?}"))?;
    }
    let profile = profile(profile_name)?;
    eprintln!(
        "running {} profile for {} contender(s)",
        profile.name,
        config.contenders.len()
    );
    let result = runner::run(&config, profile)?;
    write_json_atomic(&output_path, &result)?;
    eprintln!("wrote output");
    if let Some(markdown_path) = markdown_path {
        let markdown = report::markdown(&config.comparison.title, std::slice::from_ref(&result))?;
        write_text_atomic(&markdown_path, &markdown)?;
        eprintln!("wrote Markdown output");
    }
    if report::has_failures(&result) {
        return Err("measurement failures were recorded".into());
    }
    Ok(())
}

fn sanitize_command(args: &[String]) -> Result<(), String> {
    reject_unknown_options(args, &["--output", "--markdown"], true)?;
    let inputs = positional_args(args, &["--output", "--markdown"]);
    if inputs.len() != 1 {
        return Err("sanitize requires exactly one input report".into());
    }
    let reports = report::load_reports(&inputs)?;
    let output = option_path(args, "--output").unwrap_or_else(|| "reports/sanitized.json".into());
    write_json_atomic(&output, &reports[0])?;
    if let Some(path) = option_path(args, "--markdown") {
        write_text_atomic(&path, &report::markdown(privacy::TITLE, &reports)?)?;
    }
    eprintln!("wrote sanitized output");
    Ok(())
}

fn report_command(args: &[String]) -> Result<(), String> {
    let output_path =
        option_path(args, "--output").unwrap_or_else(|| "reports/comparison.md".into());
    let title =
        option(args, "--title").unwrap_or("Uniterm vs Herdr performance and assurance report");
    reject_unknown_options(args, &["--output", "--title"], true)?;
    let inputs = positional_args(args, &["--output", "--title"]);
    if inputs.is_empty() {
        return Err("report requires one or more run JSON paths".into());
    }
    let reports = report::load_reports(&inputs)?;
    let markdown = report::markdown(title, &reports)?;
    write_text_atomic(&output_path, &markdown)?;
    eprintln!("wrote output");
    Ok(())
}

fn doctor_command(args: &[String]) -> Result<(), String> {
    let config_path = option_path(args, "--config").unwrap_or_else(|| "comparison.toml".into());
    reject_unknown_options(args, &["--config"], false)?;
    let config = Config::load(&config_path)?;
    let mut failures = Vec::new();
    for command in ["ps", "git", "uname"] {
        if !command_available(command) {
            failures.push(format!("required command {command:?} is unavailable"));
        }
    }
    if !Path::new("/bin/sh").is_file() {
        failures.push("required POSIX shell /bin/sh is unavailable".into());
    }
    for contender in &config.contenders {
        match executable_version(contender) {
            Ok(version) => println!(
                "ok  {}",
                privacy::product_version(&contender.adapter, &version)
            ),
            Err(error) => failures.push(format!("{}: {error}", contender.id)),
        }
    }
    println!(
        "ok  host         {} / {}{}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        if std::env::var_os("WSL_DISTRO_NAME").is_some() {
            " / WSL"
        } else {
            ""
        }
    );
    if failures.is_empty() {
        println!("doctor passed");
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn option<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|index| args.get(index + 1))
        .map(String::as_str)
}

fn option_path(args: &[String], name: &str) -> Option<PathBuf> {
    option(args, name).map(PathBuf::from)
}

fn positional_args(args: &[String], valued_options: &[&str]) -> Vec<PathBuf> {
    let mut values = Vec::new();
    let mut index = 0;
    while index < args.len() {
        if valued_options.contains(&args[index].as_str()) {
            index += 2;
        } else if !args[index].starts_with('-') {
            values.push(PathBuf::from(&args[index]));
            index += 1;
        } else {
            index += 1;
        }
    }
    values
}

fn reject_unknown_options(
    args: &[String],
    valued_options: &[&str],
    allow_positionals: bool,
) -> Result<(), String> {
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if valued_options.contains(&arg.as_str()) {
            if args
                .get(index + 1)
                .is_none_or(|value| value.starts_with('-'))
            {
                return Err(format!("{arg} requires a value"));
            }
            index += 2;
        } else if arg.starts_with('-') {
            return Err(format!("unknown option {arg:?}"));
        } else if allow_positionals {
            index += 1;
        } else {
            return Err(format!("unexpected positional argument {arg:?}"));
        }
    }
    Ok(())
}

fn command_available(name: &str) -> bool {
    std::process::Command::new(name)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

fn write_json_atomic(path: &Path, value: &impl serde::Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    write_bytes_atomic(path, &bytes)
}

fn write_text_atomic(path: &Path, value: &str) -> Result<(), String> {
    write_bytes_atomic(path, value.as_bytes())
}

fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    for _ in 0..128 {
        let temp = path.with_file_name(format!(
            ".ut-compare-{}-{}.tmp",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = match options.open(&temp) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err("could not create output temporary file".into()),
        };
        let result = file
            .write_all(bytes)
            .and_then(|()| file.sync_all())
            .and_then(|()| std::fs::rename(&temp, path));
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        return result.map_err(|_| "could not write or replace output file".into());
    }
    Err("could not create output temporary file".into())
}

fn print_help() {
    println!(
        "ut-compare {} - fair terminal multiplexer performance and assurance comparison\n\n\
USAGE:\n\
  ut-compare doctor [--config comparison.toml]\n\
  ut-compare run [--config comparison.toml] [--profile smoke|standard|marketing]\n\
                 [--output reports/run.json] [--markdown reports/run.md] [--seed N]\n\
  ut-compare sanitize INPUT.json [--output OUTPUT.json] [--markdown OUTPUT.md]\n\
  ut-compare report [--title TEXT] [--output reports/comparison.md] RUN.json...\n\n\
Outputs omit local identities, paths, custom text and raw diagnostics.\n\
The run command uses isolated HOME/XDG directories and real pseudo-terminals.\n\
Use the marketing profile for publishable idle CPU measurements; use smoke only\n\
to validate a host and binary pair. Merge native Linux, WSL, and macOS run JSON\n\
files with the report command.",
        env!("CARGO_PKG_VERSION")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn atomic_output_is_private_and_does_not_follow_destination_symlinks() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let workdir = ut_compare::adapters::IsolatedWorkdir::create("output-test", false).unwrap();
        let victim = workdir.root.join("original");
        let output = workdir.root.join("output.json");
        std::fs::write(&victim, "original").unwrap();
        symlink(&victim, &output).unwrap();
        write_bytes_atomic(&output, b"sanitized").unwrap();
        assert_eq!(std::fs::read_to_string(victim).unwrap(), "original");
        assert_eq!(std::fs::read_to_string(&output).unwrap(), "sanitized");
        assert_eq!(
            std::fs::metadata(output).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn failed_atomic_output_removes_temporary_file() {
        let workdir = ut_compare::adapters::IsolatedWorkdir::create("output-test", false).unwrap();
        let output = workdir.root.join("existing-directory");
        std::fs::create_dir(&output).unwrap();
        let before = std::fs::read_dir(&workdir.root).unwrap().count();
        assert!(write_bytes_atomic(&output, b"sanitized").is_err());
        assert_eq!(std::fs::read_dir(&workdir.root).unwrap().count(), before);
    }

    #[test]
    fn positional_report_inputs_skip_option_values() {
        let args = vec![
            "--output".into(),
            "report.md".into(),
            "linux.json".into(),
            "mac.json".into(),
        ];
        assert_eq!(
            positional_args(&args, &["--output"]),
            vec![PathBuf::from("linux.json"), PathBuf::from("mac.json")]
        );
    }
}

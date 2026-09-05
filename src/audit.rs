use crate::adapters::executable_version;
use crate::config::ContenderConfig;
use crate::model::{ArtifactInfo, SourceInfo, StaticAnalysis};
use crate::process::checked_output;
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::io::Read;
use std::path::Path;
use std::process::Command;

pub fn artifact(config: &ContenderConfig) -> Result<ArtifactInfo, String> {
    let metadata = std::fs::metadata(&config.binary)
        .map_err(|error| format!("could not stat {}: {error}", config.binary.display()))?;
    Ok(ArtifactInfo {
        path: config.binary.display().to_string(),
        bytes: metadata.len(),
        version_output: executable_version(config)?,
        sha256: sha256(&config.binary)?,
    })
}

pub fn source_info(config: &ContenderConfig) -> SourceInfo {
    let cargo = read_cargo_package(&config.source);
    SourceInfo {
        path: config.source.display().to_string(),
        commit: git(&config.source, &["rev-parse", "HEAD"]).ok(),
        commit_date: git(&config.source, &["log", "-1", "--format=%cI"]).ok(),
        dirty: git(&config.source, &["status", "--porcelain"])
            .ok()
            .map(|output| !output.trim().is_empty()),
        package_version: cargo.as_ref().and_then(|value| {
            package_string(value, "version").or_else(|| workspace_package_string(value, "version"))
        }),
        license: cargo.as_ref().and_then(|value| {
            package_string(value, "license").or_else(|| workspace_package_string(value, "license"))
        }),
    }
}

pub fn static_analysis(root: &Path) -> StaticAnalysis {
    let mut analysis = StaticAnalysis::default();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        if path != root && excluded_path(root, &path) {
            continue;
        }
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&path) {
                pending.extend(entries.flatten().map(|entry| entry.path()));
            }
            continue;
        }
        let extension = path.extension().and_then(OsStr::to_str).unwrap_or("");
        if extension == "rs" {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            *analysis.rust_source_files.get_or_insert(0) += 1;
            let is_test_file = path.strip_prefix(root).ok().is_some_and(|relative| {
                relative
                    .components()
                    .any(|part| part.as_os_str() == "tests")
            });
            if is_test_file {
                *analysis.rust_test_lines.get_or_insert(0) += line_count(&text);
                continue;
            }
            let (production, tests) = split_cfg_tests(&text);
            *analysis.first_party_rust_lines.get_or_insert(0) += line_count(production);
            *analysis.rust_test_lines.get_or_insert(0) += line_count(tests);
            *analysis.unsafe_blocks.get_or_insert(0) +=
                count_occurrences(production, "unsafe {") as u64;
            *analysis.production_unwrap_calls.get_or_insert(0) +=
                count_occurrences(production, ".unwrap()") as u64;
            *analysis.network_api_references.get_or_insert(0) +=
                ["TcpListener", "TcpStream", "reqwest", "http://", "https://"]
                    .iter()
                    .map(|needle| count_occurrences(production, needle) as u64)
                    .sum::<u64>();
            *analysis.process_launch_references.get_or_insert(0) +=
                count_occurrences(production, "Command::new") as u64;
        } else if matches!(extension, "md" | "mdx") && documentation_path(root, &path) {
            if let Ok(text) = std::fs::read_to_string(&path) {
                analysis.documentation_lines += line_count(&text);
            }
        }
    }

    let lock = root.join("Cargo.lock");
    if let Ok(text) = std::fs::read_to_string(lock) {
        analysis.lockfile_packages =
            Some(text.lines().filter(|line| *line == "[[package]]").count() as u64);
    }
    if let Some(cargo) = read_cargo_package(root) {
        analysis.direct_dependencies = Some(
            cargo
                .get("dependencies")
                .and_then(toml::Value::as_table)
                .or_else(|| {
                    cargo
                        .get("workspace")
                        .and_then(|workspace| workspace.get("dependencies"))
                        .and_then(toml::Value::as_table)
                })
                .map(|table| table.len() as u64)
                .unwrap_or(0),
        );
    }
    // Null means the language-specific collector does not apply, never a safety finding.
    if analysis.rust_source_files.is_some() {
        analysis.rust_source_files.get_or_insert(0);
        analysis.first_party_rust_lines.get_or_insert(0);
        analysis.rust_test_lines.get_or_insert(0);
        analysis.unsafe_blocks.get_or_insert(0);
        analysis.production_unwrap_calls.get_or_insert(0);
        analysis.network_api_references.get_or_insert(0);
        analysis.process_launch_references.get_or_insert(0);
    }
    analysis.notes = vec![
        "Counts exclude .git, target, vendored code, generated bindings, translations, and preview/versioned duplicate docs.".into(),
        "Unsafe and unwrap counts are lexical heuristics over first-party production prefixes, not vulnerability findings.".into(),
        "Code size and dependency count are context metrics and do not contribute to assurance scores.".into(),
    ];
    if analysis.rust_source_files.is_none() {
        analysis.notes.push("Rust lexical metrics are not applicable to this source tree. Cargo counts cover only Cargo manifests, not C dependencies. Documentation counts cover Markdown only, not man pages.".into());
    }
    analysis
}

fn split_cfg_tests(text: &str) -> (&str, &str) {
    if let Some(index) = text.find("\n#[cfg(test)]") {
        (&text[..index], &text[index..])
    } else {
        (text, "")
    }
}

fn excluded_path(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return true;
    };
    relative.components().any(|component| {
        matches!(
            component.as_os_str().to_str(),
            Some(".git" | "target" | "vendor" | "node_modules" | "dist" | "zig-out")
        )
    }) || relative.starts_with("docs/preview")
        || relative.starts_with("docs/versions")
        || relative.starts_with("docs/next/README.md")
        || relative.starts_with("docs/next/CHANGELOG.md")
        || relative.ends_with("src/ghostty/bindings.rs")
        || translated_documentation(relative)
}

/// Translated documentation trees and files (`.../docs/ja/...`, `.../docs/zh-cn/...`,
/// `README.zh-CN.md`) are excluded so English documentation counts stay comparable.
fn translated_documentation(relative: &Path) -> bool {
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        if component.as_os_str() == "docs" {
            if let Some(next) = components.peek() {
                if matches!(next.as_os_str().to_str(), Some("ja" | "zh-cn" | "zh-CN")) {
                    return true;
                }
            }
        }
    }
    relative
        .file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.contains(".zh-CN.") || name.contains(".ja."))
}

fn documentation_path(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    !relative.starts_with(".github") && !relative.starts_with(".local")
}

fn line_count(text: &str) -> u64 {
    text.lines().count() as u64
}

fn count_occurrences(text: &str, needle: &str) -> usize {
    text.match_indices(needle).count()
}

fn sha256(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path)
        .map_err(|error| format!("could not open {}: {error}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("could not hash {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    checked_output(
        Command::new("git")
            .args(["-C", &root.display().to_string()])
            .args(args),
    )
}

fn read_cargo_package(root: &Path) -> Option<toml::Value> {
    let text = std::fs::read_to_string(root.join("Cargo.toml")).ok()?;
    toml::from_str(&text).ok()
}

fn package_string(value: &toml::Value, key: &str) -> Option<String> {
    value.get("package")?.get(key)?.as_str().map(str::to_owned)
}

fn workspace_package_string(value: &toml::Value, key: &str) -> Option<String> {
    value
        .get("workspace")?
        .get("package")?
        .get(key)?
        .as_str()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_rust_counts_are_null_while_measured_rust_zeroes_are_zero() {
        let tree = crate::adapters::IsolatedWorkdir::create("audit", false).unwrap();
        std::fs::write(tree.root.join("main.c"), "int main(void) { return 0; }\n").unwrap();
        let c = static_analysis(&tree.root);
        assert_eq!(c.unsafe_blocks, None);
        assert_eq!(c.direct_dependencies, None);
        assert_eq!(
            serde_json::to_value(&c).unwrap()["unsafe_blocks"],
            serde_json::Value::Null
        );
        std::fs::write(tree.root.join("main.rs"), "fn main() {}\n").unwrap();
        let rust = static_analysis(&tree.root);
        assert_eq!(rust.unsafe_blocks, Some(0));
        assert_eq!(rust.rust_test_lines, Some(0));
        assert_eq!(rust.first_party_rust_lines, Some(1));
    }

    #[test]
    fn cfg_test_suffix_is_excluded_from_production() {
        let source = "fn production() { unsafe { call(); } }\n#[cfg(test)]\nmod tests { fn test() { x.unwrap(); } }";
        let (production, tests) = split_cfg_tests(source);
        assert!(production.contains("unsafe"));
        assert!(!production.contains("unwrap"));
        assert!(tests.contains("unwrap"));
    }

    #[test]
    fn translated_and_versioned_docs_are_excluded_symmetrically() {
        let root = Path::new("/repo");
        assert!(excluded_path(
            root,
            &root.join("docs/next/website/src/content/docs/ja/x.mdx")
        ));
        assert!(excluded_path(
            root,
            &root.join("docs/versions/0.8.2/website/src/x.mdx")
        ));
        assert!(excluded_path(root, &root.join("docs/preview/README.md")));
        assert!(excluded_path(root, &root.join("README.zh-CN.md")));
        assert!(!excluded_path(
            root,
            &root.join("docs/next/website/src/content/docs/x.mdx")
        ));
        assert!(!excluded_path(
            root,
            &root.join("docs/03-system-architecture.md")
        ));
        assert!(!excluded_path(
            root,
            &root.join("skills/manage-uniterm/SKILL.md")
        ));
    }

    #[test]
    fn occurrence_count_does_not_overlap() {
        assert_eq!(count_occurrences("aaaa", "aa"), 2);
    }
}

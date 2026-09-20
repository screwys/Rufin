use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::process::{quoted_value, read_to_string, repo_root, temp_path, write_string};
use crate::{Result, parse_check_flag};

const CARGO_REGISTRY_SOURCE: &str = "registry+https://github.com/rust-lang/crates.io-index";

pub(crate) fn run(mut args: Vec<String>) -> Result<()> {
    if args.is_empty() {
        return Err("missing generate command".into());
    }

    match args.remove(0).as_str() {
        "flatpak-sources" => flatpak_sources_command(args),
        "i18n-template" => i18n_template_command(args),
        "windows-installer-languages" => crate::windows_i18n::command(args),
        "windows-installer-files" => crate::windows_installer::files(args),
        "linux-packaging" => crate::linux_packaging::command(args),
        "media-verification-files" => crate::media::verification_files_command(args),
        "rpm-srpm" => crate::rpm::srpm_command(args),
        command => Err(format!("unknown generate command: {command}").into()),
    }
}

fn flatpak_sources_command(args: Vec<String>) -> Result<()> {
    let Some(check) = parse_check_flag(
        args,
        "Usage: cargo run --locked -p xtask -- generate flatpak-sources [--check]",
    )?
    else {
        return Ok(());
    };
    flatpak_sources(check)
}

pub(crate) fn flatpak_sources(check: bool) -> Result<()> {
    let root = repo_root()?;
    let lock_file = root.join("Cargo.lock");
    let sources_file = root.join("packaging/flatpak/cargo-sources.json");
    let generated = generate_cargo_sources(&root, &read_to_string(&lock_file)?)?;

    if check {
        let current = read_to_string(&sources_file)?;
        if current != generated {
            return Err(
                "packaging/flatpak/cargo-sources.json is stale; run cargo run --locked -p xtask -- generate flatpak-sources"
                    .into(),
            );
        }
        return Ok(());
    }

    write_string(&sources_file, &generated)?;
    Ok(())
}

#[derive(Clone, Default)]
struct CargoPackage {
    name: String,
    version: String,
    source: String,
    checksum: String,
}

fn generate_cargo_sources(root: &Path, lock: &str) -> Result<String> {
    let mut output = String::from("[\n");
    let mut current = CargoPackage::default();
    let mut in_package = false;
    let mut seen = HashSet::new();
    let mut git_packages = Vec::new();

    for line in lock.lines() {
        if line == "[[package]]" {
            if in_package {
                flush_cargo_package(&current, &mut seen, &mut output)?;
                if current.source.starts_with("git+") {
                    git_packages.push(current.clone());
                }
            }
            current = CargoPackage::default();
            in_package = true;
            continue;
        }

        if !in_package {
            continue;
        }

        if let Some(value) = quoted_value(line, "name") {
            current.name = value;
        } else if let Some(value) = quoted_value(line, "version") {
            current.version = value;
        } else if let Some(value) = quoted_value(line, "source") {
            current.source = value;
        } else if let Some(value) = quoted_value(line, "checksum") {
            current.checksum = value;
        }
    }

    if in_package {
        flush_cargo_package(&current, &mut seen, &mut output)?;
        if current.source.starts_with("git+") {
            git_packages.push(current.clone());
        }
    }

    let config = if git_packages.is_empty() {
        "[source.vendored-sources]\ndirectory = \"cargo/vendor\"\n\n[source.crates-io]\nreplace-with = \"vendored-sources\"\n".to_owned()
    } else {
        append_git_sources(root, &git_packages, &mut output)?
    };

    output.push_str("    {\n");
    output.push_str("        \"type\": \"inline\",\n");
    output.push_str(&format!(
        "        \"contents\": {},\n",
        serde_json::to_string(&config)?
    ));
    output.push_str("        \"dest\": \"cargo\",\n");
    output.push_str("        \"dest-filename\": \"config\"\n");
    output.push_str("    }\n");
    output.push_str("]\n");
    Ok(output)
}

/// Cargo owns workspace inheritance and git manifest normalization. Flatpak's
/// offline inputs retain the locked checkout and use those normalized manifests.
fn append_git_sources(
    root: &Path,
    packages: &[CargoPackage],
    output: &mut String,
) -> Result<String> {
    let metadata = Command::new("cargo")
        .current_dir(root)
        .args(["metadata", "--locked", "--format-version=1"])
        .output()?;
    if !metadata.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&metadata.stderr)
        )
        .into());
    }
    let metadata: serde_json::Value = serde_json::from_slice(&metadata.stdout)?;
    let temporary = tempfile::tempdir()?;
    let vendor = temporary.path().join("vendor");
    let vendored = Command::new("cargo")
        .current_dir(root)
        .args(["vendor", "--locked", "--versioned-dirs"])
        .arg(&vendor)
        .stderr(Stdio::inherit())
        .output()?;
    if !vendored.status.success() {
        return Err("cargo vendor failed while normalizing git sources".into());
    }
    let config = String::from_utf8(vendored.stdout)?
        .lines()
        .map(|line| {
            if line.starts_with("directory = ") {
                "directory = \"cargo/vendor\""
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let mut repositories = HashSet::new();
    for package in packages {
        let (repository, commit) = locked_git_source(&package.source)?;
        let cache = format!("cargo/git/{commit}");
        if repositories.insert(package.source.clone()) {
            append_source(
                output,
                serde_json::json!({"type":"git","url":repository,"commit":commit,"dest":cache}),
            )?;
        }
        let entry = metadata["packages"]
            .as_array()
            .ok_or("cargo metadata has no packages")?
            .iter()
            .find(|entry| {
                entry["name"].as_str() == Some(&package.name)
                    && entry["version"].as_str() == Some(&package.version)
                    && entry["source"].as_str() == Some(&package.source)
            })
            .ok_or_else(|| format!("cargo metadata has no locked git package {}", package.name))?;
        let manifest = Path::new(
            entry["manifest_path"]
                .as_str()
                .ok_or("git package has no manifest path")?,
        );
        let checkout = Command::new("git")
            .current_dir(manifest.parent().ok_or("git manifest has no parent")?)
            .args(["rev-parse", "--show-toplevel"])
            .output()?;
        if !checkout.status.success() {
            return Err("could not locate cargo git checkout".into());
        }
        let checkout = String::from_utf8(checkout.stdout)?;
        let relative = manifest
            .parent()
            .ok_or("git manifest has no parent")?
            .strip_prefix(checkout.trim())?;
        let relative = relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        let destination = format!("cargo/vendor/{}-{}", package.name, package.version);
        append_source(
            output,
            serde_json::json!({"type":"shell","commands":[format!("mkdir -p cargo/vendor && cp -a {} {}",shell_quote(&format!("{cache}/{relative}")),shell_quote(&destination))]}),
        )?;
        append_source(
            output,
            serde_json::json!({"type":"inline","contents":fs::read_to_string(vendor.join(format!("{}-{}",package.name,package.version)).join("Cargo.toml"))?,"dest":destination,"dest-filename":"Cargo.toml"}),
        )?;
        append_source(
            output,
            serde_json::json!({"type":"inline","contents":"{\"package\":null,\"files\":{}}","dest":destination,"dest-filename":".cargo-checksum.json"}),
        )?;
    }
    Ok(config)
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn append_source(output: &mut String, value: serde_json::Value) -> Result<()> {
    for line in serde_json::to_string_pretty(&value)?.lines() {
        output.push_str("    ");
        output.push_str(line);
        output.push('\n');
    }
    output.pop();
    output.push_str(",\n");
    Ok(())
}

pub(crate) fn locked_git_source(source: &str) -> Result<(&str, &str)> {
    let (url, commit) = source
        .strip_prefix("git+")
        .and_then(|source| source.rsplit_once('#'))
        .ok_or("git source has no locked commit")?;
    if commit.len() != 40 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("git source commit is not a full object ID".into());
    }
    Ok((url.split_once('?').map_or(url, |(url, _)| url), commit))
}

fn flush_cargo_package(
    package: &CargoPackage,
    seen: &mut HashSet<(String, String, String)>,
    output: &mut String,
) -> Result<()> {
    if package.source != CARGO_REGISTRY_SOURCE {
        return Ok(());
    }

    if package.checksum.is_empty() {
        return Err(format!("missing checksum for {} {}", package.name, package.version).into());
    }

    let key = (
        package.name.clone(),
        package.version.clone(),
        package.checksum.clone(),
    );
    if !seen.insert(key) {
        return Ok(());
    }

    let dest = format!("cargo/vendor/{}-{}", package.name, package.version);
    output.push_str("    {\n");
    output.push_str("        \"type\": \"archive\",\n");
    output.push_str("        \"archive-type\": \"tar-gzip\",\n");
    output.push_str(&format!(
        "        \"url\": \"https://static.crates.io/crates/{name}/{name}-{version}.crate\",\n",
        name = package.name,
        version = package.version
    ));
    output.push_str(&format!("        \"sha256\": \"{}\",\n", package.checksum));
    output.push_str(&format!("        \"dest\": \"{dest}\"\n"));
    output.push_str("    },\n");
    output.push_str("    {\n");
    output.push_str("        \"type\": \"inline\",\n");
    output.push_str(&format!(
        "        \"contents\": \"{{\\\"package\\\": \\\"{}\\\", \\\"files\\\": {{}}}}\",\n",
        package.checksum
    ));
    output.push_str(&format!("        \"dest\": \"{dest}\",\n"));
    output.push_str("        \"dest-filename\": \".cargo-checksum.json\"\n");
    output.push_str("    },\n");
    Ok(())
}

fn i18n_template_command(mut args: Vec<String>) -> Result<()> {
    let mut check = false;
    let mut output = PathBuf::from("locales/rufin.pot");

    while !args.is_empty() {
        match args.remove(0).as_str() {
            "--check" => check = true,
            "--output" => {
                if args.is_empty() {
                    return Err("--output requires a path".into());
                }
                output = PathBuf::from(args.remove(0));
            }
            "-h" | "--help" => {
                eprintln!(
                    "Usage: cargo run --locked -p xtask -- generate i18n-template [--check] [--output PATH]"
                );
                return Ok(());
            }
            arg => return Err(format!("unexpected argument: {arg}").into()),
        }
    }

    if check {
        i18n_template_check()
    } else {
        i18n_template_to(&output)
    }
}

pub(crate) fn i18n_template_check() -> Result<()> {
    let root = repo_root()?;
    let output = temp_path("i18n-template.pot");
    i18n_template_to(&output)?;
    let generated = read_to_string(&output)?;
    let checked_in = read_to_string(&root.join("locales/rufin.pot"))?;
    let _ = fs::remove_file(&output);
    if checked_in == generated {
        Ok(())
    } else {
        Err(
            "locales/rufin.pot is stale; run cargo run --locked -p xtask -- generate i18n-template"
                .into(),
        )
    }
}

pub(crate) fn i18n_template_to(output: &Path) -> Result<()> {
    let root = repo_root()?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    write_string(output, &crate::i18n::template(&root)?)
}

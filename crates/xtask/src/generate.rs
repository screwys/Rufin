use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

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
    let generated = generate_cargo_sources(&read_to_string(&lock_file)?)?;

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

#[derive(Default)]
struct CargoPackage {
    name: String,
    version: String,
    source: String,
    checksum: String,
}

fn generate_cargo_sources(lock: &str) -> Result<String> {
    let mut output = String::from("[\n");
    let mut current = CargoPackage::default();
    let mut in_package = false;
    let mut seen = HashSet::new();

    for line in lock.lines() {
        if line == "[[package]]" {
            if in_package {
                flush_cargo_package(&current, &mut seen, &mut output)?;
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
    }

    output.push_str("    {\n");
    output.push_str("        \"type\": \"inline\",\n");
    output.push_str(
        "        \"contents\": \"[source.vendored-sources]\\ndirectory = \\\"cargo/vendor\\\"\\n\\n[source.crates-io]\\nreplace-with = \\\"vendored-sources\\\"\\n\",\n",
    );
    output.push_str("        \"dest\": \"cargo\",\n");
    output.push_str("        \"dest-filename\": \"config\"\n");
    output.push_str("    }\n");
    output.push_str("]\n");
    Ok(output)
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

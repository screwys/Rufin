use std::collections::HashSet;
use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use csv::{ReaderBuilder, Terminator, WriterBuilder};
use vibrato_rkyv::{Dictionary, SystemDictionaryBuilder};

use crate::process::{quoted_value, read_to_string, repo_root, temp_path, write_string};
use crate::{Result, parse_check_flag};

const CARGO_REGISTRY_SOURCE: &str = "registry+https://github.com/rust-lang/crates.io-index";
const JAPANESE_READINGS_PATH: &str = "data/japanese-readings.dic";

pub(crate) fn run(mut args: Vec<String>) -> Result<()> {
    if args.is_empty() {
        return Err("missing generate command".into());
    }

    match args.remove(0).as_str() {
        "flatpak-sources" => flatpak_sources_command(args),
        "i18n-template" => i18n_template_command(args),
        "japanese-readings" => japanese_readings_command(args),
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

fn japanese_readings_command(mut args: Vec<String>) -> Result<()> {
    let usage = "Usage: cargo run --locked -p xtask -- generate japanese-readings SOURCE [--check]";
    let mut source = None;
    let mut check = false;
    while !args.is_empty() {
        match args.remove(0).as_str() {
            "-h" | "--help" => {
                eprintln!("{usage}");
                return Ok(());
            }
            "--check" => check = true,
            arg if arg.starts_with('-') => return Err(format!("unknown option: {arg}").into()),
            arg if source.is_none() => source = Some(PathBuf::from(arg)),
            arg => return Err(format!("unexpected argument: {arg}").into()),
        }
    }
    let source = source.ok_or("generate japanese-readings requires an extracted IPADIC source")?;
    generate_japanese_readings(&source, check)
}

fn generate_japanese_readings(source: &Path, check: bool) -> Result<()> {
    for filename in ["matrix.def", "char.def", "unk.def"] {
        let path = source.join(filename);
        if !path.is_file() {
            return Err(format!("IPADIC source is missing {}", path.display()).into());
        }
    }

    let mut lexicon_paths = fs::read_dir(source)?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("csv"))
        .collect::<Vec<_>>();
    lexicon_paths.sort();
    if lexicon_paths.is_empty() {
        return Err(format!("IPADIC source has no CSV files: {}", source.display()).into());
    }

    let mut lexicon = Vec::new();
    {
        let mut writer = WriterBuilder::new()
            .has_headers(false)
            .terminator(Terminator::Any(b'\n'))
            .from_writer(&mut lexicon);
        for path in lexicon_paths {
            let mut reader = ReaderBuilder::new()
                .has_headers(false)
                .flexible(true)
                .from_path(&path)?;
            for record in reader.records() {
                let record = record?;
                if record.len() <= 11 {
                    return Err(format!("{} has an incomplete IPADIC row", path.display()).into());
                }
                writer.write_record([
                    &record[0],
                    &record[1],
                    &record[2],
                    &record[3],
                    &record[11],
                ])?;
            }
        }
        writer.flush()?;
    }

    let mut unknown = Vec::new();
    {
        let mut reader = ReaderBuilder::new()
            .has_headers(false)
            .flexible(true)
            .from_path(source.join("unk.def"))?;
        let mut writer = WriterBuilder::new()
            .has_headers(false)
            .terminator(Terminator::Any(b'\n'))
            .from_writer(&mut unknown);
        for record in reader.records() {
            let record = record?;
            if record.len() < 4 {
                return Err(
                    format!("{} has an incomplete row", source.join("unk.def").display()).into(),
                );
            }
            writer.write_record([&record[0], &record[1], &record[2], &record[3], "*"])?;
        }
        writer.flush()?;
    }

    let dictionary = SystemDictionaryBuilder::from_readers(
        lexicon.as_slice(),
        BufReader::new(fs::File::open(source.join("matrix.def"))?),
        BufReader::new(fs::File::open(source.join("char.def"))?),
        unknown.as_slice(),
    )?;
    let mut generated = Vec::new();
    Dictionary::from_inner(dictionary).write(&mut generated)?;

    let output = repo_root()?.join(JAPANESE_READINGS_PATH);
    if check {
        let current = fs::read(&output)
            .map_err(|error| format!("failed to read {}: {error}", output.display()))?;
        if current != generated {
            return Err(format!(
                "{} is stale; regenerate it from the pinned IPADIC source",
                output.display()
            )
            .into());
        }
        return Ok(());
    }

    fs::write(&output, generated)
        .map_err(|error| format!("failed to write {}: {error}", output.display()))?;
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

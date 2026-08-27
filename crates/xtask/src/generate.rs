use std::collections::HashSet;
use std::fs;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use csv::{ReaderBuilder, Terminator, WriterBuilder};
use vibrato_rkyv::{Dictionary, SystemDictionaryBuilder};

use crate::process::{
    collect_files_with_extension, ensure_command, path_to_slash, quoted_value, read_to_string,
    repo_root, temp_path, write_string,
};
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
    ensure_command("xgettext")?;
    let root = repo_root()?;
    let tmp_dir = root.join("target/tmp");
    fs::create_dir_all(&tmp_dir)?;
    let sources = tmp_dir.join(format!("i18n-sources-{}.txt", std::process::id()));
    let entries = tmp_dir.join(format!("i18n-entries-{}.pot", std::process::id()));

    let result = write_i18n_template(&root, &sources, &entries, output);
    let _ = fs::remove_file(&sources);
    let _ = fs::remove_file(&entries);
    result
}

fn write_i18n_template(root: &Path, sources: &Path, entries: &Path, output: &Path) -> Result<()> {
    let mut rust_files = Vec::new();
    collect_files_with_extension(root, &root.join("crates"), "rs", &mut rust_files)?;
    rust_files.sort();

    let mut source_list = fs::File::create(sources)?;
    for file in rust_files {
        writeln!(source_list, "{}", path_to_slash(&file))?;
    }

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }

    let status = Command::new("xgettext")
        .current_dir(root)
        .args([
            "--from-code=UTF-8",
            "--language=Rust",
            "--escape",
            "--no-location",
            "--package-name=Rufin",
            "--msgid-bugs-address=https://github.com/screwys/Rufin/issues",
            "--keyword=tr:1",
            "--keyword=tr_with:1",
            "--keyword=trn:1,2",
            "--keyword=trn_with:1,2",
            "--keyword=msgid:1",
            "--keyword=text_button:2",
            "--keyword=icon_button:2",
            "--keyword=icon_button_without_tooltip:2",
            "--keyword=detail_action_button:2",
            "--keyword=detail_link_button:2",
            "--keyword=toggle_button:2",
            "--keyword=row_button:2",
            "--keyword=cover_hover_controls:2",
            "--keyword=relocalize_icon_button:2",
            "--keyword=table_header_label:1",
            "--keyword=button_row:1",
            "--keyword=dialog_button:1",
            "--keyword=labeled_control:1",
            "--keyword=labeled_row:1",
            "--keyword=smart_playlist_dialog:1",
        ])
        .arg(format!("--files-from={}", sources.display()))
        .arg(format!("--output={}", entries.display()))
        .stdin(Stdio::inherit())
        .output()?;
    let stdout = String::from_utf8_lossy(&status.stdout);
    let stderr = String::from_utf8_lossy(&status.stderr);
    if !status.status.success() {
        return Err(format!(
            "xgettext failed with status {}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            status.status
        )
        .into());
    }
    if !stderr.trim().is_empty() {
        return Err(format!("xgettext emitted warnings:\n{stderr}").into());
    }

    let mut template = String::from(
        "# Rufin translation template.\n# Copyright (C) 2026 Rufin contributors\n# This file is distributed under the same license as the Rufin package.\n#\n#, fuzzy\nmsgid \"\"\nmsgstr \"\"\n\"Project-Id-Version: Rufin\\n\"\n\"Report-Msgid-Bugs-To: https://github.com/screwys/Rufin/issues\\n\"\n\"POT-Creation-Date: YEAR-MO-DA HO:MI+ZONE\\n\"\n\"PO-Revision-Date: YEAR-MO-DA HO:MI+ZONE\\n\"\n\"Last-Translator: Rufin translators\\n\"\n\"Language-Team: Rufin translators\\n\"\n\"Language: \\n\"\n\"MIME-Version: 1.0\\n\"\n\"Content-Type: text/plain; charset=UTF-8\\n\"\n\"Content-Transfer-Encoding: 8bit\\n\"\n",
    );
    if entries.metadata()?.len() > 0 {
        template.push('\n');
        template.push_str(&canonical_gettext_entries(strip_xgettext_header(
            &read_to_string(entries)?,
        )));
    }
    write_string(output, &template)
}

fn strip_xgettext_header(input: &str) -> &str {
    input
        .split_once("\n\n")
        .map_or(input, |(_, entries)| entries)
}

fn canonical_gettext_entries(input: &str) -> String {
    let mut entries = input
        .trim()
        .split("\n\n")
        .filter(|entry| !entry.is_empty())
        .map(|entry| (gettext_entry_sort_key(entry), entry))
        .collect::<Vec<_>>();
    entries.sort_by(|(left_key, left), (right_key, right)| {
        left_key.cmp(right_key).then_with(|| left.cmp(right))
    });
    if entries.is_empty() {
        return String::new();
    }
    let mut output = entries
        .into_iter()
        .map(|(_, entry)| entry)
        .collect::<Vec<_>>()
        .join("\n\n");
    output.push('\n');
    output
}

fn gettext_entry_sort_key(entry: &str) -> (String, String) {
    (
        gettext_field_key(entry, "msgctxt"),
        gettext_field_key(entry, "msgid"),
    )
}

fn gettext_field_key(entry: &str, field: &str) -> String {
    let prefix = format!("{field} ");
    let mut key = String::new();
    let mut collecting = false;
    for line in entry.lines() {
        if let Some(value) = line.strip_prefix(&prefix) {
            collecting = true;
            key.push_str(value);
        } else if collecting && line.starts_with('"') {
            key.push_str(line);
        } else if collecting {
            break;
        }
    }
    key
}

#[cfg(test)]
mod tests {
    use super::canonical_gettext_entries;

    #[test]
    fn gettext_entry_order_does_not_change_template() {
        let plural = "#, rust-format\n\
                      msgid \"{count} album\"\n\
                      msgid_plural \"{count} albums\"\n\
                      msgstr[0] \"\"\n\
                      msgstr[1] \"\"";
        let contextual = "msgctxt \"button\"\n\
                          msgid \"Play\"\n\
                          msgstr \"\"";
        let first = format!("{plural}\n\n{contextual}\n");
        let second = format!("{contextual}\n\n{plural}\n");

        assert_eq!(
            canonical_gettext_entries(&first),
            canonical_gettext_entries(&second)
        );
    }
}

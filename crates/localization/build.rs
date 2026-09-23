use std::{
    env,
    error::Error,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

type BuildResult<T> = Result<T, Box<dyn Error>>;

fn main() -> BuildResult<()> {
    let manifest_dir = cargo_env_path("CARGO_MANIFEST_DIR")?;
    let translation_dir = manifest_dir.join("../../locales");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", translation_dir.display());

    let po_files = translation_source_files(&translation_dir);
    write_translator_credits(&po_files)?;
    let build_locale_dir = compile_translation_catalogs(&po_files)?;
    println!(
        "cargo:rustc-env=RUFIN_BUILD_LOCALEDIR={}",
        build_locale_dir.display()
    );

    Ok(())
}

fn cargo_env_path(name: &str) -> BuildResult<PathBuf> {
    let value = env::var_os(name).ok_or_else(|| io::Error::other(format!("{name} is not set")))?;
    Ok(PathBuf::from(value))
}

fn compile_translation_catalogs(po_files: &[PathBuf]) -> BuildResult<PathBuf> {
    let out_dir = cargo_env_path("OUT_DIR")?;
    let locale_dir = out_dir.join("share/locale");

    if po_files.is_empty() {
        if locale_dir.is_dir() {
            fs::remove_dir_all(&locale_dir)?;
        }
        return Ok(locale_dir);
    }
    for po_file in po_files {
        println!("cargo:rerun-if-changed={}", po_file.display());
    }

    if !msgfmt_available() {
        println!("cargo:warning=msgfmt was not found; local .po translations will not be compiled");
        return Ok(locale_dir);
    }

    for po_file in po_files {
        let lang = po_file
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| io::Error::other(format!("{} has no UTF-8 stem", po_file.display())))?;
        let target_dir = locale_dir.join(lang).join("LC_MESSAGES");
        fs::create_dir_all(&target_dir)?;
        let target_file = target_dir.join("rufin.mo");
        let compiled = target_dir.join("rufin.mo.new");
        let status = Command::new("msgfmt")
            .arg("--check")
            .arg(po_file)
            .arg("-o")
            .arg(&compiled)
            .status();
        match status {
            Ok(status) if status.success() => {
                write_if_changed(&target_file, &fs::read(&compiled)?)?;
                fs::remove_file(compiled)?;
            }
            Ok(status) => {
                return Err(io::Error::other(format!(
                    "msgfmt failed for {} with status {status}",
                    po_file.display()
                ))
                .into());
            }
            Err(error) => {
                return Err(io::Error::other(format!(
                    "failed to run msgfmt for {}: {error}",
                    po_file.display()
                ))
                .into());
            }
        }
    }

    // A removed language must not remain available through an older build output.
    if locale_dir.is_dir() {
        for entry in fs::read_dir(&locale_dir)? {
            let entry = entry?;
            let language = entry.file_name();
            if entry.file_type()?.is_dir()
                && !po_files
                    .iter()
                    .any(|path| path.file_stem() == Some(language.as_os_str()))
            {
                fs::remove_dir_all(entry.path())?;
            }
        }
    }

    Ok(locale_dir)
}

fn write_translator_credits(po_files: &[PathBuf]) -> BuildResult<()> {
    let out_dir = cargo_env_path("OUT_DIR")?;
    let mut credits = Vec::new();
    for po_file in po_files {
        let Ok(text) = fs::read_to_string(po_file) else {
            continue;
        };
        let Some(translator) = po_header_value(&text, "Last-Translator") else {
            continue;
        };
        if translator == "Rufin translators" || credits.iter().any(|credit| credit == &translator) {
            continue;
        }
        credits.push(translator);
    }
    write_if_changed(
        &out_dir.join("translator_credits.txt"),
        credits.join("\n").as_bytes(),
    )?;
    Ok(())
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if fs::read(path).is_ok_and(|current| current == bytes) {
        return Ok(());
    }
    fs::write(path, bytes)
}

fn po_header_value(text: &str, key: &str) -> Option<String> {
    let prefix = format!("\"{key}: ");
    text.lines()
        .find_map(|line| line.trim().strip_prefix(&prefix))
        .and_then(|value| value.strip_suffix("\\n\""))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn translation_source_files(po_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(po_dir) else {
        return Vec::new();
    };
    let mut files = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("po"))
        .collect::<Vec<_>>();
    files.sort();
    files
}

fn msgfmt_available() -> bool {
    Command::new("msgfmt")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

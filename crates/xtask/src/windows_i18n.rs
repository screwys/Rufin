use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::Result;
use crate::process::{command_stdout, read_to_string, repo_root, write_string};

pub(crate) const CONTEXT: &str = "Windows installer";

pub(crate) fn source_strings(root: &Path) -> Result<BTreeMap<String, String>> {
    Ok(serde_json::from_str(&read_to_string(
        &root.join("packaging/windows/strings.json"),
    )?)?)
}

pub(crate) fn command(args: Vec<String>) -> Result<()> {
    let [locale_dir, compiler, output] = args.as_slice() else {
        return Err("Usage: cargo run --locked -p xtask -- generate windows-installer-languages LOCALE_DIR MAKENSIS OUTPUT".into());
    };
    let root = repo_root()?;
    let strings = source_strings(&root)?;
    let available = nsis_languages(compiler)?;
    let mut locales = fs::read_dir(root.join("locales"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|path| path.extension().is_some_and(|extension| extension == "po"))
        .map(|path| path.file_stem().unwrap().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    locales.sort();
    let language_ids = locale_language_ids(&locales, &available)?;
    let mut catalogs = BTreeMap::from([(1033, BTreeMap::new())]);
    for locale in locales {
        let Some(&id) = language_ids.get(&locale) else {
            eprintln!("No matching NSIS language for {locale}; omitting it from the installer.");
            continue;
        };
        let path = Path::new(locale_dir)
            .join(&locale)
            .join("LC_MESSAGES/rufin.mo");
        let translations = read_mo(&fs::read(&path).map_err(|error| {
            format!(
                "could not read installer translations {}: {error}",
                path.display()
            )
        })?)?;
        // NSIS allows one table per language ID. Regional catalogs refine a
        // shared language when Windows resolves them to the same ID.
        catalogs.entry(id).or_default().extend(translations);
    }
    let mut generated = String::from("; Generated from Rufin's gettext catalogs. Do not edit.\n");
    let english = catalogs.remove(&1033).unwrap();
    for (id, translations) in std::iter::once((1033, english)).chain(catalogs) {
        let language = &available[&id];
        writeln!(generated, "!insertmacro MUI_LANGUAGE \"{language}\"")?;
        generated.push_str(&language_strings(language, &strings, &translations)?);
    }
    write_string(Path::new(output), &generated)
}

fn nsis_languages(compiler: &str) -> Result<BTreeMap<u32, String>> {
    let flag = if cfg!(windows) {
        "/HDRINFO"
    } else {
        "-HDRINFO"
    };
    let info = command_stdout(compiler, [flag])?;
    let directory = info
        .split_once("NSISDIR=")
        .and_then(|(_, value)| value.split(',').next())
        .ok_or("makensis did not report NSISDIR")?;
    let mut languages = BTreeMap::new();
    for entry in fs::read_dir(Path::new(directory).join("Contrib/Language files"))? {
        let path = entry?.path();
        if path.extension().is_none_or(|extension| extension != "nlf") {
            continue;
        }
        let source = read_to_string(&path)?;
        let id = source
            .lines()
            .skip_while(|line| !line.contains("# Language ID"))
            .nth(1)
            .ok_or("NSIS language file has no language ID")?
            .trim()
            .parse()?;
        languages.insert(id, path.file_stem().unwrap().to_string_lossy().into_owned());
    }
    Ok(languages)
}

fn locale_language_ids(
    locales: &[String],
    available: &BTreeMap<u32, String>,
) -> Result<BTreeMap<String, u32>> {
    // Use Windows culture data rather than maintaining a gettext-to-LANGID
    // table. PowerShell Core also permits generation on other build hosts.
    let script = r#"
$ErrorActionPreference = 'Stop'
$data = [Console]::In.ReadToEnd() | ConvertFrom-Json
$result = @{}
foreach ($locale in $data.locales) {
    try {
        $culture = [Globalization.CultureInfo]::GetCultureInfo($locale.Replace('_', '-'))
        $id = [Globalization.CultureInfo]::CreateSpecificCulture($culture.Name).LCID
        if ($data.ids -notcontains $id) {
            $id = $data.ids | Where-Object {
                [Globalization.CultureInfo]::GetCultureInfo([int]$_).Parent.Name -eq $culture.Name
            } | Select-Object -First 1
        }
        if ($null -ne $id) { $result[$locale] = [int]$id }
    } catch [Globalization.CultureNotFoundException] { }
}
ConvertTo-Json -InputObject $result -Compress
"#;
    let shell = if cfg!(windows) {
        "powershell.exe"
    } else {
        "pwsh"
    };
    let mut process = Command::new(shell)
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let input =
        serde_json::json!({"locales": locales, "ids": available.keys().collect::<Vec<_>>()});
    process
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())?;
    let output = process.wait_with_output()?;
    if !output.status.success() {
        return Err("PowerShell could not resolve installer language IDs".into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn language_strings(
    language: &str,
    strings: &BTreeMap<String, String>,
    translations: &BTreeMap<String, String>,
) -> Result<String> {
    let mut output = String::new();
    for (id, english) in strings {
        let key = format!("{CONTEXT}\u{4}{english}");
        let translated = translations
            .get(&key)
            .filter(|value| !value.is_empty())
            .unwrap_or(english);
        writeln!(
            output,
            "LangString {id} ${{LANG_{}}} \"{}\"",
            language.to_uppercase(),
            nsis_string(translated)
        )?;
    }
    Ok(output)
}

fn nsis_string(value: &str) -> String {
    value
        .replace('$', "$$")
        .replace('"', "$\\\"")
        .replace("\r\n", "\n")
        .replace('\n', "$\\r$\\n")
        .replace('\t', "$\\t")
        .replace("{app_name}", "${RUFIN_DISPLAY_NAME}")
}

// GNU MO tables contain (byte length, byte offset) pairs for each original
// and translation. msgfmt excludes fuzzy and untranslated entries by default.
fn read_mo(bytes: &[u8]) -> Result<BTreeMap<String, String>> {
    let big_endian = match bytes.get(..4) {
        Some([0xde, 0x12, 0x04, 0x95]) => false,
        Some([0x95, 0x04, 0x12, 0xde]) => true,
        _ => return Err("invalid gettext MO header".into()),
    };
    let word = |offset: usize| -> Result<usize> {
        let value = bytes
            .get(offset..offset + 4)
            .ok_or("truncated gettext MO table")?
            .try_into()?;
        Ok(if big_endian {
            u32::from_be_bytes(value)
        } else {
            u32::from_le_bytes(value)
        } as usize)
    };
    let string = |offset: usize| -> Result<String> {
        let length = word(offset)?;
        let start = word(offset + 4)?;
        Ok(std::str::from_utf8(
            bytes
                .get(start..start + length)
                .ok_or("truncated gettext MO string")?,
        )?
        .to_owned())
    };
    let originals = word(12)?;
    let translations = word(16)?;
    (0..word(8)?)
        .map(|index| {
            Ok((
                string(originals + index * 8)?,
                string(translations + index * 8)?,
            ))
        })
        .collect()
}

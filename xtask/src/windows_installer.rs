use std::fs;
use std::path::Path;

use crate::Result;

pub(crate) fn files(args: Vec<String>) -> Result<()> {
    let [stage, output] = args.as_slice() else {
        return Err("windows-installer-files requires STAGE_DIR OUTPUT".into());
    };
    let root = Path::new(stage);
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    let mut directories = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let relative =
                crate::process::path_to_slash(path.strip_prefix(root)?).replace('/', "\\");
            if entry.file_type()?.is_dir() {
                directories.push(relative);
                pending.push(path);
            } else {
                files.push(relative);
            }
        }
    }
    files.sort();
    directories.sort();
    // Delete files first, then attempt child directories before their parents.
    // UTF-16LE matches NSIS FileReadUTF16LE, including non-ASCII filenames.
    let inventory = files
        .into_iter()
        .map(|path| format!("F{path}\r\n"))
        .chain(
            directories
                .into_iter()
                .rev()
                .map(|path| format!("D{path}\r\n")),
        )
        .collect::<String>();
    fs::write(
        output,
        inventory
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    )?;
    Ok(())
}

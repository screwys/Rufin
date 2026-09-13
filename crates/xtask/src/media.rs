use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::Result;

const WAV_FILE: &str = "package-check.wav";
const WAVPACK_FILE: &str = "package-check.wv";
const OPENMPT_FILE: &str = "package-check.mod";
const GME_FILE: &str = "package-check.vgm";
const MEDIA_FILES: &[&str] = &[
    "package-check.mp3",
    "package-check.flac",
    "package-check.m4a",
    "package-check.ogg",
    "package-check.opus",
    WAV_FILE,
    WAVPACK_FILE,
    "package-check.mka",
    "package-check.wma",
];
#[cfg(not(windows))]
const GST_LAUNCH: &str = "gst-launch-1.0";
#[cfg(windows)]
const GST_LAUNCH: &str = "gst-launch-1.0.exe";
const GST_MEDIA_FILES: &[(&str, &[&str])] = &[
    ("package-check.mp3", &["lamemp3enc"]),
    ("package-check.flac", &["flacenc"]),
    ("package-check.m4a", &["avenc_aac", "mp4mux"]),
    ("package-check.ogg", &["vorbisenc", "oggmux"]),
    ("package-check.opus", &["opusenc", "oggmux"]),
    (WAV_FILE, &["audio/x-raw,format=S16LE", "wavenc"]),
    ("package-check.mka", &["vorbisenc", "matroskamux"]),
    ("package-check.wma", &["avenc_wmav2", "asfmux"]),
];
const OPENMPT_GME_MEDIA_FILES: &[&str] = &[OPENMPT_FILE, GME_FILE];

pub(crate) fn playback_fixture(directory: &Path) -> Result<()> {
    run_command(
        directory,
        GST_LAUNCH,
        gst_arguments(&["wavenc"], "Björk – проверка.wav", 300),
    )
}

pub(crate) fn verification_files_command(args: Vec<String>) -> Result<()> {
    let usage = "Usage: cargo run --locked -p xtask -- generate media-verification-files OUTPUT [--with-openmpt-gme]";
    if matches!(args.as_slice(), [arg] if arg == "-h" || arg == "--help") {
        eprintln!("{usage}");
        return Ok(());
    }
    let (output, include_openmpt_gme) = match args.as_slice() {
        [output] => (output, false),
        [output, flag] if flag == "--with-openmpt-gme" => (output, true),
        _ => return Err(usage.into()),
    };
    generate_verification_files(&PathBuf::from(output), include_openmpt_gme)
}

fn generate_verification_files(output_directory: &Path, include_openmpt_gme: bool) -> Result<()> {
    fs::create_dir_all(output_directory)?;
    for filename in MEDIA_FILES.iter().chain(OPENMPT_GME_MEDIA_FILES) {
        let path = output_directory.join(filename);
        if path.exists() {
            fs::remove_file(&path)
                .map_err(|error| format!("failed to remove {}: {error}", path.display()))?;
        }
    }

    for (filename, pipeline) in GST_MEDIA_FILES {
        gst_file(output_directory, pipeline, filename)?;
    }

    run_command(output_directory, "wavpack", wavpack_arguments())?;
    if include_openmpt_gme {
        fs::write(output_directory.join(OPENMPT_FILE), openmpt_fixture())?;
        fs::write(output_directory.join(GME_FILE), gme_fixture())?;
    }

    let expected_files = MEDIA_FILES.iter().copied().chain(
        include_openmpt_gme
            .then_some(OPENMPT_GME_MEDIA_FILES)
            .into_iter()
            .flatten()
            .copied(),
    );
    for filename in expected_files {
        let path = output_directory.join(filename);
        let metadata = fs::metadata(&path).map_err(|error| {
            format!(
                "missing media verification file {}: {error}",
                path.display()
            )
        })?;
        if !metadata.is_file() || metadata.len() == 0 {
            return Err(format!("media verification file is empty: {}", path.display()).into());
        }
    }
    Ok(())
}

fn openmpt_fixture() -> Vec<u8> {
    const HEADER_LEN: usize = 1_084;
    const PATTERN_LEN: usize = 1_024;
    const SAMPLE_LEN: usize = 64;

    let mut module = vec![0; HEADER_LEN + PATTERN_LEN + SAMPLE_LEN];
    module[..19].copy_from_slice(b"Rufin package check");

    let sample_header = 20;
    module[sample_header..sample_header + 11].copy_from_slice(b"Square wave");
    module[sample_header + 22..sample_header + 24]
        .copy_from_slice(&((SAMPLE_LEN / 2) as u16).to_be_bytes());
    module[sample_header + 25] = 64;
    module[sample_header + 48..sample_header + 50]
        .copy_from_slice(&((SAMPLE_LEN / 2) as u16).to_be_bytes());

    module[950] = 1;
    module[951] = 0;
    module[1_080..HEADER_LEN].copy_from_slice(b"M.K.");

    for (row, period) in [(0, 428_u16), (16, 339), (32, 285), (48, 214)] {
        let note = HEADER_LEN + row * 16;
        module[note] = (period >> 8) as u8;
        module[note + 1] = period as u8;
        module[note + 2] = 0x10;
    }

    let sample = &mut module[HEADER_LEN + PATTERN_LEN..];
    for (index, value) in sample.iter_mut().enumerate() {
        *value = if index < SAMPLE_LEN / 2 { 96 } else { 160 };
    }
    module
}

fn gme_fixture() -> Vec<u8> {
    const HEADER_LEN: usize = 0x40;
    const FRAME_SAMPLES: u32 = 735;
    const FRAME_COUNT: u32 = 60;

    let mut commands = vec![0x50, 0x8e, 0x50, 0x0f, 0x50, 0x90];
    commands.extend(std::iter::repeat_n(0x62, (FRAME_COUNT / 2) as usize));
    commands.extend([0x50, 0x87, 0x50, 0x0c]);
    commands.extend(std::iter::repeat_n(0x62, (FRAME_COUNT / 2) as usize));
    commands.push(0x66);

    let mut vgm = vec![0; HEADER_LEN];
    vgm[..4].copy_from_slice(b"Vgm ");
    vgm[8..12].copy_from_slice(&0x0000_0150_u32.to_le_bytes());
    vgm[12..16].copy_from_slice(&3_579_545_u32.to_le_bytes());
    vgm[24..28].copy_from_slice(&(FRAME_SAMPLES * FRAME_COUNT).to_le_bytes());
    vgm[52..56].copy_from_slice(&0x0c_u32.to_le_bytes());
    vgm.extend(commands);
    let eof_offset = (vgm.len() - 4) as u32;
    vgm[4..8].copy_from_slice(&eof_offset.to_le_bytes());
    vgm
}

fn gst_file(output_directory: &Path, pipeline: &[&str], filename: &str) -> Result<()> {
    run_command(
        output_directory,
        GST_LAUNCH,
        gst_arguments(pipeline, filename, 20),
    )
}

fn gst_arguments(pipeline: &[&str], filename: &str, buffers: u32) -> Vec<OsString> {
    let mut args = [
        "-q",
        "audiotestsrc",
        &format!("num-buffers={buffers}"),
        "!",
        "audioconvert",
        "!",
    ]
    .into_iter()
    .map(OsString::from)
    .collect::<Vec<_>>();
    for element in pipeline {
        args.push(OsString::from(element));
        args.push(OsString::from("!"));
    }
    args.push(OsString::from("filesink"));
    args.push(location_argument(filename));
    args
}

fn location_argument(filename: &str) -> OsString {
    let mut argument = OsString::from("location=");
    argument.push(filename);
    argument
}

fn wavpack_arguments() -> [OsString; 5] {
    [
        OsString::from("-q"),
        OsString::from("-y"),
        OsString::from(WAV_FILE),
        OsString::from("-o"),
        OsString::from(WAVPACK_FILE),
    ]
}

fn run_command<I>(working_directory: &Path, program: &str, args: I) -> Result<()>
where
    I: IntoIterator<Item = OsString>,
{
    let status = Command::new(program)
        .current_dir(working_directory)
        .args(args)
        .status()
        .map_err(|error| format!("failed to run {program}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} failed with status {status}").into())
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::{WAV_FILE, gst_arguments, wavpack_arguments};

    #[test]
    fn media_tools_receive_filenames_relative_to_the_output_directory() {
        let gst = gst_arguments(&["wavenc"], WAV_FILE, 20);
        assert_has_no_directory(gst.last().expect("GStreamer output argument"));

        let wavpack = wavpack_arguments();
        assert_has_no_directory(&wavpack[2]);
        assert_has_no_directory(&wavpack[4]);
    }

    fn assert_has_no_directory(argument: &OsStr) {
        let argument = argument.to_string_lossy();
        assert!(
            !argument.contains('/'),
            "unexpected directory in {argument}"
        );
        assert!(
            !argument.contains('\\'),
            "unexpected directory in {argument}"
        );
    }
}

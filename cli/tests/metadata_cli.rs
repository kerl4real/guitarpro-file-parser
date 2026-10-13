use std::{
    fs,
    io::{Cursor, Read, Write},
    process::Command,
};
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

#[test]
fn edits_title_preserves_assets_and_backs_up_original() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("song.gp");
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .start_file("Content/score.gpif", SimpleFileOptions::default())
        .unwrap();
    writer
        .write_all(b"<GPIF><Score><Title/><Artist>Artist</Artist></Score><Tracks/></GPIF>")
        .unwrap();
    writer
        .start_file("Content/audio.bin", SimpleFileOptions::default())
        .unwrap();
    writer.write_all(b"audio bytes preserved exactly").unwrap();
    let original = writer.finish().unwrap().into_inner();
    fs::write(&input, &original).unwrap();
    let run = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_score_tool"))
            .args(["metadata", "set", "-i"])
            .arg(&input)
            .args(["--title", "Title & More"])
            .args(extra)
            .output()
            .unwrap()
    };
    let output = run(&["--only-if-empty", "--backup"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(temp.path().join("song.gp.bak")).unwrap(), original);
    let edited = fs::read(&input).unwrap();
    let mut archive = ZipArchive::new(Cursor::new(&edited)).unwrap();
    let mut xml = String::new();
    archive
        .by_name("Content/score.gpif")
        .unwrap()
        .read_to_string(&mut xml)
        .unwrap();
    assert!(xml.contains("<Title><![CDATA[Title & More]]></Title>"));
    assert!(xml.contains("<Artist>Artist</Artist>"));
    let mut asset = Vec::new();
    archive
        .by_name("Content/audio.bin")
        .unwrap()
        .read_to_end(&mut asset)
        .unwrap();
    assert_eq!(asset, b"audio bytes preserved exactly");
    assert!(run(&["--only-if-empty"]).status.success());
    assert_eq!(fs::read(&input).unwrap(), edited);
    let destination = temp.path().join("copy.gp");
    let result = Command::new(env!("CARGO_BIN_EXE_score_tool"))
        .args(["metadata", "set", "-i"])
        .arg(&input)
        .args(["--title", "Different", "--output"])
        .arg(&destination)
        .output()
        .unwrap();
    assert!(result.status.success());
    assert_eq!(fs::read(&input).unwrap(), edited);
    assert!(destination.exists());
}

#[test]
fn updates_multiple_fields_independently_without_title() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("song.gp");
    let output = temp.path().join("edited.gp");
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .start_file("Content/score.gpif", SimpleFileOptions::default())
        .unwrap();
    writer.write_all(b"<GPIF><Score><Title>Original</Title><SubTitle/><Artist>Keep me</Artist></Score></GPIF>").unwrap();
    let original = writer.finish().unwrap().into_inner();
    fs::write(&input, &original).unwrap();
    let invoke = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_score_tool"))
            .args(["metadata", "set", "-i"])
            .arg(&input)
            .args(extra)
            .output()
            .unwrap()
    };
    assert!(!invoke(&[]).status.success());
    assert!(!invoke(&["--album", ""]).status.success());
    let result = Command::new(env!("CARGO_BIN_EXE_score_tool"))
        .args(["metadata", "set", "-i"])
        .arg(&input)
        .args([
            "--subtitle",
            "Live & loud",
            "--artist",
            "Replace me",
            "--album",
            "New album",
            "--only-if-empty",
            "--output",
        ])
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(fs::read(&input).unwrap(), original);
    let mut archive = ZipArchive::new(Cursor::new(fs::read(output).unwrap())).unwrap();
    let mut xml = String::new();
    archive
        .by_name("Content/score.gpif")
        .unwrap()
        .read_to_string(&mut xml)
        .unwrap();
    assert_eq!(
        xml,
        "<GPIF><Score><Title>Original</Title><SubTitle><![CDATA[Live & loud]]></SubTitle><Artist>Keep me</Artist><Album><![CDATA[New album]]></Album></Score></GPIF>"
    );
}

fn metadata_end(bytes: &[u8]) -> usize {
    let mut cursor = 31;
    for _ in 0..4 {
        let length = i32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap()) as usize;
        cursor += 4 + length;
    }
    cursor
}

#[test]
fn legacy_versions_preserve_every_byte_after_album() {
    let temp = tempfile::tempdir().unwrap();
    for extension in ["gp3", "gp4", "gp5"] {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../test/volta.{extension}"));
        let original = fs::read(fixture).unwrap();
        let input = temp.path().join(format!("song.{extension}"));
        let output = temp.path().join(format!("edited.{extension}"));
        fs::write(&input, &original).unwrap();
        let result = Command::new(env!("CARGO_BIN_EXE_score_tool"))
            .args(["metadata", "set", "-i"])
            .arg(&input)
            .args([
                "--title",
                "Caf\u{e9}",
                "--subtitle",
                "Live",
                "--artist",
                "Artist",
                "--album",
                "Album",
                "--output",
            ])
            .arg(&output)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let edited = fs::read(&output).unwrap();
        assert_eq!(
            &original[metadata_end(&original)..],
            &edited[metadata_end(&edited)..]
        );
        let mut song = guitarpro::Song::default();
        match extension {
            "gp3" => song.read_gp3(&edited),
            "gp4" => song.read_gp4(&edited),
            _ => song.read_gp5(&edited),
        }
        .unwrap();
        assert_eq!(song.name, "Caf\u{e9}");
        assert_eq!(song.subtitle, "Live");
        assert_eq!(song.artist, "Artist");
        assert_eq!(song.album, "Album");
        assert_eq!(fs::read(&input).unwrap(), original);
        let rejected = Command::new(env!("CARGO_BIN_EXE_score_tool"))
            .args(["metadata", "set", "-i"])
            .arg(&input)
            .args(["--title", "\u{1f3b8}"])
            .output()
            .unwrap();
        assert!(!rejected.status.success());
        assert_eq!(fs::read(&input).unwrap(), original);
    }
}

#[test]
fn gpx_edits_roundtrip_through_the_score_parser() {
    let temp = tempfile::tempdir().unwrap();
    let original =
        fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../test/volta.gpx"))
            .unwrap();
    let input = temp.path().join("song.gpx");
    fs::write(&input, &original).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_score_tool"))
        .args(["metadata", "set", "-i"])
        .arg(&input)
        .args([
            "--title",
            "GPX title",
            "--subtitle",
            "GPX subtitle",
            "--artist",
            "GPX artist",
            "--album",
            "GPX album",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let edited = fs::read(&input).unwrap();
    let mut song = guitarpro::Song::default();
    song.read_gpx(&edited).unwrap();
    assert_eq!(song.name, "GPX title");
    assert_eq!(song.album, "GPX album");
    assert!(!temp.path().join("song.gpx.bak").exists());
}

#[test]
fn gpx_preserves_original_sectors_and_embedded_assets() {
    use guitarpro::io::gpx::{pack_bcfs, patch_gpx_score};
    let xml = b"<GPIF><Score><Title>Old</Title></Score></GPIF>";
    let mut original = pack_bcfs("score.gpif", xml);
    let sector = 4096;
    let entry_sector = (original.len() - 4) / sector;
    let entry = original.len();
    original.resize(entry + 2 * sector, 0);
    original[entry..entry + 4].copy_from_slice(&2i32.to_le_bytes());
    original[entry + 4..entry + 13].copy_from_slice(b"asset.bin");
    original[entry + 0x8c..entry + 0x90].copy_from_slice(&11i32.to_le_bytes());
    original[entry + 0x94..entry + 0x98]
        .copy_from_slice(&((entry_sector + 1) as i32).to_le_bytes());
    original[entry + sector..entry + sector + 11].copy_from_slice(b"asset bytes");
    let edited = patch_gpx_score(&original, |xml| Ok(Some(xml.replace("Old", "New title"))))
        .unwrap()
        .unwrap();
    let score_entry = 4 + sector;
    assert_eq!(
        &original[..score_entry + 0x8c],
        &edited[..score_entry + 0x8c]
    );
    assert_eq!(
        &original[score_entry + 0x90..score_entry + 0x94],
        &edited[score_entry + 0x90..score_entry + 0x94]
    );
    assert_eq!(
        &original[score_entry + sector..],
        &edited[score_entry + sector..original.len()]
    );
    assert!(
        edited[original.len()..]
            .windows(9)
            .any(|bytes| bytes == b"New title")
    );
    assert!(patch_gpx_score(&original, |_| Ok(None)).unwrap().is_none());
}

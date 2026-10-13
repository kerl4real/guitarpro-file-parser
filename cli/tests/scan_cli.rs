use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn scan(root: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_score_tool"))
        .arg("scan")
        .arg("--input")
        .arg(root)
        .args(extra)
        .output()
        .unwrap()
}

#[test]
fn recursive_scan_streams_metadata_and_errors() {
    let temp = tempfile::tempdir().unwrap();
    let nested = temp.path().join("nested");
    fs::create_dir(&nested).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../test/mine/AAL.gp");
    let valid = nested.join("score.GP");
    fs::copy(fixture, &valid).unwrap();
    fs::write(temp.path().join("broken.gp5"), b"invalid score").unwrap();
    fs::write(temp.path().join("ignore.txt"), b"not a score").unwrap();

    let output = scan(
        temp.path(),
        &["--recursive", "--jobs", "2", "--format", "jsonl"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 2);
    let success = records
        .iter()
        .find(|record| record["status"] == "ok")
        .unwrap();
    let failure = records
        .iter()
        .find(|record| record["status"] == "error")
        .unwrap();
    assert_eq!(success["schema_version"], 1);
    let file_metadata = &success["file_metadata"];
    assert_eq!(
        file_metadata["size_bytes"],
        fs::metadata(&valid).unwrap().len()
    );
    assert_eq!(file_metadata["checksum_algorithm"], "sha256");
    assert_eq!(file_metadata["checksum"].as_str().unwrap().len(), 64);
    let extracted = time::OffsetDateTime::parse(
        success["extracted_at"].as_str().unwrap(),
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    assert_eq!(extracted.offset(), time::UtcOffset::UTC);
    assert!(file_metadata["updated_at"].as_str().unwrap().ends_with('Z'));

    assert!(failure["error"].as_str().unwrap().contains("header"));
    let info = Command::new(env!("CARGO_BIN_EXE_score_tool"))
        .args(["info", "--json", "-i"])
        .arg(fs::canonicalize(&valid).unwrap())
        .output()
        .unwrap();
    assert!(info.status.success());
    let mut metadata: serde_json::Value = serde_json::from_slice(&info.stdout).unwrap();
    assert!(metadata.get("timeline").is_some());
    metadata.as_object_mut().unwrap().remove("timeline");
    metadata.as_object_mut().unwrap().remove("file");
    let canonical = fs::canonicalize(&valid).unwrap();
    assert_eq!(success["metadata"]["file_name"], "score.GP");
    assert_eq!(
        success["metadata"]["directory"],
        canonical.parent().unwrap().to_str().unwrap()
    );
    assert!(success["metadata"].get("file").is_none());
    metadata["directory"] = success["metadata"]["directory"].clone();
    metadata["file_name"] = success["metadata"]["file_name"].clone();
    assert!(success["metadata"].get("timeline").is_none());
    assert_eq!(success["metadata"], metadata);
    assert!(String::from_utf8_lossy(&output.stderr).contains("1 successful, 1 errors"));

    let shallow = scan(temp.path(), &[]);
    assert!(shallow.status.success());
    assert_eq!(
        String::from_utf8(shallow.stdout).unwrap().lines().count(),
        1
    );
}

#[test]
fn rejects_invalid_input_and_worker_counts() {
    let temp = tempfile::tempdir().unwrap();
    for jobs in ["0", "65"] {
        assert!(!scan(temp.path(), &["--jobs", jobs]).status.success());
    }
    assert!(!scan(temp.path(), &["--format", "csv"]).status.success());
    assert!(!scan(&temp.path().join("missing"), &[]).status.success());
    let file = temp.path().join("file.gp");
    fs::write(&file, b"test").unwrap();
    assert!(!scan(&file, &[]).status.success());
}

#[test]
fn empty_directory_has_no_stdout_records() {
    let temp = tempfile::tempdir().unwrap();
    let output = scan(temp.path(), &["--recursive"]);
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("0 successful, 0 errors"));
}

#[test]
fn checksum_is_stable_and_changes_with_contents() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("broken.gp5");
    fs::write(&file, b"hello world").unwrap();
    let read_record = || -> serde_json::Value {
        let output = scan(temp.path(), &[]);
        assert!(output.status.success());
        serde_json::from_slice(&output.stdout).unwrap()
    };
    let first = read_record();
    let second = read_record();
    assert_eq!(
        first["file_metadata"]["checksum"],
        "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
    );
    assert_eq!(
        first["file_metadata"]["checksum"],
        second["file_metadata"]["checksum"]
    );
    assert_eq!(first["file_metadata"]["size_bytes"], 11);
    assert_eq!(first["status"], "error");
    fs::write(&file, b"hello WORLD").unwrap();
    let changed = read_record();
    assert_ne!(
        first["file_metadata"]["checksum"],
        changed["file_metadata"]["checksum"]
    );
}

use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::SystemTime;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use bpaf::Bpaf;
use serde::Serialize;

use crate::command_info::{InfoOutput, build_scan_json};

/// Scan a directory for score metadata, emitting one JSON object per line
#[derive(Bpaf, Debug)]
#[bpaf(command("scan"))]
pub struct ScanArgs {
    /// Directory to scan
    #[bpaf(short, long, argument("DIR"))]
    pub input: PathBuf,
    /// Include subdirectories (symbolic links are skipped)
    #[bpaf(long, switch)]
    pub recursive: bool,
    /// Number of parser workers
    #[bpaf(
        short,
        long,
        argument("N"),
        fallback(4usize),
        guard(valid_jobs, "jobs must be between 1 and 64")
    )]
    pub jobs: usize,
    /// Output format (currently jsonl only)
    #[bpaf(
        long,
        argument("FORMAT"),
        fallback(String::from("jsonl")),
        guard(valid_format, "format must be jsonl")
    )]
    pub format: String,
}

fn valid_jobs(jobs: &usize) -> bool {
    (1..=64).contains(jobs)
}

fn valid_format(format: &String) -> bool {
    format == "jsonl"
}

#[derive(Serialize)]
struct Record {
    schema_version: u8,
    path: PathBuf,
    status: &'static str,
    extracted_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_metadata: Option<FileMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<InfoOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl Record {
    fn error(path: PathBuf, error: impl ToString) -> Self {
        Self {
            schema_version: 1,
            path,
            status: "error",
            extracted_at: timestamp(SystemTime::now()).expect("current time is representable"),
            file_metadata: None,
            metadata: None,
            error: Some(error.to_string()),
        }
    }
}

fn supported(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            ["gp3", "gp4", "gp5", "gp", "gpx", "mscz"]
                .iter()
                .any(|candidate| ext.eq_ignore_ascii_case(candidate))
        })
}

// Discovery and parsing run concurrently. Bounded queues prevent a large scan
// from collecting all paths or parsed scores in memory before writing results.
fn discover(
    root: PathBuf,
    recursive: bool,
    paths: mpsc::SyncSender<PathBuf>,
    results: mpsc::SyncSender<Record>,
) {
    let mut directories = vec![root];
    while let Some(directory) = directories.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                if results.send(Record::error(directory, error)).is_err() {
                    return;
                }
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    if results
                        .send(Record::error(directory.clone(), error))
                        .is_err()
                    {
                        return;
                    }
                    continue;
                }
            };
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() && recursive => directories.push(path),
                Ok(kind) if kind.is_file() && supported(&path) => {
                    if paths.send(path).is_err() {
                        return;
                    }
                }
                Ok(_) => (),
                Err(error) => {
                    if results.send(Record::error(path, error)).is_err() {
                        return;
                    }
                }
            }
        }
    }
}

#[derive(Serialize)]
struct FileMetadata {
    created_at: Option<String>,
    updated_at: Option<String>,
    size_bytes: u64,
    checksum: String,
    checksum_algorithm: &'static str,
}

fn timestamp(value: SystemTime) -> Option<String> {
    OffsetDateTime::from(value).format(&Rfc3339).ok()
}

fn parse(path: PathBuf) -> Record {
    let mut record = Record::error(path.clone(), "Extraction failed");
    let result = (|| -> anyhow::Result<InfoOutput> {
        let path_str = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Path is not valid UTF-8"))?;
        let file = fs::File::open(&path)?;
        let before = file.metadata()?;
        // Bound the read to the loader's largest accepted input plus one byte.
        let mut bytes = Vec::new();
        file.take(32 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        anyhow::ensure!(
            bytes.len() <= 32 * 1024 * 1024,
            "File is too large (> 32 MB)"
        );
        let after = fs::metadata(&path)?;
        anyhow::ensure!(
            before.len() == after.len()
                && before.modified().ok() == after.modified().ok()
                && after.len() == bytes.len() as u64,
            "File changed while being read; rescan it"
        );
        record.file_metadata = Some(FileMetadata {
            created_at: before.created().ok().and_then(timestamp),
            updated_at: before.modified().ok().and_then(timestamp),
            size_bytes: bytes.len() as u64,
            checksum: Sha256::digest(&bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
            checksum_algorithm: "sha256",
        });
        // Hash and parse the same snapshot so the checksum describes precisely
        // the contents from which the score metadata was extracted.
        match std::panic::catch_unwind(|| crate::loader::load_song_bytes(&path, &bytes)) {
            Ok(result) => {
                let (song, format) = result?;
                Ok(build_scan_json(&song, path_str, &format))
            }
            Err(_) => anyhow::bail!("Parser panicked while reading this file"),
        }
    })();
    record.extracted_at = timestamp(SystemTime::now()).expect("current time is representable");
    match result {
        Ok(metadata) => {
            record.status = "ok";
            record.metadata = Some(metadata);
            record.error = None;
        }
        Err(error) => record.error = Some(format!("{error:#}")),
    }
    record
}

pub fn run(args: &ScanArgs) -> anyhow::Result<()> {
    anyhow::ensure!(args.format == "jsonl", "Output format must be jsonl");
    let root = fs::canonicalize(&args.input)?;
    anyhow::ensure!(root.is_dir(), "Input must be a directory");
    let capacity = args.jobs * 2;
    let (paths_tx, paths_rx) = mpsc::sync_channel(capacity);
    let (results_tx, results_rx) = mpsc::sync_channel(capacity);
    let paths_rx = Arc::new(Mutex::new(paths_rx));
    std::thread::scope(|scope| -> anyhow::Result<()> {
        let discovery_results = results_tx.clone();
        scope.spawn(move || discover(root, args.recursive, paths_tx, discovery_results));
        for _ in 0..args.jobs {
            let paths = Arc::clone(&paths_rx);
            let results = results_tx.clone();
            scope.spawn(move || {
                loop {
                    let next = paths.lock().expect("path queue poisoned").recv();
                    let Ok(path) = next else { break };
                    if results.send(parse(path)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(paths_rx);
        drop(results_tx);
        // Dropping the receiver on output failure releases blocked producers.
        let mut output = io::stdout().lock();
        let mut succeeded = 0;
        let mut failed = 0;
        for record in results_rx {
            serde_json::to_writer(&mut output, &record)?;
            writeln!(output)?;
            output.flush()?;
            if record.status == "ok" {
                succeeded += 1;
            } else {
                failed += 1;
            }
            if (succeeded + failed) % 100 == 0 {
                eprintln!("Scanned: {succeeded} successful, {failed} errors");
            }
        }
        eprintln!("Scan complete: {succeeded} successful, {failed} errors");
        Ok(())
    })
}

# Guitar Pro Tools

A comprehensive suite of tools for parsing, manipulating, and visualizing Guitar Pro files in Rust.

## Project Structure

This workspace is divided into several crates:

- **[lib](guitarpro/README.md)** (`guitarpro`): The core library to read and write **Guitar Pro** files (GP3, GP4, GP5, GPX, GP7) and **MuseScore** files (MSCZ). It provides a unified data model for musical scores.
- **[cli](cli/README.md)** (`score_tool`): A command-line interface to inspect files, view metadata, and generate ASCII tablatures.
- **web_server**: (Experimental) A web server to search and browse music scores through an API.

## Supported formats

| Format | Read | Write | Notes |
|---|---|---|---|
| `.gp3` / `.gp4` / `.gp5` | ✅ | ✅ | High-fidelity legacy binary |
| `.gpx` (GP6) | ✅ | ✅ | BCFZ/BCFS container |
| `.gp` (GP7+) | ✅ | ✅ | ZIP container with GPIF XML |
| `.mscz` (MuseScore 4.x) | ✅ | ✅ | ZIP container with MSCX XML — see [`docs/Roadmap-mscz.md`](docs/Roadmap-mscz.md) |
| MusicXML (`.xml` / `.musicxml`) | ✅ | ✅ | Score-partwise and timewise |

## Features

- **Multi-format support**: GP3/GP4/GP5, GP6 (`.gpx`), GP7+ (`.gp`), MuseScore (`.mscz`), MusicXML.
- **Rich Data Model**: Exhaustive representation of tracks, measures, beats, notes, and musical effects.
- **ASCII Visualization**: Generate text-based tablatures directly from the CLI.
- **Extensible Architecture**: Module-based design with traits for easy extension.

## Usage

To get started with the CLI, run:

```bash
cargo run -p cli -- info -i ./test/mine/AAL.gp
```

Inspect a MuseScore file:

```bash
cargo run -p cli -- info -i path/to/song.mscz
cargo run -p cli -- mscz list -i path/to/song.mscz
```

## Editing GP metadata

Set title, subtitle, artist, or album in GP3, GP4, GP5, GPX (GP6), or GP (GP7+) files:

```powershell
score_tool metadata set -i song.gp --title "My Song" --only-if-empty
```

This edits the file in place without creating a backup by default. Add `--backup`
to first save the original as `song.gp.bak`:

```powershell
score_tool metadata set -i song.gp --title "My Song" --backup
```

An existing backup is never overwritten. `--backup` cannot be combined with `--output`. `--only-if-empty` skips fields whose value
contains non-whitespace text. It applies separately to each requested field; omit
the flag to replace existing values. Supply at least one metadata option.
A skipped edit leaves the input unchanged and creates no output or backup.

Multiple fields can be updated together, including without a title:

```powershell
score_tool metadata set -i song.gp --subtitle "Live version" --artist "My Artist" --album "My Album" --only-if-empty
```

To keep the input untouched and write a new file:

```powershell
score_tool metadata set -i song.gp --title "My Song" --output song.updated.gp
```

The output must not already exist and must use the same extension as the input.
Supported extensions are `.gp3`, `.gp4`, `.gp5`, `.gpx`, and `.gp`. Supplied values must be nonempty. Omitted fields remain untouched.
The command replaces or inserts the requested `Title`, `SubTitle`, `Artist`, or `Album`
elements within `GPIF/Score` in `Content/score.gpif`, uses Guitar Pro-style CDATA sections, and retains the rest of the XML. Other archive entries (including
audio, styles, and layout) are copied without re-encoding. The archive container
is rebuilt; it is not a byte-for-byte copy. Writes use a temporary file beside the
destination, and in-place edits check whether the input changed before saving.

### Format-specific preservation

For GP3/4/5, the command replaces only the requested length-prefixed metadata
strings; the header, omitted fields, and every byte after the album remain intact.
Supported standard headers are v3.00, v4.00, v4.06, v5.00, and v5.10; clipboard and
other versions are rejected. Values must fit Windows-1252 and the 255-byte limit.
Unrepresentable characters are rejected rather than replaced or truncated.

For GPX, metadata uses the same GPIF/CDATA editing as GP files. The original BCFS
filesystem sectors are retained, and the edited GPIF is appended with an updated
score directory entry. All other entry data remains unchanged. BCFZ input is
recompressed using literal blocks, so the resulting file may be larger. Repeated
edits can grow the container because original score sectors are retained. Edits
that exceed the filesystem block-table capacity are rejected.

```powershell
score_tool metadata set -i song.gp5 --title "My Song" --only-if-empty
score_tool metadata set -i song.gpx --artist "My Artist" --output song.updated.gpx
```

Reopen edited files in Guitar Pro to verify application compatibility. Parser
roundtrip tests alone do not establish that every Guitar Pro version accepts them.

## Batch metadata scanning

Build once with `cargo build --release -p cli`, then call the executable from any project:

```powershell
.\target\release\score_tool.exe scan -i C:\scores --recursive --jobs 4 --format jsonl > metadata.jsonl
```

The scan supports `.gp3`, `.gp4`, `.gp5`, `.gp`, `.gpx`, and `.mscz` (case-insensitive).
Without `--recursive`, it scans only the input directory. Symbolic links are skipped.
`--jobs` defaults to 4 and accepts 1?64 workers. Results stream to stdout in completion
order; progress every 100 results and a final summary go to stderr. Queues are bounded.
Each score is fully parsed using the same loader as `info --json`.

Each line is an independent JSON object with `schema_version: 1`, an absolute `path`,
and `status`. Successful records contain `metadata` with the same fields as `info --json`, excluding the per-measure timeline and splitting `file` into `directory` and `file_name`;
failed records contain an `error` string.
Every record includes `extracted_at` (UTC RFC 3339). Readable, size-supported files also
include `file_metadata`: `created_at`, `updated_at`, `size_bytes`, `checksum`, and
`checksum_algorithm` (`sha256`). Filesystem dates use UTC RFC 3339 and are `null` when
unavailable. `updated_at` is the last content modification time. The checksum is
lowercase hexadecimal SHA-256 over the complete file bytes, including embedded assets.
Compare checksums between scans to detect content changes, independent of file dates.
The parser and checksum use the same byte snapshot; detectable changes during the read
produce an error requesting a rescan. Creation dates describe the current filesystem
copy, which may differ from the original file's creation date. Directory access errors also produce error
records with the directory path. Empty metadata fields may be omitted.

```json
{"schema_version":1,"path":"C:\\scores\\broken.gp5","status":"error","error":"..."}
```

Individual parse or discovery failures do not stop the scan and do not cause a nonzero
exit status. Inspect the records for failures. Invalid arguments, an invalid root, or
output failures cause a nonzero exit status. An empty directory produces no records.
Keep output files outside the scanned directory or use an unsupported extension such
as `.jsonl`. The executable must be built for the operating system running the scan.

### Metadata field reference

The following fields describe the JSON Lines output from `scan`. Field names are
shown exactly as emitted. Values depend on the source format and the metadata
present in the score; missing source values may be empty or use parser defaults.

#### Scan record

| Field | Type | Meaning |
|---|---|---|
| `schema_version` | integer | Output schema version; currently `1`. |
| `path` | string | Absolute path to the score, or the directory associated with a discovery error. |
| `status` | string | `ok` for a successful extraction; `error` for a failed extraction or discovery. |
| `extracted_at` | string | UTC RFC 3339 timestamp when this record was produced. |
| `file_metadata` | object | File details listed below; omitted if the file could not be read consistently or exceeded the size limit. May be present even when score parsing fails. |
| `metadata` | object | Score metadata listed below; present only on success. |
| `error` | string | Failure description; present only on error. |

#### File metadata (`file_metadata`)

| Field | Type | Meaning |
|---|---|---|
| `created_at` | string or null | Filesystem creation date in UTC RFC 3339; `null` when unavailable. This is the creation date of the current filesystem copy. |
| `updated_at` | string or null | Filesystem last modification date in UTC RFC 3339; `null` when unavailable. |
| `size_bytes` | integer | Complete file size in bytes. |
| `checksum` | string | Lowercase hexadecimal SHA-256 checksum of the exact file bytes used for parsing. Includes embedded assets; compare between scans to detect content changes. |
| `checksum_algorithm` | string | Checksum algorithm; currently `sha256`. |

#### Score metadata (`metadata`)

| Field | Type | Meaning |
|---|---|---|
| `directory` | string | Absolute parent directory of the score file. |
| `file_name` | string | File name including its extension, without the directory. |
| `format` | string | Loader format label: `GP3`, `GP4`, `GP5`, `GPX`, `GP`, or `MSCZ`. |
| `version` | array of three integers | Parser's score version tuple `[major, minor, patch]`; interpretation depends on source format. |
| `title` | string | Score title. |
| `subtitle` | string | Score subtitle; omitted when empty. |
| `artist` | string | Artist; omitted when empty. |
| `album` | string | Album; omitted when empty. |
| `composer` | string | Music author/composer; omitted when empty. |
| `words` | string | Words/lyrics author credit; omitted when empty. |
| `copyright` | string | Copyright notice; omitted when empty. |
| `transcriber` | string | Transcription/writer credit; omitted when empty. |
| `instructions` | string | Score instructions; omitted when empty. |
| `comments` | string | Score comments; omitted when empty. |
| `notice` | array of strings | Notice lines; omitted when empty. |
| `initial_tempo` | integer | Initial tempo in beats per minute. |
| `initial_key` | string | Initial key signature as formatted by the parser. |
| `measure_count` | integer | Number of global measure headers. |
| `track_count` | integer | Number of tracks. |
| `tracks` | array of objects | Track metadata listed below. |

#### Track metadata (`metadata.tracks[]`)

| Field | Type | Meaning |
|---|---|---|
| `index` | integer | Track position, starting at `1`. |
| `name` | string | Track name. |
| `kind` | string | Track classification: `percussion`, `12-string`, `banjo`, or `stringed`. |
| `tuning` | array of objects | String tuning entries listed below; omitted for percussion tracks or empty tuning. |
| `frets` | integer | Instrument fret count. |
| `measure_count` | integer | Number of measures in this track. |
| `max_voices` | integer | Maximum number of voices containing beats in any track measure. |
| `midi_channel` | integer | MIDI channel number, starting at `1`. |
| `midi_program` | integer | Raw MIDI instrument program value from the parsed channel; conventionally zero-based. |
| `instrument` | string | Instrument name resolved from the MIDI program, or `Drums / Percussion` for percussion tracks. |
| `solo` | boolean | Whether the track is marked solo. |
| `mute` | boolean | Whether the track is marked muted. |
| `hidden` | boolean | Whether the track is hidden; omitted when false. |

#### String tuning (`metadata.tracks[].tuning[]`)

| Field | Type | Meaning |
|---|---|---|
| `number` | integer | String number from the parsed track. |
| `midi` | integer | Open-string MIDI pitch number. |
| `note` | string | Note name and octave derived from the MIDI pitch, such as `E2`. |

#### Timeline (`info --json` only)

`scan` omits the timeline. The standalone `info --json` command additionally emits
`timeline`, an array with one object per global measure header:

| Field | Type | Meaning |
|---|---|---|
| `number` | integer | Measure number. |
| `time_signature` | string | Time signature, such as `4/4`. |
| `tempo` | integer | Effective tempo in beats per minute, carried forward between tempo changes. |
| `key` | string | Effective key signature. |
| `marker` | string | Section marker title; omitted when absent. |
| `navigation` | array of strings | Repeat/navigation labels, such as `repeat_open`, `repeat_close:2`, or `volta:1,2`; omitted when empty. |

The standalone `info --json` output contains the score fields directly at its root,
with `file` (the input path) instead of `directory` and `file_name`.
The scan-only record fields and `file_metadata` are not included in that command.

## Roadmap

### Library
- [x] Refactor core into `model`, `io`, and `audio` modules.
- [x] Comprehensive trait-based API for `Song` operations.
- [x] High-fidelity GP5 parsing.
- [x] Initial support for GP6/7 (.gp/.gpx) formats.
- [x] MuseScore (.mscz) read / write / conversion — see [`docs/Roadmap-mscz.md`](docs/Roadmap-mscz.md).
- [ ] Full RSE (Realistic Sound Engine) data parsing.
- [ ] Export to MIDI/Audio.

### CLI
- [x] Basic metadata inspection.
- [x] ASCII Tablature generation.
- [x] Batch conversion (`score_tool convert`).
- [x] MSCZ archive tooling (`score_tool mscz list / extract / thumbnail`).
- [ ] Advanced search and filtering.

## Documentation

- [Web-server roadmap](docs/Roadmap-web.md)
- [MSCZ (MuseScore) roadmap](docs/Roadmap-mscz.md)

## License

This project is licensed under the MIT License.

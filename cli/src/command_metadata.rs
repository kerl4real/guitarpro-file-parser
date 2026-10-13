use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, ensure};
use bpaf::Bpaf;
use quick_xml::{Reader, events::Event};
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

/// Edit metadata in GP3/4/5, GPX, and GP score files
#[derive(Bpaf, Debug)]
#[bpaf(command("metadata"))]
pub struct MetadataArgs {
    #[bpaf(external(metadata_action))]
    action: MetadataAction,
}

#[derive(Bpaf, Debug)]
enum MetadataAction {
    /// Set score metadata without rewriting the musical score
    #[bpaf(command("set"))]
    Set {
        /// Input .gp3, .gp4, .gp5, .gpx, or .gp file
        #[bpaf(short, long, argument("FILE"))]
        input: PathBuf,
        /// New title
        #[bpaf(long, argument("TEXT"))]
        title: Option<String>,
        /// New subtitle
        #[bpaf(long, argument("TEXT"))]
        subtitle: Option<String>,
        /// New artist
        #[bpaf(long, argument("TEXT"))]
        artist: Option<String>,
        /// New album
        #[bpaf(long, argument("TEXT"))]
        album: Option<String>,
        /// Leave each nonempty field unchanged
        #[bpaf(long, switch)]
        only_if_empty: bool,
        /// Save the original as <input>.bak when editing in place
        #[bpaf(long, switch)]
        backup: bool,
        /// Write to a new file instead of editing in place
        #[bpaf(short, long, argument("FILE"))]
        output: Option<PathBuf>,
    },
}

fn is_gp(path: &Path) -> bool {
    path.extension().is_some_and(|ext| {
        ["gp3", "gp4", "gp5", "gpx", "gp"]
            .iter()
            .any(|candidate| ext.eq_ignore_ascii_case(candidate))
    })
}

// Patch only the requested metadata element byte range. Re-serializing GPIF would risk
// discarding fields, comments, formatting, or extensions we do not understand.
fn patch_field(
    xml: &str,
    field: &str,
    title: &str,
    only_if_empty: bool,
) -> anyhow::Result<Option<String>> {
    ensure!(!title.trim().is_empty(), "{field} must not be empty");
    ensure!(
        title
            .chars()
            .all(|c| matches!(c, '\t' | '\n' | '\r') || c >= '\u{20}'),
        "{field} contains invalid XML control characters"
    );
    let mut reader = Reader::from_str(xml);
    let mut stack = Vec::new();
    let mut range = None;
    let mut title_start = None;
    let mut current = String::new();
    let mut score_end = None;
    let mut score_count = 0;
    loop {
        let start = reader.buffer_position() as usize;
        match reader.read_event()? {
            Event::Start(tag) => {
                ensure!(
                    title_start.is_none(),
                    "Nested elements inside {field} are unsupported"
                );
                let name = tag.name().as_ref().to_owned();
                if stack.as_slice() == ["GPIF".to_owned()] && name == "Score" {
                    score_count += 1;
                }
                if stack.as_slice() == ["GPIF".to_owned(), "Score".to_owned()] && name == field {
                    ensure!(range.is_none(), "Multiple {field} elements are unsupported");
                    title_start = Some(start);
                }
                stack.push(name);
            }
            Event::Empty(tag) => {
                ensure!(
                    title_start.is_none(),
                    "Nested elements inside {field} are unsupported"
                );
                if stack.as_slice() == ["GPIF".to_owned(), "Score".to_owned()]
                    && tag.name().as_ref() == field
                {
                    ensure!(range.is_none(), "Multiple {field} elements are unsupported");
                    range = Some((start, reader.buffer_position() as usize));
                }
            }
            Event::End(tag) => {
                if stack.as_slice() == ["GPIF".to_owned(), "Score".to_owned(), field.to_owned()] {
                    range = Some((
                        title_start.take().context("Missing Title start")?,
                        reader.buffer_position() as usize,
                    ));
                }
                if stack.as_slice() == ["GPIF".to_owned(), "Score".to_owned()] {
                    score_end = Some(start);
                }
                ensure!(
                    stack.pop().as_deref() == Some(tag.name().as_ref()),
                    "Invalid XML nesting"
                );
            }
            Event::Text(text) if title_start.is_some() => current.push_str(&text),
            Event::CData(text) if title_start.is_some() => current.push_str(&text),
            Event::GeneralRef(reference) if title_start.is_some() => {
                if let Some(character) = reference.resolve_char_ref()? {
                    current.push(character);
                } else {
                    // Predefined named XML entities all represent non-whitespace.
                    current.push('x');
                }
            }
            Event::DocType(_) => anyhow::bail!("GPIF with a document type is unsupported"),
            Event::Eof => break,
            _ => (),
        }
    }
    ensure!(
        stack.is_empty() && score_count == 1,
        "Expected one GPIF/Score element"
    );
    if only_if_empty && !current.trim().is_empty() {
        return Ok(None);
    }
    let insertion = score_end.context("Missing Score element")?;
    let (start, end) = range.unwrap_or((insertion, insertion));
    // Match Guitar Pro's native metadata representation. Split the CDATA
    // terminator so arbitrary user text remains valid XML without alteration.
    let cdata = title.replace("]]>", "]]]]><![CDATA[>");
    let replacement = format!("<{field}><![CDATA[{cdata}]]></{field}>");
    let mut patched = xml.to_owned();
    patched.replace_range(start..end, &replacement);
    if patched == xml {
        Ok(None)
    } else {
        Ok(Some(patched))
    }
}

type Updates<'a> = [(&'a str, &'a Option<String>)];

fn patch_legacy(
    original: &[u8],
    updates: &Updates<'_>,
    only_if_empty: bool,
) -> anyhow::Result<Option<Vec<u8>>> {
    ensure!(original.len() >= 31, "Truncated legacy header");
    let header_len = original[0] as usize;
    ensure!(header_len <= 30, "Invalid legacy header length");
    let header = std::str::from_utf8(&original[1..1 + header_len])?;
    ensure!(
        [
            "FICHIER GUITAR PRO v3.00",
            "FICHIER GUITAR PRO v4.00",
            "FICHIER GUITAR PRO v4.06",
            "FICHIER GUITAR PRO v5.00",
            "FICHIER GUITAR PRO v5.10"
        ]
        .contains(&header),
        "Unsupported legacy version or clipboard file"
    );
    let mut cursor = 31usize;
    let mut edits = Vec::new();
    for &(field, value) in updates {
        let start = cursor;
        let length_bytes = original
            .get(cursor..cursor + 4)
            .context("Truncated metadata length")?;
        let length = i32::from_le_bytes(length_bytes.try_into().unwrap());
        ensure!(length >= 0, "Negative metadata length");
        cursor += 4;
        let old = if length == 0 {
            &[][..]
        } else {
            let stored = original
                .get(cursor..cursor + length as usize)
                .context("Truncated metadata string")?;
            let text_len = stored[0] as usize;
            ensure!(text_len < stored.len(), "Invalid metadata string length");
            cursor += length as usize;
            &stored[1..1 + text_len]
        };
        if let Some(value) = value {
            ensure!(!value.trim().is_empty(), "{field} must not be empty");
            ensure!(!value.contains('\0'), "{field} must not contain NUL");
            let (bytes, _, errors) = encoding_rs::WINDOWS_1252.encode(value);
            ensure!(
                !errors,
                "{field} contains characters not representable in Windows-1252"
            );
            ensure!(
                bytes.len() <= 255,
                "{field} exceeds the legacy 255-byte limit"
            );
            let (decoded, _, _) = encoding_rs::WINDOWS_1252.decode(old);
            if only_if_empty && !decoded.trim().is_empty() {
                continue;
            }
            let mut replacement = Vec::new();
            replacement.extend_from_slice(&((bytes.len() + 1) as i32).to_le_bytes());
            replacement.push(bytes.len() as u8);
            replacement.extend_from_slice(&bytes);
            edits.push((start, cursor, replacement));
        }
    }
    let mut patched = original.to_vec();
    for (start, end, bytes) in edits.into_iter().rev() {
        patched.splice(start..end, bytes);
    }
    Ok((patched != original).then_some(patched))
}

fn patch_xml(
    xml: &str,
    updates: &Updates<'_>,
    only_if_empty: bool,
) -> anyhow::Result<Option<String>> {
    let mut patched = xml.to_owned();
    for &(field, value) in updates {
        if let Some(value) = value {
            if let Some(updated) = patch_field(&patched, field, value, only_if_empty)? {
                patched = updated;
            }
        }
    }
    Ok((patched != xml).then_some(patched))
}

fn patch_gp(
    original: &[u8],
    updates: &Updates<'_>,
    only_if_empty: bool,
) -> anyhow::Result<Option<Vec<u8>>> {
    let mut archive =
        ZipArchive::new(Cursor::new(original)).context("Input must be a GP ZIP archive")?;
    let indices: Vec<_> = (0..archive.len())
        .filter(|&i| archive.name_for_index(i) == Some("Content/score.gpif"))
        .collect();
    ensure!(
        indices.len() == 1,
        "Expected exactly one Content/score.gpif entry"
    );
    let index = indices[0];
    let (xml, options) = {
        let mut entry = archive.by_index(index)?;
        ensure!(entry.size() <= 64 * 1024 * 1024, "GPIF exceeds 64 MB");
        let options = SimpleFileOptions::default()
            .compression_method(entry.compression())
            .last_modified_time(entry.last_modified().unwrap_or_default())
            .unix_permissions(entry.unix_mode().unwrap_or(0o644));
        let mut xml = String::new();
        entry
            .read_to_string(&mut xml)
            .context("GPIF must be UTF-8 XML")?;
        (xml, options)
    };
    let mut patched = xml.clone();
    for &(field, value) in updates {
        if let Some(value) = value {
            if let Some(updated) = patch_field(&patched, field, value, only_if_empty)? {
                patched = updated;
            }
        }
    }
    if patched == xml {
        return Ok(None);
    }
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    writer.set_raw_comment(archive.comment().to_vec().into())?;
    for i in 0..archive.len() {
        if i == index {
            writer.start_file("Content/score.gpif", options)?;
            writer.write_all(patched.as_bytes())?;
        } else {
            writer.raw_copy_file(archive.by_index(i)?)?;
        }
    }
    Ok(Some(writer.finish()?.into_inner()))
}

pub fn run(args: &MetadataArgs) -> anyhow::Result<()> {
    let MetadataAction::Set {
        input,
        title,
        subtitle,
        artist,
        album,
        only_if_empty,
        backup,
        output,
    } = &args.action;
    let updates = [
        ("Title", title),
        ("SubTitle", subtitle),
        ("Artist", artist),
        ("Album", album),
    ];
    ensure!(
        updates.iter().any(|(_, value)| value.is_some()),
        "Provide at least one of --title, --subtitle, --artist, or --album"
    );
    ensure!(is_gp(input), "Supported extensions: gp3, gp4, gp5, gpx, gp");
    ensure!(
        !(*backup && output.is_some()),
        "--backup applies only to in-place edits; omit --output"
    );
    if let Some(output) = output {
        ensure!(is_gp(output), "Unsupported output extension");
        ensure!(
            input
                .extension()
                .unwrap()
                .eq_ignore_ascii_case(output.extension().unwrap()),
            "Output must use the same extension as input"
        );
        ensure!(!output.exists(), "Output already exists");
    }
    let input = fs::canonicalize(input)?;
    ensure!(
        fs::metadata(&input)?.len() <= 32 * 1024 * 1024,
        "Archive exceeds 32 MB"
    );
    let original = fs::read(&input)?;
    let updated = if original.starts_with(b"BCFZ") || original.starts_with(b"BCFS") {
        guitarpro::io::gpx::patch_gpx_score(&original, |xml| {
            patch_xml(xml, &updates, *only_if_empty).map_err(|error| error.to_string())
        })
        .map_err(|error| anyhow::anyhow!(error))?
    } else if original.starts_with(b"PK") {
        patch_gp(&original, &updates, *only_if_empty)?
    } else {
        patch_legacy(&original, &updates, *only_if_empty)?
    };
    let Some(updated) = updated else {
        println!("Unchanged: {}", input.display());
        return Ok(());
    };
    let destination = output.as_deref().unwrap_or(&input);
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(&updated)?;
    temporary.as_file_mut().sync_all()?;
    if output.is_none() {
        ensure!(
            fs::read(&input)? == original,
            "Input changed while editing; retry"
        );
        if *backup {
            let mut backup = input.as_os_str().to_owned();
            backup.push(".bak");
            let mut backup_file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(PathBuf::from(backup))
                .context("Cannot create backup (existing .bak files are never overwritten)")?;
            backup_file.write_all(&original)?;
            backup_file.sync_all()?;
        }
        temporary
            .as_file()
            .set_permissions(fs::metadata(&input)?.permissions())?;
        temporary.persist(&input).map_err(|e| e.error)?;
    } else {
        temporary
            .persist_noclobber(destination)
            .map_err(|e| e.error)?;
    }
    println!("Updated: {}", destination.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::patch_field;

    #[test]
    fn changes_only_title_and_escapes_text() {
        let xml = "<GPIF><Score><Title><![CDATA[ old ]]></Title><Artist>X</Artist></Score><Tracks/></GPIF>";
        assert_eq!(
            patch_field(xml, "Title", "A & B", false).unwrap().unwrap(),
            xml.replace(
                "<Title><![CDATA[ old ]]></Title>",
                "<Title><![CDATA[A & B]]></Title>"
            )
        );
        assert!(patch_field(xml, "Title", "new", true).unwrap().is_none());
        for content in ["<Title/>", "<Title>  </Title>", "<Title>&#32;</Title>", ""] {
            let xml = format!("<GPIF><Score>{content}</Score></GPIF>");
            assert!(
                patch_field(&xml, "Title", "new", true)
                    .unwrap()
                    .unwrap()
                    .contains("<Title><![CDATA[new]]></Title>")
            );
        }
        assert!(patch_field("<GPIF><Score>", "Title", "new", false).is_err());
    }
}

use crate::error::GpResult;
use crate::io::gpif::Gpif;
use quick_xml::de::from_str;
use std::io::Cursor;
use zip::ZipArchive;

// ---------------------------------------------------------------------------
// GP6 (.gpx) BCFZ writer
// ---------------------------------------------------------------------------

/// Bit-level writer (MSB-first within each byte, matching `BitStream` read order).
struct BitWriter {
    data: Vec<u8>,
    current: u8,
    used: u8, // bits filled (0-7), bit 7 is written first
}

impl BitWriter {
    fn new() -> Self {
        BitWriter {
            data: Vec::new(),
            current: 0,
            used: 0,
        }
    }

    fn write_bit(&mut self, bit: u8) {
        let shift = 7 - self.used;
        self.current |= (bit & 1) << shift;
        self.used += 1;
        if self.used == 8 {
            self.data.push(self.current);
            self.current = 0;
            self.used = 0;
        }
    }

    /// Write `count` bits MSB-first (matches `read_bits`).
    fn write_bits_msb(&mut self, value: u32, count: usize) {
        for i in (0..count).rev() {
            self.write_bit(((value >> i) & 1) as u8);
        }
    }

    /// Write `count` bits LSB-first (matches `read_bits_reversed`).
    fn write_bits_lsb(&mut self, value: u32, count: usize) {
        for i in 0..count {
            self.write_bit(((value >> i) & 1) as u8);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.used > 0 {
            self.data.push(self.current);
        }
        self.data
    }
}

/// Encode `data` as a valid BCFZ container (store-only, no back-references).
///
/// Each byte is emitted as a literal block:
///   flag=0 (literal), size=1 (2 bits LSB-first), byte (8 bits MSB-first).
pub fn compress_bcfz(data: &[u8]) -> Vec<u8> {
    let mut bits = BitWriter::new();
    let mut i = 0;
    while i < data.len() {
        // Up to 3 bytes per literal block (2-bit size field, value 1-3)
        let batch = (data.len() - i).min(3) as u32;
        bits.write_bit(0); // literal flag
        bits.write_bits_lsb(batch, 2);
        for j in 0..batch as usize {
            bits.write_bits_msb(data[i + j] as u32, 8);
        }
        i += batch as usize;
    }
    let compressed = bits.finish();

    let mut out = Vec::with_capacity(8 + compressed.len());
    out.extend_from_slice(BCFZ_MAGIC);
    out.extend_from_slice(&(data.len() as i32).to_le_bytes());
    out.extend(compressed);
    out
}

/// Pack a single file into a BCFS virtual filesystem (uncompressed BCFZ input).
///
/// Layout (after the 4-byte "BCFS" magic, all in 0x1000-byte sectors):
///   sector 0: blank (header)
///   sector 1: file directory entry for `filename`
///   sectors 2+: file data
pub fn pack_bcfs(filename: &str, file_data: &[u8]) -> Vec<u8> {
    let file_size = file_data.len();
    let num_data_sectors = file_size.div_ceil(SECTOR_SIZE);
    let total_sectors = 2 + num_data_sectors;
    let disk_size = total_sectors * SECTOR_SIZE;

    let mut disk = vec![0u8; disk_size];

    // Sector 1: directory entry (disk offset = SECTOR_SIZE)
    let entry = SECTOR_SIZE;

    // Type = 2
    disk[entry..entry + 4].copy_from_slice(&2i32.to_le_bytes());

    // Name (null-terminated, max 127 bytes)
    let name_bytes = filename.as_bytes();
    let name_len = name_bytes.len().min(127);
    disk[entry + 4..entry + 4 + name_len].copy_from_slice(&name_bytes[..name_len]);
    // disk[entry + 4 + name_len] = 0; // already zero

    // File size at +0x8C
    disk[entry + 0x8C..entry + 0x90].copy_from_slice(&(file_size as i32).to_le_bytes());

    // Block index table at +0x94 (data sectors start at index 2)
    for i in 0..num_data_sectors {
        let idx_off = entry + 0x94 + i * 4;
        disk[idx_off..idx_off + 4].copy_from_slice(&((2 + i) as i32).to_le_bytes());
    }
    // Terminator already zero

    // Sectors 2+: file content
    for (i, chunk) in file_data.chunks(SECTOR_SIZE).enumerate() {
        let off = (2 + i) * SECTOR_SIZE;
        disk[off..off + chunk.len()].copy_from_slice(chunk);
    }

    let mut out = Vec::with_capacity(4 + disk_size);
    out.extend_from_slice(BCFS_MAGIC);
    out.extend(disk);
    out
}

/// Serialize a `Song` to GP6 (.gpx) bytes.
pub fn write_gpx_bytes(song: &crate::model::legacy::song::Song) -> GpResult<Vec<u8>> {
    use crate::io::gpif_export::SongGpifExportOps;
    let xml = song.write_gpif_xml();
    let bcfs = pack_bcfs("score.gpif", xml.as_bytes());
    Ok(compress_bcfz(&bcfs))
}

/// Serialize a `Song` to GP7+ (.gp) bytes (ZIP archive with `Content/score.gpif`).
pub fn write_gp_bytes(song: &crate::model::legacy::song::Song) -> GpResult<Vec<u8>> {
    use crate::io::gpif_export::SongGpifExportOps;
    use std::io::Write as IoWrite;
    use zip::write::SimpleFileOptions;

    let xml = song.write_gpif_xml();
    let buf = Vec::new();
    let cursor = std::io::Cursor::new(buf);
    let mut zip = zip::ZipWriter::new(cursor);

    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("Content/score.gpif", options)
        .map_err(|e| format!("Zip write error: {}", e))?;
    zip.write_all(xml.as_bytes())
        .map_err(|e| format!("Zip data write error: {}", e))?;

    let cursor = zip
        .finish()
        .map_err(|e| format!("Zip finish error: {}", e))?;
    Ok(cursor.into_inner())
}

/// Reads a .gp (GP7+) file which is a ZIP archive containing 'Content/score.gpif'.
pub fn read_gp(data: &[u8]) -> GpResult<Gpif> {
    Ok(read_gp_with_audio(data)?.0)
}

/// Reads a .gp (GP7+) file, returning the parsed GPIF plus any embedded
/// backing-track audio found under `Content/Assets/` (e.g. `*.mp3`).
///
/// GP8 files (declared via `<GPVersion>8.x</GPVersion>` in the GPIF; the ZIP
/// `VERSION` marker stays `7.0`) may bundle the original recording so the
/// score can be synchronized against it via `SyncPoint` automations. The audio
/// bytes are surfaced so the web server can serve them directly
/// (`/api/score/:id/audio`) and the CLI can extract them.
pub fn read_gp_with_audio(data: &[u8]) -> GpResult<(Gpif, Option<Vec<u8>>)> {
    use std::io::Read as _;

    let cursor = Cursor::new(data);
    let mut zip = ZipArchive::new(cursor).map_err(|e| format!("Zip error: {}", e))?;

    // Standard path for GP7 files
    let mut file = zip
        .by_name("Content/score.gpif")
        .map_err(|e| format!("Could not find score.gpif: {}", e))?;

    let mut contents = String::new();
    file.read_to_string(&mut contents)
        .map_err(|e| format!("Read error: {}", e))?;

    let gpif: Gpif = from_str(&contents).map_err(|e| format!("XML Parse error: {}", e))?;

    // Embedded backing track: first audio asset under Content/Assets/.
    // (zip entries can only be read once, so re-open the archive.)
    let mut audio: Option<Vec<u8>> = None;
    if let Ok(mut zip) = ZipArchive::new(Cursor::new(data)) {
        for i in 0..zip.len() {
            let Ok(mut entry) = zip.by_index(i) else {
                continue;
            };
            let name = entry.name().to_string();
            if name.starts_with("Content/Assets/") && name.ends_with(".mp3") {
                let mut buf = Vec::new();
                if entry.read_to_end(&mut buf).is_ok() {
                    audio = Some(buf);
                }
                break;
            }
        }
    }

    Ok((gpif, audio))
}

// ---------------------------------------------------------------------------
// GP6 (.gpx) BCFZ/BCFS container support
// ---------------------------------------------------------------------------

const BCFZ_MAGIC: &[u8; 4] = b"BCFZ";
const BCFS_MAGIC: &[u8; 4] = b"BCFS";

/// Edit only the score GPIF in a GPX container. All original filesystem sectors
/// are retained; the edited score is appended and its directory entry updated.
/// Compressed input is recompressed using the existing literal BCFZ encoder.
pub fn patch_gpx_score(
    input: &[u8],
    update: impl FnOnce(&str) -> Result<Option<String>, String>,
) -> Result<Option<Vec<u8>>, String> {
    let compressed = input.starts_with(BCFZ_MAGIC);
    let mut filesystem = if compressed {
        let size = input.get(4..8).ok_or("Truncated BCFZ header")?;
        let size = i32::from_le_bytes(size.try_into().unwrap());
        if !(0..=64 * 1024 * 1024).contains(&size) {
            return Err("Invalid or oversized BCFS filesystem".into());
        }
        let result = decompress_bcfz(input)?;
        if result.len() != size as usize {
            return Err("Truncated BCFZ filesystem".into());
        }
        result
    } else {
        input.to_vec()
    };
    if !filesystem.starts_with(BCFS_MAGIC)
        || filesystem.len() < 4 + SECTOR_SIZE * 2
        || (filesystem.len() - 4) % SECTOR_SIZE != 0
    {
        return Err("Invalid BCFS filesystem".into());
    }
    let disk = &filesystem[4..];
    let mut score = None;
    // Exclude every referenced data sector from directory discovery. Musical or
    // embedded data may coincidentally begin with the directory entry marker.
    let mut data_sectors = std::collections::HashSet::new();
    for sector in 1..disk.len() / SECTOR_SIZE {
        let offset = sector * SECTOR_SIZE;
        if data_sectors.contains(&sector) || read_le_i32(disk, offset) != 2 {
            continue;
        }
        let name_bytes = &disk[offset + 4..offset + 131];
        let name_end = name_bytes.iter().position(|&byte| byte == 0).unwrap_or(127);
        let name = &name_bytes[..name_end];
        let size = read_le_i32(disk, offset + 0x8c);
        if size < 0 || size as usize > 64 * 1024 * 1024 {
            return Err("Invalid BCFS entry size".into());
        }
        let mut bytes = Vec::new();
        let mut terminated = false;
        for table in (offset + 0x94..offset + SECTOR_SIZE - 3).step_by(4) {
            let block = read_le_i32(disk, table);
            if block == 0 {
                terminated = true;
                break;
            }
            if block < 1 || block as usize >= disk.len() / SECTOR_SIZE {
                return Err("Invalid BCFS data block".into());
            }
            data_sectors.insert(block as usize);
            if name == b"score.gpif" {
                let block_offset = block as usize * SECTOR_SIZE;
                bytes.extend_from_slice(&disk[block_offset..block_offset + SECTOR_SIZE]);
            }
        }
        if !terminated {
            return Err("Unterminated BCFS block table".into());
        }
        if name == b"score.gpif" {
            if score.is_some() || bytes.len() < size as usize {
                return Err("Duplicate or truncated GPIF".into());
            }
            bytes.truncate(size as usize);
            score = Some((offset, bytes));
        }
    }
    let (entry, bytes) = score.ok_or("score.gpif not found")?;
    let xml = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
    let Some(patched) = update(xml)? else {
        return Ok(None);
    };
    let blocks = patched.len().div_ceil(SECTOR_SIZE);
    if blocks + 1 > (SECTOR_SIZE - 0x94) / 4 {
        return Err("Edited GPIF exceeds BCFS block table capacity".into());
    }
    let first = (filesystem.len() - 4) / SECTOR_SIZE;
    let offset = filesystem.len();
    filesystem.resize(offset + blocks * SECTOR_SIZE, 0);
    filesystem[offset..offset + patched.len()].copy_from_slice(patched.as_bytes());
    filesystem[4 + entry + 0x8c..4 + entry + 0x90]
        .copy_from_slice(&(patched.len() as i32).to_le_bytes());
    filesystem[4 + entry + 0x94..4 + entry + SECTOR_SIZE].fill(0);
    for i in 0..blocks {
        let table = 4 + entry + 0x94 + i * 4;
        filesystem[table..table + 4].copy_from_slice(&((first + i) as i32).to_le_bytes());
    }
    Ok(Some(if compressed {
        compress_bcfz(&filesystem)
    } else {
        filesystem
    }))
}
const SECTOR_SIZE: usize = 0x1000;

/// Bit-level reader for BCFZ decompression.
/// Reads bits MSB-first within each byte.
struct BitStream<'a> {
    data: &'a [u8],
    bit_position: usize,
}

impl<'a> BitStream<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitStream {
            data,
            bit_position: 0,
        }
    }

    fn byte_offset(&self) -> usize {
        self.bit_position / 8
    }

    fn is_eof(&self) -> bool {
        self.byte_offset() >= self.data.len()
    }

    /// Read a single bit (MSB-first within the current byte).
    fn read_bit(&mut self) -> u8 {
        let byte_index = self.bit_position / 8;
        let bit_index = 7 - (self.bit_position % 8);
        if byte_index >= self.data.len() {
            return 0;
        }
        self.bit_position += 1;
        (self.data[byte_index] >> bit_index) & 1
    }

    /// Read `count` bits, accumulated big-endian (MSB first).
    fn read_bits(&mut self, count: usize) -> u32 {
        let mut result: u32 = 0;
        for _ in 0..count {
            result = (result << 1) | self.read_bit() as u32;
        }
        result
    }

    /// Read `count` bits, accumulated little-endian (LSB first / "reversed").
    fn read_bits_reversed(&mut self, count: usize) -> u32 {
        let mut result: u32 = 0;
        for i in 0..count {
            result |= (self.read_bit() as u32) << i;
        }
        result
    }
}

/// Decompress a BCFZ-compressed buffer into raw BCFS data.
fn decompress_bcfz(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < 8 {
        return Err("BCFZ data too short".to_string());
    }
    if &data[0..4] != BCFZ_MAGIC {
        return Err(format!("Expected BCFZ magic, got {:?}", &data[0..4]));
    }

    let raw_len = i32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    if raw_len < 0 {
        return Err(format!("BCFZ: negative expected length {}", raw_len));
    }
    let expected_len = raw_len as usize;
    let mut output = Vec::with_capacity(expected_len);
    let mut bits = BitStream::new(&data[8..]);

    while output.len() < expected_len && !bits.is_eof() {
        let flag = bits.read_bit();
        if flag == 1 {
            // Back-reference (LZ77-style)
            let word_size = bits.read_bits(4) as usize;
            let offset = bits.read_bits_reversed(word_size) as usize;
            let size = bits.read_bits_reversed(word_size) as usize;
            if offset == 0 || offset > output.len() {
                return Err(format!(
                    "BCFZ: invalid back-reference offset {} (output len {})",
                    offset,
                    output.len()
                ));
            }
            let source_start = output.len() - offset;
            // LZ77 overlapping copy: when size > offset the source overlaps
            // the destination, so we must copy byte-by-byte with modular indexing.
            for i in 0..size {
                let byte = output[source_start + (i % offset)];
                output.push(byte);
            }
        } else {
            // Literal bytes
            let size = bits.read_bits_reversed(2) as usize;
            for _ in 0..size {
                let byte = bits.read_bits(8) as u8;
                output.push(byte);
            }
        }
    }

    output.truncate(expected_len);
    Ok(output)
}

/// A file extracted from a BCFS virtual filesystem.
struct BcfsFile {
    name: String,
    data: Vec<u8>,
}

/// Read the integer at the given offset (little-endian i32).
fn read_le_i32(data: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

/// Parse the BCFS virtual filesystem and extract all files.
///
/// The BCFS format starts with a 4-byte magic ("BCFS"), followed by sector-based data.
/// The Java reference implementation (TuxGuitar) strips the 4-byte magic and then treats
/// the remaining data as a virtual disk with 0x1000-byte sectors.
fn parse_bcfs(data: &[u8]) -> Result<Vec<BcfsFile>, String> {
    if data.len() < 4 {
        return Err("BCFS data too short".to_string());
    }
    if &data[0..4] != BCFS_MAGIC {
        return Err(format!("Expected BCFS magic, got {:?}", &data[0..4]));
    }

    // Strip the 4-byte magic — all sector offsets are relative to this base.
    let disk = &data[4..];
    let mut files = Vec::new();
    let mut sector_offset = SECTOR_SIZE; // Skip sector 0 (header area)

    while sector_offset + 3 < disk.len() {
        let entry_type = read_le_i32(disk, sector_offset);

        if entry_type == 2 {
            // File directory entry — requires at least 0x98 bytes from sector_offset
            if sector_offset + 0x98 > disk.len() {
                sector_offset += SECTOR_SIZE;
                continue;
            }

            let name_start = sector_offset + 4;
            let name_end = (name_start + 127).min(disk.len());
            let name_bytes = &disk[name_start..name_end];
            let name_len = name_bytes
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(name_bytes.len());
            let name = String::from_utf8_lossy(&name_bytes[..name_len]).to_string();

            let file_size = read_le_i32(disk, sector_offset + 0x8C) as usize;

            // Block index table at +0x94, array of i32, terminated by 0
            let mut file_data = Vec::with_capacity(file_size);
            let mut idx_offset = sector_offset + 0x94;
            loop {
                if idx_offset + 4 > sector_offset + SECTOR_SIZE {
                    break;
                }
                let block_idx = read_le_i32(disk, idx_offset);
                if block_idx == 0 {
                    break;
                }
                let block_start = block_idx as usize * SECTOR_SIZE;
                let block_end = (block_start + SECTOR_SIZE).min(disk.len());
                if block_start < disk.len() {
                    file_data.extend_from_slice(&disk[block_start..block_end]);
                }
                idx_offset += 4;
            }

            file_data.truncate(file_size);
            if !name.is_empty() {
                files.push(BcfsFile {
                    name,
                    data: file_data,
                });
            }
        }

        sector_offset += SECTOR_SIZE;
    }

    Ok(files)
}

/// Reads a .gpx (GP6) file which is a BCFZ/BCFS container holding 'score.gpif'.
pub fn read_gpx(data: &[u8]) -> GpResult<Gpif> {
    let decompressed = decompress_bcfz(data)?;
    let files = parse_bcfs(&decompressed)?;

    let score_file = files
        .iter()
        .find(|f| f.name == "score.gpif")
        .ok_or_else(|| {
            let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
            format!(
                "score.gpif not found in GPX archive. Files found: {:?}",
                names
            )
        })?;

    let xml_str = std::str::from_utf8(&score_file.data)
        .map_err(|e| format!("UTF-8 error in score.gpif: {}", e))?;

    let gpif: Gpif =
        from_str(xml_str).map_err(|e| format!("XML parse error in score.gpif: {}", e))?;

    Ok(gpif)
}

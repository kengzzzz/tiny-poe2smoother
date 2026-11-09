use super::hashing::murmur2_32;
use crate::backup::GgpkBackupMeta;
use anyhow::{Context, Result, anyhow, bail};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::collections::HashSet;
use std::fs;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::Path;

pub struct GgpkArchive {
    pub mmap: memmap2::Mmap,
    utf32_paths: bool,
}

impl GgpkArchive {
    pub fn from_file(path: &Path) -> Result<Self> {
        let file =
            fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
        let mmap = unsafe { memmap2::Mmap::map(&file) }
            .with_context(|| format!("failed to map {}", path.display()))?;
        if mmap.len() < 12 {
            bail!("{} is too small to be a GGPK archive", path.display());
        }
        let version = u32::from_le_bytes(mmap[8..12].try_into().expect("length checked"));
        // Versions 2 and 3 store UTF-16 names, 4 and newer store UTF-32.
        Ok(Self {
            mmap,
            utf32_paths: !matches!(version, 2 | 3),
        })
    }

    pub(super) fn use_utf32(&self) -> bool {
        self.utf32_paths
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct GgpkFileSpan {
    pub(super) begin: usize,
    pub(super) len: usize,
    pub(super) digest: [u8; 32],
}

pub(super) struct GgpkPatchPlan {
    pub(super) backup: GgpkBackupMeta,
    pub(super) append_bytes: Vec<u8>,
    pub(super) new_bundles2_offset: u64,
}

#[derive(Debug, Clone)]
pub(super) struct GgpkPdirRecord {
    pub(super) name: String,
    pub(super) offset: u64,
    pub(super) digest: [u8; 32],
    pub(super) entries: Vec<GgpkPdirEntry>,
    pub(super) parent_offset_pos: Option<u64>,
}

#[derive(Debug, Clone)]
pub(super) struct GgpkPdirEntry {
    pub(super) name: String,
    pub(super) name_hash: u32,
    pub(super) offset: u64,
}

pub(super) fn ggpk_find_file(ggpk: &GgpkArchive, wanted: &str) -> Result<Option<GgpkFileSpan>> {
    ggpk_find_record(ggpk, 0, "", wanted, &mut Vec::new())
}

pub(super) fn ggpk_find_pdir(ggpk: &GgpkArchive, wanted: &str) -> Result<Option<GgpkPdirRecord>> {
    ggpk_find_pdir_record(ggpk, 0, "", wanted, None, &mut Vec::new())
}

fn ggpk_find_pdir_record(
    ggpk: &GgpkArchive,
    offset: u64,
    base: &str,
    wanted: &str,
    parent_offset_pos: Option<u64>,
    ancestors: &mut Vec<u64>,
) -> Result<Option<GgpkPdirRecord>> {
    check_traversal_offset(ggpk, offset, ancestors)?;
    ancestors.push(offset);
    let mut cursor = Cursor::new(ggpk.mmap.as_ref());
    cursor.set_position(offset);

    let _record_size = cursor.read_u32::<LittleEndian>()? as u64;
    let record_tag = read_ggpk_record_tag(&mut cursor)?;
    let result = (|| match record_tag.as_str() {
        "GGPK" => {
            let _version = cursor.read_u32::<LittleEndian>()?;
            let records = (_record_size.saturating_sub(12)) / 8;
            for _ in 0..records {
                let child_offset_pos = cursor.position();
                let child_offset = cursor.read_u64::<LittleEndian>()?;
                if let Some(record) = ggpk_find_pdir_record(
                    ggpk,
                    child_offset,
                    base,
                    wanted,
                    Some(child_offset_pos),
                    ancestors,
                )? {
                    return Ok(Some(record));
                }
            }
            Ok(None)
        }
        "PDIR" => {
            let _name_length = cursor.read_u32::<LittleEndian>()?;
            let entries_len = cursor.read_u32::<LittleEndian>()?;
            let mut digest = [0u8; 32];
            cursor.read_exact(&mut digest)?;
            let name = read_ggpk_string(&mut cursor, ggpk.use_utf32())?;
            let path = join_ggpk_path(base, &name);
            if path == wanted {
                return Ok(Some(GgpkPdirRecord {
                    name,
                    offset,
                    digest,
                    entries: read_pdir_entries(ggpk, cursor.position(), entries_len)?,
                    parent_offset_pos,
                }));
            }
            if !path_could_contain(&path, wanted) {
                return Ok(None);
            }

            let entries_start = cursor.position();
            for i in 0..entries_len {
                let mut entry = Cursor::new(ggpk.mmap.as_ref());
                entry.set_position(entries_start + u64::from(i) * 12 + 4);
                let offset_pos = entry.position();
                let child_offset = entry.read_u64::<LittleEndian>()?;
                if let Some(record) = ggpk_find_pdir_record(
                    ggpk,
                    child_offset,
                    &path,
                    wanted,
                    Some(offset_pos),
                    ancestors,
                )? {
                    return Ok(Some(record));
                }
            }
            Ok(None)
        }
        "FILE" | "FREE" => Ok(None),
        other => bail!("unsupported GGPK record type {other}"),
    })();
    ancestors.pop();
    result
}

fn read_pdir_entries(
    ggpk: &GgpkArchive,
    entries_start: u64,
    entries_len: u32,
) -> Result<Vec<GgpkPdirEntry>> {
    let mut entries = Vec::with_capacity(entries_len as usize);
    let mut entry = Cursor::new(ggpk.mmap.as_ref());
    entry.set_position(entries_start);
    for _ in 0..entries_len {
        let name_hash = entry.read_u32::<LittleEndian>()?;
        let offset = entry.read_u64::<LittleEndian>()?;
        entries.push(GgpkPdirEntry {
            name: ggpk_record_name(ggpk, offset)?,
            name_hash,
            offset,
        });
    }
    Ok(entries)
}

fn read_file_record(ggpk: &GgpkArchive, offset: u64) -> Result<(String, GgpkFileSpan)> {
    let mut cursor = Cursor::new(ggpk.mmap.as_ref());
    cursor.set_position(offset);
    let record_size = u64::from(cursor.read_u32::<LittleEndian>()?);
    if read_ggpk_record_tag(&mut cursor)? != "FILE" {
        bail!("GGPK record at {offset} is not a file");
    }
    let _name_length = cursor.read_u32::<LittleEndian>()?;
    let mut digest = [0u8; 32];
    cursor.read_exact(&mut digest)?;
    let name = read_ggpk_string(&mut cursor, ggpk.use_utf32())?;
    let data_start = cursor.position() as usize;
    let record_end = usize::try_from(offset + record_size)?;
    if record_end < data_start || record_end > ggpk.mmap.len() {
        bail!("GGPK file record {name} has invalid bounds");
    }
    Ok((
        name,
        GgpkFileSpan {
            begin: data_start,
            len: record_end - data_start,
            digest,
        },
    ))
}

pub(super) fn ggpk_pdir_at(ggpk: &GgpkArchive, offset: u64) -> Result<Option<GgpkPdirRecord>> {
    if offset
        .checked_add(16)
        .is_none_or(|end| end > ggpk.mmap.len() as u64)
    {
        return Ok(None);
    }
    let mut cursor = Cursor::new(ggpk.mmap.as_ref());
    cursor.set_position(offset);
    let _record_size = cursor.read_u32::<LittleEndian>()?;
    if read_ggpk_record_tag(&mut cursor)? != "PDIR" {
        return Ok(None);
    }
    let _name_length = cursor.read_u32::<LittleEndian>()?;
    let entries_len = cursor.read_u32::<LittleEndian>()?;
    let mut digest = [0u8; 32];
    cursor.read_exact(&mut digest)?;
    let name = read_ggpk_string(&mut cursor, ggpk.use_utf32())?;
    Ok(Some(GgpkPdirRecord {
        name,
        offset,
        digest,
        entries: read_pdir_entries(ggpk, cursor.position(), entries_len)?,
        parent_offset_pos: None,
    }))
}

pub(super) fn ggpk_file_at(ggpk: &GgpkArchive, offset: u64) -> Result<Option<GgpkFileSpan>> {
    if offset
        .checked_add(8)
        .is_none_or(|end| end > ggpk.mmap.len() as u64)
        || ggpk.mmap[offset as usize + 4..offset as usize + 8] != *b"FILE"
    {
        return Ok(None);
    }
    read_file_record(ggpk, offset).map(|(_, span)| Some(span))
}

/// True when a live or free record reaches `tail_start`, so cutting the file there loses data.
pub(super) fn ggpk_tail_in_use(ggpk: &GgpkArchive, tail_start: u64) -> Result<bool> {
    let data = ggpk.mmap.as_ref();
    let char_size = if ggpk.use_utf32() { 4 } else { 2 };
    let mut seen = HashSet::new();
    let mut pending = vec![0u64];
    while let Some(offset) = pending.pop() {
        if !seen.insert(offset) {
            continue;
        }
        let start = usize::try_from(offset)?;
        let Some(header) = data.get(start..start.saturating_add(8)) else {
            return Ok(true);
        };
        let size = u32::from_le_bytes(header[..4].try_into()?) as usize;
        if offset.saturating_add(size.max(8) as u64) > tail_start {
            return Ok(true);
        }
        let Some(record) = data.get(start..start + size) else {
            return Ok(true);
        };
        let u32_at = |pos: usize| -> Result<u32> {
            Ok(u32::from_le_bytes(
                record
                    .get(pos..pos + 4)
                    .context("short GGPK record")?
                    .try_into()?,
            ))
        };
        let u64_at = |pos: usize| -> Result<u64> {
            Ok(u64::from_le_bytes(
                record
                    .get(pos..pos + 8)
                    .context("short GGPK record")?
                    .try_into()?,
            ))
        };
        match &header[4..8] {
            b"GGPK" => {
                for i in 0..size.saturating_sub(12) / 8 {
                    pending.push(u64_at(12 + i * 8)?);
                }
            }
            b"PDIR" => {
                let name_len = u32_at(8)? as usize;
                let entries = u32_at(12)? as usize;
                let first = 16 + 32 + name_len * char_size;
                for i in 0..entries {
                    pending.push(u64_at(first + i * 12 + 4)?);
                }
            }
            b"FREE" => match u64_at(8)? {
                0 => {}
                next => pending.push(next),
            },
            b"FILE" => {}
            _ => return Ok(true),
        }
    }
    Ok(false)
}

fn check_traversal_offset(ggpk: &GgpkArchive, offset: u64, ancestors: &[u64]) -> Result<()> {
    if usize::try_from(offset)
        .ok()
        .and_then(|offset| offset.checked_add(8))
        .is_none_or(|end| end > ggpk.mmap.len())
    {
        bail!("GGPK record offset {offset} is outside Content.ggpk");
    }
    if ancestors.contains(&offset) {
        bail!("GGPK record cycle at offset {offset}");
    }
    if ancestors.len() >= 128 {
        bail!("GGPK directory nesting exceeds 128 records");
    }
    Ok(())
}

fn ggpk_find_record(
    ggpk: &GgpkArchive,
    offset: u64,
    base: &str,
    wanted: &str,
    ancestors: &mut Vec<u64>,
) -> Result<Option<GgpkFileSpan>> {
    check_traversal_offset(ggpk, offset, ancestors)?;
    ancestors.push(offset);
    let mut cursor = Cursor::new(ggpk.mmap.as_ref());
    cursor.set_position(offset);

    let record_size = cursor.read_u32::<LittleEndian>()? as u64;
    let record_tag = read_ggpk_record_tag(&mut cursor)?;
    let result = (|| match record_tag.as_str() {
        "GGPK" => {
            let _version = cursor.read_u32::<LittleEndian>()?;
            let records = (record_size.saturating_sub(12)) / 8;
            for _ in 0..records {
                let child_offset = cursor.read_u64::<LittleEndian>()?;
                if let Some(span) = ggpk_find_record(ggpk, child_offset, base, wanted, ancestors)? {
                    return Ok(Some(span));
                }
            }
            Ok(None)
        }
        "PDIR" => {
            let _name_length = cursor.read_u32::<LittleEndian>()?;
            let entries_len = cursor.read_u32::<LittleEndian>()?;
            cursor.seek(SeekFrom::Current(32))?;
            let name = read_ggpk_string(&mut cursor, ggpk.use_utf32())?;
            let path = join_ggpk_path(base, &name);
            if !path_could_contain(&path, wanted) {
                return Ok(None);
            }

            let entries_start = cursor.position();
            for i in 0..entries_len {
                let mut entry = Cursor::new(ggpk.mmap.as_ref());
                entry.set_position(entries_start + u64::from(i) * 12);
                entry.seek(SeekFrom::Current(4))?;
                let child_offset = entry.read_u64::<LittleEndian>()?;
                if let Some(span) = ggpk_find_record(ggpk, child_offset, &path, wanted, ancestors)?
                {
                    return Ok(Some(span));
                }
            }
            Ok(None)
        }
        "FILE" => {
            let (filename, span) = read_file_record(ggpk, offset)?;
            Ok((join_ggpk_path(base, &filename) == wanted).then_some(span))
        }
        "FREE" => Ok(None),
        other => bail!("unsupported GGPK record type {other}"),
    })();
    ancestors.pop();
    result
}

fn ggpk_record_name(ggpk: &GgpkArchive, offset: u64) -> Result<String> {
    if offset as usize + 16 > ggpk.mmap.len() {
        bail!("GGPK record offset {offset} is outside Content.ggpk");
    }
    let mut cursor = Cursor::new(ggpk.mmap.as_ref());
    cursor.set_position(offset);
    let _record_size = cursor.read_u32::<LittleEndian>()?;
    let record_tag = read_ggpk_record_tag(&mut cursor)?;
    match record_tag.as_str() {
        "PDIR" => {
            let _name_length = cursor.read_u32::<LittleEndian>()?;
            let _entries_len = cursor.read_u32::<LittleEndian>()?;
            cursor.seek(SeekFrom::Current(32))?;
            read_ggpk_string(&mut cursor, ggpk.use_utf32())
        }
        "FILE" => {
            let _name_length = cursor.read_u32::<LittleEndian>()?;
            cursor.seek(SeekFrom::Current(32))?;
            read_ggpk_string(&mut cursor, ggpk.use_utf32())
        }
        other => bail!("expected GGPK named record, found {other}"),
    }
}

fn read_ggpk_record_tag(cursor: &mut Cursor<&[u8]>) -> Result<String> {
    let mut bytes = [0u8; 4];
    cursor.read_exact(&mut bytes)?;
    String::from_utf8(bytes.to_vec()).context("GGPK record tag is not UTF-8")
}

fn read_ggpk_string(cursor: &mut Cursor<&[u8]>, utf32: bool) -> Result<String> {
    if utf32 {
        let mut chars = Vec::new();
        loop {
            let value = cursor.read_u32::<LittleEndian>()?;
            if value == 0 {
                break;
            }
            chars.push(std::char::from_u32(value).ok_or_else(|| {
                anyhow!("GGPK path contains invalid UTF-32 scalar value {value:#x}")
            })?);
        }
        Ok(chars.into_iter().collect())
    } else {
        let mut units = Vec::new();
        loop {
            let value = cursor.read_u16::<LittleEndian>()?;
            if value == 0 {
                break;
            }
            units.push(value);
        }
        String::from_utf16(&units).context("GGPK path is not valid UTF-16")
    }
}

fn join_ggpk_path(base: &str, name: &str) -> String {
    match (base.is_empty(), name.is_empty()) {
        (true, true) => String::new(),
        (true, false) => name.to_string(),
        (false, true) => base.to_string(),
        (false, false) => format!("{base}/{name}"),
    }
}

pub(super) fn ggpk_file_record(name: &str, data: &[u8], utf32: bool) -> Result<Vec<u8>> {
    let name_bytes = ggpk_name_bytes(name, utf32);
    let mut out = Vec::new();
    out.write_u32::<LittleEndian>(u32::try_from(
        4 + 4 + 4 + 32 + name_bytes.len() + data.len(),
    )?)?;
    out.extend_from_slice(b"FILE");
    out.write_u32::<LittleEndian>(ggpk_name_units(name)?)?;
    out.extend_from_slice(&[0; 32]);
    out.extend_from_slice(&name_bytes);
    out.extend_from_slice(data);
    Ok(out)
}

pub(super) fn ggpk_pdir_record(
    name: &str,
    digest: &[u8; 32],
    entries: &[GgpkPdirEntry],
    utf32: bool,
) -> Result<Vec<u8>> {
    let name_bytes = ggpk_name_bytes(name, utf32);
    let mut out = Vec::new();
    out.write_u32::<LittleEndian>(u32::try_from(
        4 + 4 + 4 + 4 + 32 + name_bytes.len() + entries.len() * 12,
    )?)?;
    out.extend_from_slice(b"PDIR");
    out.write_u32::<LittleEndian>(ggpk_name_units(name)?)?;
    out.write_u32::<LittleEndian>(u32::try_from(entries.len())?)?;
    out.extend_from_slice(digest);
    out.extend_from_slice(&name_bytes);
    for entry in entries {
        out.write_u32::<LittleEndian>(entry.name_hash)?;
        out.write_u64::<LittleEndian>(entry.offset)?;
    }
    Ok(out)
}

fn ggpk_name_units(name: &str) -> Result<u32> {
    Ok(u32::try_from(name.chars().count() + 1)?)
}

fn ggpk_name_bytes(name: &str, utf32: bool) -> Vec<u8> {
    if utf32 {
        name.chars()
            .map(|ch| ch as u32)
            .chain(std::iter::once(0))
            .flat_map(u32::to_le_bytes)
            .collect()
    } else {
        name.encode_utf16()
            .chain(std::iter::once(0))
            .flat_map(u16::to_le_bytes)
            .collect()
    }
}

pub(super) fn ggpk_name_hash(name: &str) -> u32 {
    let lower = name.to_lowercase();
    let bytes = lower
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    murmur2_32(&bytes)
}

fn path_could_contain(path: &str, wanted: &str) -> bool {
    path.is_empty() || wanted == path || wanted.starts_with(&format!("{path}/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(children: &[u64]) -> Vec<u8> {
        let mut bytes = ((12 + children.len() * 8) as u32).to_le_bytes().to_vec();
        bytes.extend_from_slice(b"GGPK");
        bytes.extend_from_slice(&3u32.to_le_bytes());
        for offset in children {
            bytes.extend_from_slice(&offset.to_le_bytes());
        }
        bytes
    }

    fn assert_bad_traversal(bytes: &[u8], expected: &str) {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("Content.ggpk");
        fs::write(&path, bytes).unwrap();
        let archive = GgpkArchive::from_file(&path).unwrap();
        assert!(
            ggpk_find_file(&archive, "Bundles2/_.index.bin")
                .unwrap_err()
                .to_string()
                .contains(expected)
        );
        assert!(
            ggpk_find_pdir(&archive, "Bundles2")
                .unwrap_err()
                .to_string()
                .contains(expected)
        );
    }

    #[test]
    fn cyclic_archive_returns_an_error_in_both_walkers() {
        assert_bad_traversal(&root(&[0]), "cycle");
        let mut indirect = root(&[20]);
        indirect.extend_from_slice(&root(&[0]));
        assert_bad_traversal(&indirect, "cycle");
    }

    #[test]
    fn excessively_deep_archive_returns_an_error_in_both_walkers() {
        let mut bytes = Vec::new();
        for depth in 0..130 {
            bytes.extend_from_slice(&root(&[(depth + 1) * 20]));
        }
        bytes.extend_from_slice(&8u32.to_le_bytes());
        bytes.extend_from_slice(b"FREE");
        assert_bad_traversal(&bytes, "nesting exceeds");
    }

    #[test]
    fn overflowing_record_offset_returns_an_error_in_both_walkers() {
        assert_bad_traversal(&root(&[u64::MAX]), "outside");
    }

    #[test]
    fn a_free_record_in_the_tail_counts_as_in_use() {
        let pdir = ggpk_pdir_record("", &[0; 32], &[], false).unwrap();
        let free = 28 + pdir.len() as u64;
        let mut bytes = root(&[28, free]);
        bytes.extend_from_slice(&pdir);
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(b"FREE");
        bytes.extend_from_slice(&0u64.to_le_bytes());
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("Content.ggpk");
        fs::write(&path, &bytes).unwrap();
        let archive = GgpkArchive::from_file(&path).unwrap();

        assert!(ggpk_tail_in_use(&archive, free).unwrap());
        assert!(!ggpk_tail_in_use(&archive, bytes.len() as u64).unwrap());
    }

    #[test]
    fn repeated_sibling_offsets_are_not_cycles() {
        for record in [
            ggpk_pdir_record("Other", &[0; 32], &[], false).unwrap(),
            ggpk_file_record("other.bin", b"data", false).unwrap(),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("Content.ggpk");
            let mut bytes = root(&[28, 28]);
            bytes.extend_from_slice(&record);
            fs::write(&path, bytes).unwrap();
            let archive = GgpkArchive::from_file(&path).unwrap();
            assert!(
                ggpk_find_file(&archive, "Bundles2/_.index.bin")
                    .unwrap()
                    .is_none()
            );
            assert!(ggpk_find_pdir(&archive, "Bundles2").unwrap().is_none());
        }
    }
}

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

            for (offset_pos, child_offset) in
                lookup_order(ggpk, cursor.position(), entries_len, &path, wanted)?
            {
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

/// Entries with a matching name hash come first, so a cold disk reads less.
fn lookup_order(
    ggpk: &GgpkArchive,
    entries_start: u64,
    entries_len: u32,
    path: &str,
    wanted: &str,
) -> Result<Vec<(u64, u64)>> {
    let next = wanted
        .get(path.len()..)
        .unwrap_or_default()
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or_default();
    let next_hash = ggpk_name_hash(next);
    let mut cursor = Cursor::new(ggpk.mmap.as_ref());
    cursor.set_position(entries_start);
    let (mut hits, mut misses) = (Vec::new(), Vec::new());
    for _ in 0..entries_len {
        let hash = cursor.read_u32::<LittleEndian>()?;
        let entry = (cursor.position(), cursor.read_u64::<LittleEndian>()?);
        if hash == next_hash {
            hits.push(entry);
        } else {
            misses.push(entry);
        }
    }
    hits.append(&mut misses);
    Ok(hits)
}

fn read_pdir_entries(
    ggpk: &GgpkArchive,
    entries_start: u64,
    entries_len: u32,
) -> Result<Vec<GgpkPdirEntry>> {
    let room = ggpk.mmap.len().saturating_sub(entries_start as usize) / 12;
    let mut entries = Vec::with_capacity((entries_len as usize).min(room));
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

pub(super) fn is_generated_bundle(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with("tinypoe2smoother_") && name.ends_with(".bundle.bin")
}

fn record_header(data: &[u8], offset: u64) -> Option<(u64, [u8; 4])> {
    let start = usize::try_from(offset).ok()?;
    let bytes = data.get(start..start.checked_add(8)?)?;
    Some((
        u64::from(u32::from_le_bytes(bytes[..4].try_into().ok()?)),
        bytes[4..8].try_into().ok()?,
    ))
}

fn u64_at(data: &[u8], offset: u64) -> Option<u64> {
    let start = usize::try_from(offset).ok()?;
    Some(u64::from_le_bytes(
        data.get(start..start.checked_add(8)?)?.try_into().ok()?,
    ))
}

fn header_children(data: &[u8]) -> Option<Vec<(u64, u64, [u8; 4])>> {
    let (header_size, _) = record_header(data, 0)?;
    (0..header_size.saturating_sub(12) / 8)
        .map(|i| {
            let offset = u64_at(data, 12 + i * 8)?;
            let (size, tag) = record_header(data, offset)?;
            Some((offset, offset.saturating_add(size), tag))
        })
        .collect()
}

fn free_spans(data: &[u8]) -> Option<Vec<(u64, u64)>> {
    let mut next = header_children(data)?
        .into_iter()
        .find(|&(_, _, tag)| &tag == b"FREE")
        .map_or(0, |(offset, _, _)| offset);
    let mut spans = Vec::new();
    let mut seen = HashSet::new();
    while next != 0 {
        let (size, tag) = record_header(data, next)?;
        if &tag != b"FREE" || !seen.insert(next) {
            return None;
        }
        spans.push((next, next.saturating_add(size)));
        next = u64_at(data, next + 8)?;
    }
    Some(spans)
}

/// Also true when the FREE chain is broken.
pub(super) fn ggpk_records_freed(ggpk: &GgpkArchive, offsets: &[u64]) -> bool {
    let data = ggpk.mmap.as_ref();
    let Some(free) = free_spans(data) else {
        return true;
    };
    offsets.iter().any(|&offset| {
        record_header(data, offset).is_none_or(|(size, _)| {
            let end = offset.saturating_add(size);
            free.iter()
                .any(|&(free_start, free_end)| offset < free_end && free_start < end)
        })
    })
}

/// True when cutting at `tail_start` could lose data. Only our three records may sit there.
pub(super) fn ggpk_tail_in_use(ggpk: &GgpkArchive, tail_start: u64) -> bool {
    let data = ggpk.mmap.as_ref();
    let len = data.len() as u64;
    let crosses = |offset: u64| {
        offset >= tail_start
            || record_header(data, offset)
                .is_none_or(|(size, _)| offset.saturating_add(size) > tail_start)
    };

    let Some(children) = header_children(data) else {
        return true;
    };
    if children.iter().any(|&(_, end, _)| end > tail_start)
        || free_spans(data).is_none_or(|free| free.iter().any(|&(_, end)| end > tail_start))
    {
        return true;
    }
    let mut pdirs = children.iter().filter(|&&(_, _, tag)| &tag == b"PDIR");
    let (Some(&(root, _, _)), None) = (pdirs.next(), pdirs.next()) else {
        return true;
    };
    let Some(root) = ggpk_pdir_at(ggpk, root).ok().flatten() else {
        return true;
    };
    if root.entries.iter().any(|entry| crosses(entry.offset)) {
        return true;
    }
    let Some(bundles2) = root
        .entries
        .iter()
        .find(|entry| entry.name == "Bundles2")
        .and_then(|entry| ggpk_pdir_at(ggpk, entry.offset).ok().flatten())
    else {
        return true;
    };
    if bundles2.entries.iter().any(|entry| crosses(entry.offset)) {
        return true;
    }

    let expected: [fn(&[u8; 4], &str) -> bool; 3] = [
        |tag, name| tag == b"FILE" && name.eq_ignore_ascii_case("_.index.bin"),
        |tag, name| tag == b"FILE" && is_generated_bundle(name),
        |tag, name| tag == b"PDIR" && name == "Bundles2",
    ];
    let mut offset = tail_start;
    for matches in expected {
        if offset >= len {
            return false;
        }
        // A killed Apply can leave a cut or zero-filled record.
        let Some((size, tag)) = record_header(data, offset) else {
            return offset == tail_start;
        };
        if size == 0 && tag == [0; 4] {
            return data[offset as usize..].iter().any(|&byte| byte != 0);
        }
        if (&tag != b"FILE" && &tag != b"PDIR") || size < 8 {
            return true;
        }
        // Only our FILE records have an all-zero digest.
        let digest = data.get(offset as usize + 12..).unwrap_or_default();
        if &tag == b"FILE" && digest.iter().take(32).any(|&byte| byte != 0) {
            return true;
        }
        let complete = offset.saturating_add(size) <= len;
        match ggpk_record_name(ggpk, offset) {
            Ok(name) if matches(&tag, &name) => {}
            Ok(_) => return true,
            Err(_) => return complete,
        }
        offset = offset.saturating_add(size);
    }
    offset < len
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

            for (_, child_offset) in
                lookup_order(ggpk, cursor.position(), entries_len, &path, wanted)?
            {
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
    if usize::try_from(offset)
        .ok()
        .and_then(|offset| offset.checked_add(16))
        .is_none_or(|end| end > ggpk.mmap.len())
    {
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

    fn free(size: u32) -> Vec<u8> {
        let mut bytes = size.to_le_bytes().to_vec();
        bytes.extend_from_slice(b"FREE");
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes
    }

    fn file(name: &str, digest: u8, data: &[u8]) -> Vec<u8> {
        let mut bytes = ggpk_file_record(name, data, false).unwrap();
        bytes[12..44].fill(digest);
        bytes
    }

    /// Returns the bytes and each record's offset.
    fn build(
        records: &[Vec<u8>],
        referenced: &[usize],
        free_head: Option<usize>,
    ) -> (Vec<u8>, Vec<u64>) {
        let entry = |offset| GgpkPdirEntry {
            name: String::new(),
            name_hash: 0,
            offset,
        };
        let root_size = ggpk_pdir_record("", &[0; 32], &[entry(0)], false)
            .unwrap()
            .len() as u64;
        let bundles2_size = ggpk_pdir_record(
            "Bundles2",
            &[0; 32],
            &vec![entry(0); referenced.len()],
            false,
        )
        .unwrap()
        .len() as u64;
        let mut next = 28 + root_size + bundles2_size;
        let offsets = records
            .iter()
            .map(|record| {
                next += record.len() as u64;
                next - record.len() as u64
            })
            .collect::<Vec<_>>();
        let entries = referenced
            .iter()
            .map(|&i| entry(offsets[i]))
            .collect::<Vec<_>>();
        let mut bytes = root(&[28, free_head.map_or(0, |i| offsets[i])]);
        bytes.extend(ggpk_pdir_record("", &[0; 32], &[entry(28 + root_size)], false).unwrap());
        bytes.extend(ggpk_pdir_record("Bundles2", &[0; 32], &entries, false).unwrap());
        bytes.extend(records.concat());
        (bytes, offsets)
    }

    fn tail_in_use(bytes: &[u8], tail_start: u64) -> bool {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("Content.ggpk");
        fs::write(&path, bytes).unwrap();
        ggpk_tail_in_use(&GgpkArchive::from_file(&path).unwrap(), tail_start)
    }

    #[test]
    fn a_free_record_in_the_tail_counts_as_in_use() {
        let (bytes, offsets) = build(&[free(16)], &[], Some(0));
        assert!(tail_in_use(&bytes, offsets[0]));
        assert!(!tail_in_use(&bytes, bytes.len() as u64));

        let index = file("_.index.bin", 0, &free(16));
        let (mut bytes, offsets) = build(&[free(16), index], &[], Some(0));
        let len = bytes.len() as u64;
        assert!(!tail_in_use(&bytes, offsets[1]));
        let link = offsets[0] as usize + 8;
        bytes[link..link + 8].copy_from_slice(&(len - 16).to_le_bytes());
        assert!(tail_in_use(&bytes, offsets[1]));
    }

    #[test]
    fn a_live_record_that_crosses_the_cut_counts_as_in_use() {
        let index = file("_.index.bin", 0, &[0; 32]);
        for (referenced, in_use) in [(&[][..], false), (&[0][..], true)] {
            let (bytes, _) = build(std::slice::from_ref(&index), referenced, None);
            assert_eq!(tail_in_use(&bytes, bytes.len() as u64 - 16), in_use);
        }
    }

    #[test]
    fn a_second_header_directory_counts_as_in_use() {
        let extra = |offset| {
            let entry = GgpkPdirEntry {
                name: String::new(),
                name_hash: 0,
                offset,
            };
            ggpk_pdir_record("Extra", &[0; 32], &[entry], false).unwrap()
        };
        let index = file("_.index.bin", 0, b"index");
        let (_, offsets) = build(&[extra(0), index.clone()], &[], None);
        let (mut bytes, _) = build(&[extra(offsets[1]), index], &[], None);
        assert!(!tail_in_use(&bytes, offsets[1]));
        bytes[20..28].copy_from_slice(&offsets[0].to_le_bytes());
        assert!(tail_in_use(&bytes, offsets[1]));
    }

    #[test]
    fn both_walkers_pick_the_same_duplicate_directory() {
        let entry = |name_hash, offset| GgpkPdirEntry {
            name: String::new(),
            name_hash,
            offset,
        };
        let dir = |file| ggpk_pdir_record("Bundles2", &[0; 32], &[entry(0, file)], false).unwrap();
        let root_size = ggpk_pdir_record("", &[0; 32], &[entry(0, 0), entry(0, 0)], false)
            .unwrap()
            .len() as u64;
        let first = 28 + root_size;
        let second = first + dir(0).len() as u64;
        let first_file = second + dir(0).len() as u64;
        let index = |data: &[u8]| ggpk_file_record("_.index.bin", data, false).unwrap();
        let second_file = first_file + index(b"aaaa").len() as u64;
        let root_entries = [entry(0, first), entry(ggpk_name_hash("Bundles2"), second)];
        let mut bytes = root(&[28, 28]);
        bytes.extend(ggpk_pdir_record("", &[0; 32], &root_entries, false).unwrap());
        bytes.extend(dir(first_file));
        bytes.extend(dir(second_file));
        bytes.extend(index(b"aaaa"));
        bytes.extend(index(b"bbbb"));
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("Content.ggpk");
        fs::write(&path, bytes).unwrap();
        let archive = GgpkArchive::from_file(&path).unwrap();

        let span = ggpk_find_file(&archive, "Bundles2/_.index.bin")
            .unwrap()
            .unwrap();
        assert_eq!(&archive.mmap[span.begin..span.begin + span.len], b"bbbb");
        let pdir = ggpk_find_pdir(&archive, "Bundles2").unwrap().unwrap();
        assert_eq!(pdir.offset, second);
    }

    #[test]
    fn only_our_records_may_sit_in_the_tail() {
        for (digest, in_use) in [(0, false), (7, true)] {
            let (bytes, offsets) = build(&[file("_.index.bin", digest, b"index")], &[], None);
            assert_eq!(tail_in_use(&bytes, offsets[0]), in_use);
        }
        let (bytes, offsets) = build(&[file("_.index.bin", 0, b"index"), vec![1; 4]], &[], None);
        assert!(!tail_in_use(&bytes, offsets[0]));
        assert!(tail_in_use(&bytes, offsets[1]));
    }

    #[test]
    fn records_inside_free_space_count_as_freed() {
        let orphan = ggpk_pdir_record("Bundles2", &[0; 32], &[], false).unwrap();
        let (bytes, offsets) = build(&[free(16 + orphan.len() as u32), orphan], &[], Some(0));
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("Content.ggpk");
        fs::write(&path, bytes).unwrap();
        let archive = GgpkArchive::from_file(&path).unwrap();
        let live = ggpk_find_pdir(&archive, "Bundles2").unwrap().unwrap();

        assert!(ggpk_records_freed(&archive, &[offsets[1]]));
        assert!(!ggpk_records_freed(&archive, &[live.offset]));
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

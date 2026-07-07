use super::hashing::murmur2_32;
use crate::backup::GgpkBackupMeta;
use anyhow::{anyhow, bail, Context, Result};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
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
        // Version ids 2 and 3 store names as UTF-16; 4 (and anything newer/unknown) as UTF-32.
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
    ggpk_find_record(ggpk, 0, "", wanted)
}

pub(super) fn ggpk_find_pdir(ggpk: &GgpkArchive, wanted: &str) -> Result<Option<GgpkPdirRecord>> {
    ggpk_find_pdir_record(ggpk, 0, "", wanted, None)
}

fn ggpk_find_pdir_record(
    ggpk: &GgpkArchive,
    offset: u64,
    base: &str,
    wanted: &str,
    parent_offset_pos: Option<u64>,
) -> Result<Option<GgpkPdirRecord>> {
    if offset as usize + 8 > ggpk.mmap.len() {
        bail!("GGPK record offset {offset} is outside Content.ggpk");
    }
    let mut cursor = Cursor::new(ggpk.mmap.as_ref());
    cursor.set_position(offset);

    let _record_size = cursor.read_u32::<LittleEndian>()? as u64;
    let record_tag = read_ggpk_record_tag(&mut cursor)?;
    match record_tag.as_str() {
        "GGPK" => {
            let _version = cursor.read_u32::<LittleEndian>()?;
            let records = (_record_size.saturating_sub(12)) / 8;
            for _ in 0..records {
                let child_offset_pos = cursor.position();
                let child_offset = cursor.read_u64::<LittleEndian>()?;
                if let Some(record) =
                    ggpk_find_pdir_record(ggpk, child_offset, base, wanted, Some(child_offset_pos))?
                {
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
                let entries_start = cursor.position();
                let mut entries = Vec::with_capacity(entries_len as usize);
                for i in 0..entries_len {
                    let entry_pos = entries_start + u64::from(i) * 12;
                    let mut entry = Cursor::new(ggpk.mmap.as_ref());
                    entry.set_position(entry_pos);
                    let name_hash = entry.read_u32::<LittleEndian>()?;
                    let child_offset = entry.read_u64::<LittleEndian>()?;
                    let child_name = ggpk_record_name(ggpk, child_offset)?;
                    entries.push(GgpkPdirEntry {
                        name: child_name,
                        name_hash,
                        offset: child_offset,
                    });
                }
                return Ok(Some(GgpkPdirRecord {
                    name,
                    offset,
                    digest,
                    entries,
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
                if let Some(record) =
                    ggpk_find_pdir_record(ggpk, child_offset, &path, wanted, Some(offset_pos))?
                {
                    return Ok(Some(record));
                }
            }
            Ok(None)
        }
        "FILE" | "FREE" => Ok(None),
        other => bail!("unsupported GGPK record type {other}"),
    }
}

fn ggpk_find_record(
    ggpk: &GgpkArchive,
    offset: u64,
    base: &str,
    wanted: &str,
) -> Result<Option<GgpkFileSpan>> {
    if offset as usize + 8 > ggpk.mmap.len() {
        bail!("GGPK record offset {offset} is outside Content.ggpk");
    }
    let mut cursor = Cursor::new(ggpk.mmap.as_ref());
    cursor.set_position(offset);

    let record_size = cursor.read_u32::<LittleEndian>()? as u64;
    let record_tag = read_ggpk_record_tag(&mut cursor)?;
    match record_tag.as_str() {
        "GGPK" => {
            let _version = cursor.read_u32::<LittleEndian>()?;
            let records = (record_size.saturating_sub(12)) / 8;
            for _ in 0..records {
                let child_offset = cursor.read_u64::<LittleEndian>()?;
                if let Some(span) = ggpk_find_record(ggpk, child_offset, base, wanted)? {
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
                if let Some(span) = ggpk_find_record(ggpk, child_offset, &path, wanted)? {
                    return Ok(Some(span));
                }
            }
            Ok(None)
        }
        "FILE" => {
            let _name_length = cursor.read_u32::<LittleEndian>()?;
            cursor.seek(SeekFrom::Current(32))?;
            let filename = read_ggpk_string(&mut cursor, ggpk.use_utf32())?;
            let path = join_ggpk_path(base, &filename);
            if path != wanted {
                return Ok(None);
            }
            let data_start = cursor.position() as usize;
            let record_end = usize::try_from(offset + record_size)?;
            if record_end < data_start || record_end > ggpk.mmap.len() {
                bail!("GGPK file record for {wanted} has invalid bounds");
            }
            Ok(Some(GgpkFileSpan {
                begin: data_start,
                len: record_end - data_start,
            }))
        }
        "FREE" => Ok(None),
        other => bail!("unsupported GGPK record type {other}"),
    }
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

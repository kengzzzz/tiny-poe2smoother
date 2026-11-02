use super::compression::{decompress_bundle, pack_uncompressed_bundle};
use super::hashing::{HashMode, hash_path};
use anyhow::{Context, Result, anyhow, bail};
use byteorder::{ByteOrder, LittleEndian, ReadBytesExt, WriteBytesExt};
use rayon::prelude::*;
use std::convert::TryFrom;
use std::io::Cursor;

const FILE_RECORD_SIZE: usize = 20;
const DIRECTORY_RECORD_SIZE: usize = 20;

/// The owning bundle is resolved via `BundleIndex::bundle_name` to avoid
/// cloning ~1M strings on parse.
#[derive(Debug, Clone, Copy)]
pub struct BundleFile {
    pub hash: u64,
    pub bundle_index: u32,
    pub offset: u32,
    pub size: u32,
    pub(super) ordinal: u32,
}

#[cfg(test)]
impl BundleFile {
    pub(crate) fn for_test(size: u32) -> Self {
        Self::for_test_with_hash(0, size)
    }

    pub(crate) fn for_test_with_hash(hash: u64, size: u32) -> Self {
        Self {
            hash,
            bundle_index: 0,
            offset: 0,
            size,
            ordinal: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BundleIndex {
    pub(super) raw_decompressed: Vec<u8>,
    pub(super) bundles: Vec<BundleInfo>,
    hash_mode: HashMode,
    file_count_pos: usize,
    /// Open-addressed hash table of record ordinal + 1, where 0 is empty.
    file_slots: Vec<u32>,
    paths: PathList,
}

#[derive(Debug, Clone)]
pub(super) struct BundleInfo {
    pub(super) name: String,
    pub(super) uncompressed_size: u32,
    pub(super) size_pos: usize,
}

#[derive(Debug, Clone)]
pub struct IndexedPath {
    pub path: String,
    pub file: BundleFile,
}

/// NUL-terminated paths in a few large blocks, because 4.27M `String`s fragment the heap.
/// Blocks let the build run in parallel without a second full copy.
#[derive(Debug, Clone, Default)]
pub struct PathList {
    blocks: Vec<String>,
    len: usize,
}

pub struct PathIter<'a> {
    blocks: std::slice::Iter<'a, String>,
    current: std::str::SplitTerminator<'a, char>,
}

impl<'a> Iterator for PathIter<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        loop {
            if let Some(path) = self.current.next() {
                return Some(path);
            }
            self.current = self.blocks.next()?.split_terminator('\0');
        }
    }
}

impl PathList {
    pub(super) fn from_text(text: String) -> Option<Self> {
        (text.is_empty() || text.ends_with('\0')).then(|| Self {
            len: text.bytes().filter(|byte| *byte == 0).count(),
            blocks: vec![text],
        })
    }

    pub(super) fn blocks(&self) -> &[String] {
        &self.blocks
    }

    #[cfg(test)]
    fn push(&mut self, path: &str) {
        if self.blocks.is_empty() {
            self.blocks.push(String::new());
        }
        let block = self.blocks.last_mut().expect("block added above");
        block.push_str(path);
        block.push('\0');
        self.len += 1;
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn iter(&self) -> PathIter<'_> {
        PathIter {
            blocks: self.blocks.iter(),
            current: "".split_terminator('\0'),
        }
    }
}

impl<'a> IntoIterator for &'a PathList {
    type Item = &'a str;
    type IntoIter = PathIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl BundleIndex {
    #[cfg(test)]
    pub(crate) fn for_test_paths(paths: &[(&str, &str, u32)]) -> Self {
        let mut bundle_names = Vec::<&str>::new();
        let mut records = Vec::new();
        for (path, bundle_name, size) in paths {
            let bundle_index = bundle_names
                .iter()
                .position(|name| name == bundle_name)
                .unwrap_or_else(|| {
                    bundle_names.push(bundle_name);
                    bundle_names.len() - 1
                });
            records
                .write_u64::<LittleEndian>(super::hashing::fnv1a_bundle_hash(path))
                .unwrap();
            records
                .write_u32::<LittleEndian>(bundle_index as u32)
                .unwrap();
            records.write_u32::<LittleEndian>(0).unwrap();
            records.write_u32::<LittleEndian>(*size).unwrap();
        }
        let mut raw = Vec::new();
        raw.write_u32::<LittleEndian>(bundle_names.len() as u32)
            .unwrap();
        for name in bundle_names {
            raw.write_u32::<LittleEndian>(name.len() as u32).unwrap();
            raw.extend_from_slice(name.as_bytes());
            raw.write_u32::<LittleEndian>(0).unwrap();
        }
        raw.write_u32::<LittleEndian>(paths.len() as u32).unwrap();
        raw.extend_from_slice(&records);
        raw.write_u32::<LittleEndian>(1).unwrap();
        raw.write_u64::<LittleEndian>(0x07E47507B4A92E53).unwrap();
        raw.extend_from_slice(&[0; 12]);

        let mut index = Self::parse_records(raw).unwrap();
        for (path, _, _) in paths {
            index.paths.push(path);
        }
        index
    }

    pub fn parse(raw_decompressed: Vec<u8>) -> Result<Self> {
        let mut index = Self::parse_records(raw_decompressed)?;
        index.paths = index.build_paths()?;
        Ok(index)
    }

    pub(super) fn with_paths(raw_decompressed: Vec<u8>, paths: PathList) -> Result<Self> {
        let mut index = Self::parse_records(raw_decompressed)?;
        index.paths = paths;
        Ok(index)
    }

    fn parse_records(raw_decompressed: Vec<u8>) -> Result<Self> {
        let mut cursor = Cursor::new(raw_decompressed.as_slice());
        let bundle_count = cursor.read_u32::<LittleEndian>()? as usize;
        // Capped so a corrupt cache can't force a huge allocation.
        let mut bundles = Vec::with_capacity(bundle_count.min(raw_decompressed.len() / 8));

        for _ in 0..bundle_count {
            let len = cursor.read_u32::<LittleEndian>()? as usize;
            let name_pos = usize::try_from(cursor.position())?;
            let size_pos = name_pos + len;
            let name = raw_decompressed
                .get(name_pos..size_pos)
                .context("bundle name exceeds index data")?;
            cursor.set_position(size_pos as u64);
            let uncompressed_size = cursor.read_u32::<LittleEndian>()?;
            bundles.push(BundleInfo {
                name: std::str::from_utf8(name)
                    .context("bundle name is not UTF-8")?
                    .to_string(),
                uncompressed_size,
                size_pos,
            });
        }

        let file_count_pos = usize::try_from(cursor.position())?;
        let file_count = cursor.read_u32::<LittleEndian>()? as usize;
        let records_pos = file_count_pos + 4;
        let records_end = records_pos + file_count * FILE_RECORD_SIZE;
        let records = raw_decompressed
            .get(records_pos..records_end)
            .context("file records exceed index data")?;
        let file_slots = build_file_slots(records, bundles.len())?;

        cursor.set_position(records_end as u64);
        let directory_count = cursor.read_u32::<LittleEndian>()? as usize;
        if directory_count == 0 {
            bail!("index contains no directory records");
        }
        let directories_end = records_end + 4 + directory_count * DIRECTORY_RECORD_SIZE;
        if directories_end > raw_decompressed.len() {
            bail!("directory records exceed index data");
        }
        let hash_mode = match cursor.read_u64::<LittleEndian>()? {
            0xF42A94E69CFF42FE => HashMode::Murmur64A,
            0x07E47507B4A92E53 => HashMode::Fnv1A,
            value => bail!("unsupported index name-hash sentinel {value:#x}"),
        };

        Ok(Self {
            raw_decompressed,
            bundles,
            hash_mode,
            file_count_pos,
            file_slots,
            paths: PathList::default(),
        })
    }

    fn build_paths(&self) -> Result<PathList> {
        let raw = &self.raw_decompressed;
        let file_count = LittleEndian::read_u32(&raw[self.file_count_pos..]) as usize;
        let directory_count_pos = self.record_pos(file_count);
        let directory_count = LittleEndian::read_u32(&raw[directory_count_pos..]) as usize;
        let directories_pos = directory_count_pos + 4;
        let directories_end = directories_pos + directory_count * DIRECTORY_RECORD_SIZE;
        let directories = &raw[directories_pos..directories_end];

        crate::timing!("dir_decompress");
        let directory_bytes = decompress_bundle(&raw[directories_end..])
            .context("failed to decompress index directory data")?;
        crate::timing!("dir_build_paths");
        build_paths_from_directories(&directory_bytes, directories)
    }

    fn record_pos(&self, ordinal: usize) -> usize {
        self.file_count_pos + 4 + ordinal * FILE_RECORD_SIZE
    }

    fn file_record(&self, ordinal: usize) -> BundleFile {
        let pos = self.record_pos(ordinal);
        let record = &self.raw_decompressed[pos..pos + FILE_RECORD_SIZE];
        BundleFile {
            hash: LittleEndian::read_u64(record),
            bundle_index: LittleEndian::read_u32(&record[8..]),
            offset: LittleEndian::read_u32(&record[12..]),
            size: LittleEndian::read_u32(&record[16..]),
            ordinal: ordinal as u32,
        }
    }

    fn file_by_hash(&self, hash: u64) -> Option<BundleFile> {
        let mask = self.file_slots.len() - 1;
        let mut slot = hash as usize & mask;
        loop {
            let ordinal = self.file_slots[slot].checked_sub(1)?;
            let file = self.file_record(ordinal as usize);
            if file.hash == hash {
                return Some(file);
            }
            slot = (slot + 1) & mask;
        }
    }

    pub fn file_by_path(&self, path: &str) -> Option<BundleFile> {
        self.file_by_hash(hash_path(self.hash_mode, path))
    }

    pub fn bundle_name(&self, bundle_index: u32) -> Result<&str> {
        self.bundles
            .get(bundle_index as usize)
            .map(|bundle| bundle.name.as_str())
            .ok_or_else(|| anyhow!("invalid bundle index {bundle_index}"))
    }

    pub fn paths(&self) -> &PathList {
        &self.paths
    }

    pub fn matching_paths_by<F>(&self, mut predicate: F) -> Vec<IndexedPath>
    where
        F: FnMut(&str) -> bool,
    {
        self.paths
            .iter()
            .filter(|path| predicate(path))
            .filter_map(|path| {
                self.file_by_path(path).map(|file| IndexedPath {
                    path: path.to_string(),
                    file,
                })
            })
            .collect()
    }

    pub fn update_file_record(
        &mut self,
        hash: u64,
        bundle_index: u32,
        offset: u32,
        size: u32,
    ) -> Result<()> {
        if bundle_index as usize >= self.bundles.len() {
            bail!("invalid bundle index {bundle_index}");
        }
        let file = self
            .file_by_hash(hash)
            .ok_or_else(|| anyhow!("file hash not found in index: {hash:#x}"))?;

        let pos = self.record_pos(file.ordinal as usize) + 8;
        let mut writer = Cursor::new(&mut self.raw_decompressed[pos..pos + 12]);
        writer.write_u32::<LittleEndian>(bundle_index)?;
        writer.write_u32::<LittleEndian>(offset)?;
        writer.write_u32::<LittleEndian>(size)?;
        Ok(())
    }

    pub fn update_bundle_size(&mut self, bundle_index: u32, uncompressed_size: u32) -> Result<()> {
        let bundle = self
            .bundles
            .get_mut(bundle_index as usize)
            .ok_or_else(|| anyhow!("invalid bundle index {bundle_index}"))?;
        bundle.uncompressed_size = uncompressed_size;
        let mut writer =
            Cursor::new(&mut self.raw_decompressed[bundle.size_pos..bundle.size_pos + 4]);
        writer.write_u32::<LittleEndian>(uncompressed_size)?;
        Ok(())
    }

    pub fn packed_bytes(&self) -> Result<Vec<u8>> {
        pack_uncompressed_bundle(&self.raw_decompressed)
    }

    pub fn has_bundle_prefix(&self, prefix: &str) -> bool {
        self.bundles
            .iter()
            .any(|bundle| bundle.name.starts_with(prefix))
    }

    pub fn create_custom_bundle(&mut self, name_prefix: &str) -> Result<u32> {
        let mut ordinal = 0usize;
        loop {
            let name = format!("{name_prefix}{ordinal}");
            if !self.bundles.iter().any(|bundle| bundle.name == name) {
                let bundle_index = u32::try_from(self.bundles.len())?;
                let mut record = Vec::new();
                record.write_u32::<LittleEndian>(u32::try_from(name.len())?)?;
                record.extend_from_slice(name.as_bytes());
                let size_pos = self.file_count_pos + record.len();
                record.write_u32::<LittleEndian>(0)?;

                self.raw_decompressed.splice(
                    self.file_count_pos..self.file_count_pos,
                    record.iter().copied(),
                );
                self.file_count_pos += record.len();
                self.bundles.push(BundleInfo {
                    name,
                    uncompressed_size: 0,
                    size_pos,
                });
                let mut writer = Cursor::new(&mut self.raw_decompressed[0..4]);
                writer.write_u32::<LittleEndian>(u32::try_from(self.bundles.len())?)?;
                return Ok(bundle_index);
            }
            ordinal += 1;
        }
    }
}

fn build_file_slots(records: &[u8], bundle_count: usize) -> Result<Vec<u32>> {
    let (records, _) = records.as_chunks::<FILE_RECORD_SIZE>();
    // Linear probing stays short under a 3/4 load.
    let len = (records.len() + records.len() / 3 + 1).next_power_of_two();
    let mask = len - 1;
    let mut slots = vec![0u32; len];
    for (ordinal, record) in records.iter().enumerate() {
        let bundle_index = LittleEndian::read_u32(&record[8..]);
        if bundle_index as usize >= bundle_count {
            bail!("file references invalid bundle index {bundle_index}");
        }
        let hash = LittleEndian::read_u64(record);
        let mut slot = hash as usize & mask;
        while slots[slot] != 0 && LittleEndian::read_u64(&records[slots[slot] as usize - 1]) != hash
        {
            slot = (slot + 1) & mask;
        }
        slots[slot] = ordinal as u32 + 1;
    }
    Ok(slots)
}

/// Directory bytes per parallel block, about 9 MB of paths.
const PATH_BLOCK_SOURCE_BYTES: usize = 4 << 20;

fn build_paths_from_directories(bytes: &[u8], directories: &[u8]) -> Result<PathList> {
    let records = directories.as_chunks::<DIRECTORY_RECORD_SIZE>().0;
    let range = |directory: &[u8; DIRECTORY_RECORD_SIZE]| {
        let start = LittleEndian::read_u32(&directory[8..]) as usize;
        start..start + LittleEndian::read_u32(&directory[12..]) as usize
    };
    let mut groups = Vec::new();
    let mut group_start = 0;
    let mut group_bytes = 0;
    for (i, directory) in records.iter().enumerate() {
        let range = range(directory);
        if range.end > bytes.len() {
            bail!("directory path data exceeds decompressed directory bundle");
        }
        group_bytes += range.len();
        if group_bytes >= PATH_BLOCK_SOURCE_BYTES {
            groups.push(&records[group_start..=i]);
            group_start = i + 1;
            group_bytes = 0;
        }
    }
    if group_start < records.len() {
        groups.push(&records[group_start..]);
    }

    let blocks = groups
        .par_iter()
        .map(|group| {
            let source: usize = group.iter().map(|directory| range(directory).len()).sum();
            // Paths run about 2.1x the directory bytes.
            let mut block = String::with_capacity(source * 5 / 2);
            let mut count = 0;
            for directory in *group {
                count += build_paths(&bytes[range(directory)], &mut block)?;
            }
            block.shrink_to_fit();
            Ok((block, count))
        })
        .collect::<Result<Vec<_>>>()?;
    let len = blocks.iter().map(|(_, count)| count).sum();
    Ok(PathList {
        blocks: blocks.into_iter().map(|(block, _)| block).collect(),
        len,
    })
}

/// Appends this directory's paths to `out` and returns how many it added.
fn build_paths(bytes: &[u8], out: &mut String) -> Result<usize> {
    let mut pos = 0;
    let mut count = 0;
    let mut generation_phase = false;
    let mut table: Vec<String> = Vec::new();

    while pos + 4 <= bytes.len() {
        let index = LittleEndian::read_u32(&bytes[pos..]) as usize;
        pos += 4;
        if index == 0 {
            generation_phase = !generation_phase;
            if generation_phase {
                table.clear();
            }
            continue;
        }

        let len = bytes[pos..]
            .iter()
            .position(|&byte| byte == 0)
            .ok_or_else(|| anyhow!("path data ends without a NUL"))?;
        let suffix =
            std::str::from_utf8(&bytes[pos..pos + len]).context("path data is not UTF-8")?;
        pos += len + 1;
        let prefix = table.get(index.wrapping_sub(1)).map_or("", String::as_str);
        if generation_phase {
            table.push([prefix, suffix].concat());
        } else {
            out.push_str(prefix);
            out.push_str(suffix);
            out.push('\0');
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::super::hashing::fnv1a_bundle_hash;
    use super::*;

    const FNV_SENTINEL: u64 = 0x07E47507B4A92E53;

    fn entry(index: u32, text: &str) -> Vec<u8> {
        let mut bytes = index.to_le_bytes().to_vec();
        bytes.extend_from_slice(text.as_bytes());
        bytes.push(0);
        bytes
    }

    fn file_record(hash: u64, bundle_index: u32, offset: u32, size: u32) -> Vec<u8> {
        let mut bytes = hash.to_le_bytes().to_vec();
        for value in [bundle_index, offset, size] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    fn directories_with_prefix_table() -> Vec<u8> {
        let mut first = entry(1, "art/a.dds");
        first.extend(entry(1, "art/b.dds"));
        let mut second = 0u32.to_le_bytes().to_vec();
        second.extend(entry(1, "metadata/"));
        second.extend(0u32.to_le_bytes());
        second.extend(entry(1, "c.ao"));

        let mut raw = 2u32.to_le_bytes().to_vec();
        raw.extend(FNV_SENTINEL.to_le_bytes());
        raw.extend(0u32.to_le_bytes());
        raw.extend((first.len() as u32).to_le_bytes());
        raw.extend(0u32.to_le_bytes());
        raw.extend(1u64.to_le_bytes());
        raw.extend((first.len() as u32).to_le_bytes());
        raw.extend((second.len() as u32).to_le_bytes());
        raw.extend(0u32.to_le_bytes());
        raw.extend(pack_uncompressed_bundle(&[first, second].concat()).unwrap());
        raw
    }

    fn raw_index(bundles: &[(&str, u32)], records: &[Vec<u8>]) -> Vec<u8> {
        let mut raw = (bundles.len() as u32).to_le_bytes().to_vec();
        for (name, size) in bundles {
            raw.extend((name.len() as u32).to_le_bytes());
            raw.extend(name.as_bytes());
            raw.extend(size.to_le_bytes());
        }
        raw.extend((records.len() as u32).to_le_bytes());
        raw.extend(records.concat());
        raw.extend(directories_with_prefix_table());
        raw
    }

    fn test_index() -> Vec<u8> {
        raw_index(
            &[("first", 10), ("second", 20)],
            &[
                file_record(fnv1a_bundle_hash("art/a.dds"), 0, 1, 2),
                file_record(fnv1a_bundle_hash("art/b.dds"), 1, 3, 4),
                file_record(fnv1a_bundle_hash("metadata/c.ao"), 1, 5, 6),
            ],
        )
    }

    fn fields(index: &BundleIndex, path: &str) -> (u32, u32, u32) {
        let file = index.file_by_path(path).unwrap();
        (file.bundle_index, file.offset, file.size)
    }

    #[test]
    fn parse_builds_paths_and_finds_each_record() {
        let index = BundleIndex::parse(test_index()).unwrap();

        assert_eq!(
            index.paths().iter().collect::<Vec<_>>(),
            ["art/a.dds", "art/b.dds", "metadata/c.ao"]
        );
        assert_eq!(index.paths().len(), 3);
        assert_eq!(fields(&index, "art/a.dds"), (0, 1, 2));
        assert_eq!(fields(&index, "Art/B.dds"), (1, 3, 4));
        assert_eq!(fields(&index, "metadata/c.ao"), (1, 5, 6));
        assert!(index.file_by_path("missing").is_none());
        assert_eq!(
            index
                .matching_paths_by(|path| path.starts_with("art/"))
                .iter()
                .map(|entry| (entry.path.as_str(), entry.file.ordinal))
                .collect::<Vec<_>>(),
            [("art/a.dds", 0), ("art/b.dds", 1)]
        );
    }

    #[test]
    fn apply_edits_reach_the_raw_bytes_written_to_disk() {
        let mut index = BundleIndex::parse(test_index()).unwrap();

        let custom = index.create_custom_bundle("TinyPoe2Smoother_").unwrap();
        assert_eq!(custom, 2);
        assert_eq!(fields(&index, "art/b.dds"), (1, 3, 4));
        let hash = fnv1a_bundle_hash("art/b.dds");
        index.update_file_record(hash, custom, 7, 8).unwrap();
        index.update_bundle_size(custom, 15).unwrap();
        assert_eq!(fields(&index, "art/b.dds"), (2, 7, 8));

        let written = BundleIndex::parse(index.raw_decompressed.clone()).unwrap();
        assert_eq!(fields(&written, "art/a.dds"), (0, 1, 2));
        assert_eq!(fields(&written, "art/b.dds"), (2, 7, 8));
        assert_eq!(fields(&written, "metadata/c.ao"), (1, 5, 6));
        assert_eq!(written.bundle_name(2).unwrap(), "TinyPoe2Smoother_0");
        assert_eq!(written.bundles[0].uncompressed_size, 10);
        assert_eq!(written.bundles[1].uncompressed_size, 20);
        assert_eq!(written.bundles[2].uncompressed_size, 15);
        assert_eq!(written.paths().len(), 3);

        let mut unknown = written;
        assert!(unknown.update_file_record(1, 0, 0, 0).is_err());
        assert!(unknown.update_file_record(hash, 3, 0, 0).is_err());
    }

    #[test]
    fn colliding_hashes_probe_to_their_own_record() {
        let hashes: Vec<u64> = (1..=64).map(|high| high << 40).collect();
        let records: Vec<_> = hashes
            .iter()
            .enumerate()
            .map(|(ordinal, hash)| file_record(*hash, 0, ordinal as u32, 0))
            .collect();
        let index = BundleIndex::parse(raw_index(&[("only", 0)], &records)).unwrap();

        for (ordinal, hash) in hashes.iter().enumerate() {
            assert_eq!(index.file_by_hash(*hash).unwrap().offset, ordinal as u32);
        }
        assert!(index.file_by_hash(65 << 40).is_none());
    }

    #[test]
    fn duplicate_hash_resolves_to_the_last_record() {
        let records = [file_record(5, 0, 1, 1), file_record(5, 0, 2, 2)];
        let index = BundleIndex::parse(raw_index(&[("only", 0)], &records)).unwrap();

        assert_eq!(index.file_by_hash(5).unwrap().offset, 2);
    }

    #[test]
    fn empty_index_finds_nothing() {
        let index = BundleIndex::parse(raw_index(&[], &[])).unwrap();

        assert!(index.file_by_hash(0).is_none());
    }

    #[test]
    fn invalid_records_are_rejected() {
        let bad_bundle = raw_index(&[("only", 0)], &[file_record(5, 1, 0, 0)]);
        assert!(BundleIndex::parse(bad_bundle).is_err());

        let empty = raw_index(&[], &[]);
        let mut no_directories = empty.clone();
        no_directories[8..12].fill(0);
        let mut bad_sentinel = empty.clone();
        bad_sentinel[12..20].fill(0);
        let mut absurd_directories = empty;
        absurd_directories[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        let mut absurd_files = 0u32.to_le_bytes().to_vec();
        absurd_files.extend(u32::MAX.to_le_bytes());
        let mut absurd_name = 1u32.to_le_bytes().to_vec();
        absurd_name.extend(u32::MAX.to_le_bytes());

        for raw in [
            no_directories,
            bad_sentinel,
            absurd_directories,
            absurd_files,
            absurd_name,
        ] {
            assert!(BundleIndex::with_paths(raw, PathList::default()).is_err());
        }
    }

    #[test]
    fn path_list_keeps_empty_paths_and_needs_a_final_terminator() {
        let paths = PathList::from_text("a\0\0b\0".to_string()).unwrap();
        assert_eq!(paths.iter().collect::<Vec<_>>(), ["a", "", "b"]);
        assert_eq!(paths.len(), 3);

        assert!(PathList::from_text(String::new()).unwrap().is_empty());
        assert!(PathList::from_text("a".to_string()).is_none());
    }
}

const ROW_HEAP_MARKER: [u8; 8] = [0xbb; 8];

pub struct DatTable<'a> {
    pub row_count: usize,
    pub row_len: usize,
    row_block: &'a [u8],
    heap: &'a [u8],
}

impl<'a> DatTable<'a> {
    pub fn rows(&self) -> impl Iterator<Item = &'a [u8]> + use<'a> {
        self.row_block.chunks_exact(self.row_len)
    }

    pub fn heap(&self) -> &'a [u8] {
        self.heap
    }
}

pub fn parse_table(bytes: &[u8]) -> Option<DatTable<'_>> {
    let row_count = u32::from_le_bytes(bytes.get(0..4)?.try_into().ok()?) as usize;
    if row_count == 0 {
        return None;
    }
    // 0xBB x8 can also occur in row data, so accept it only where rows split evenly.
    let marker_pos = {
        let mut search_from = 4;
        loop {
            let rel = bytes.get(search_from..).and_then(|s| {
                s.windows(ROW_HEAP_MARKER.len())
                    .position(|w| w == ROW_HEAP_MARKER)
            })?;
            let candidate = search_from + rel;
            let block_len = candidate - 4;
            if block_len % row_count == 0 && block_len / row_count >= 8 {
                break candidate;
            }
            search_from = candidate + 1;
        }
    };
    let row_block = bytes.get(4..marker_pos)?;
    let row_len = row_block.len() / row_count;
    let heap = bytes.get(marker_pos..)?;
    Some(DatTable {
        row_count,
        row_len,
        row_block,
        heap,
    })
}

/// `None` for anything that isn't a valid non-empty string, including dat nulls (u64::MAX).
pub fn utf16le_string_at(heap: &[u8], offset: usize) -> Option<String> {
    if heap.starts_with(&ROW_HEAP_MARKER) && offset < ROW_HEAP_MARKER.len() {
        return None;
    }
    let mut units = Vec::new();
    let mut i = offset;
    let mut terminated = false;
    while let Some(chunk) = i.checked_add(2).and_then(|end| heap.get(i..end)) {
        let unit = u16::from_le_bytes([chunk[0], chunk[1]]);
        if unit == 0 {
            terminated = true;
            break;
        }
        units.push(unit);
        i += 2;
        if units.len() > 300 {
            return None;
        }
    }
    if !terminated || units.is_empty() {
        return None;
    }
    String::from_utf16(&units).ok()
}

/// Rows can share heap strings, so edits append new ones instead of overwriting.
pub fn with_strings(bytes: &[u8], edits: &[(usize, usize, &str)]) -> Option<Vec<u8>> {
    let table = parse_table(bytes)?;
    let marker_pos = 4 + table.row_count * table.row_len;
    let mut out = bytes.to_vec();
    for &(row, column, text) in edits {
        if row >= table.row_count || column + 8 > table.row_len {
            return None;
        }
        let offset = (out.len() - marker_pos) as u64;
        for unit in text.encode_utf16() {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out.extend_from_slice(&[0; 4]);
        let cell = 4 + row * table.row_len + column;
        out[cell..cell + 8].copy_from_slice(&offset.to_le_bytes());
    }
    Some(out)
}

pub fn row_strings(row: &[u8], heap: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for field in (0..row.len()).step_by(4) {
        if field + 8 <= row.len() {
            let off = u64::from_le_bytes(row[field..field + 8].try_into().unwrap()) as usize;
            if let Some(value) = utf16le_string_at(heap, off) {
                push_unique(&mut out, value);
            }
        }
        if field + 4 <= row.len() {
            let off = u32::from_le_bytes(row[field..field + 4].try_into().unwrap()) as usize;
            if let Some(value) = utf16le_string_at(heap, off) {
                push_unique(&mut out, value);
            }
        }
    }
    out
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_utf16(heap: &mut Vec<u8>, s: &str) -> u64 {
        let offset = heap.len() as u64;
        for unit in s.encode_utf16() {
            heap.extend_from_slice(&unit.to_le_bytes());
        }
        heap.extend_from_slice(&0u16.to_le_bytes());
        offset
    }

    #[test]
    fn parse_table_skips_false_marker_inside_row_data() {
        let mut heap = ROW_HEAP_MARKER.to_vec();
        let hello_off = push_utf16(&mut heap, "Hello");

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&hello_off.to_le_bytes());
        bytes.extend_from_slice(&ROW_HEAP_MARKER);
        bytes.extend_from_slice(&hello_off.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&hello_off.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&heap);

        let table = parse_table(&bytes).unwrap();

        assert_eq!(table.row_count, 3);
        assert_eq!(table.row_len, 16);
        assert_eq!(
            utf16le_string_at(table.heap(), hello_off as usize),
            Some("Hello".to_string())
        );
    }

    #[test]
    fn utf16le_string_at_handles_sentinel_and_huge_offsets() {
        let mut heap = ROW_HEAP_MARKER.to_vec();
        push_utf16(&mut heap, "Hello");
        // u64::MAX is a common dat null. mods.datc64 used to crash here.
        assert_eq!(utf16le_string_at(&heap, usize::MAX), None);
        assert_eq!(utf16le_string_at(&heap, usize::MAX - 1), None);
        assert_eq!(utf16le_string_at(&heap, heap.len()), None);
        assert_eq!(utf16le_string_at(&heap, heap.len() - 1), None);

        let mut row = Vec::new();
        row.extend_from_slice(&u64::MAX.to_le_bytes());
        assert!(row_strings(&row, &heap).is_empty());
    }

    #[test]
    fn with_strings_appends_and_repoints_only_edited_cells() {
        let mut heap = ROW_HEAP_MARKER.to_vec();
        let shared = push_utf16(&mut heap, "Shared");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&2u32.to_le_bytes());
        for _ in 0..2 {
            bytes.extend_from_slice(&shared.to_le_bytes());
            bytes.extend_from_slice(&shared.to_le_bytes());
        }
        bytes.extend_from_slice(&heap);

        let out = with_strings(&bytes, &[(1, 8, "New")]).unwrap();
        let table = parse_table(&out).unwrap();
        let rows: Vec<_> = table.rows().collect();
        let cell = |row: &[u8], column: usize| {
            let offset = u64::from_le_bytes(row[column..column + 8].try_into().unwrap());
            utf16le_string_at(table.heap(), offset as usize).unwrap()
        };
        assert_eq!(cell(rows[0], 0), "Shared");
        assert_eq!(cell(rows[0], 8), "Shared");
        assert_eq!(cell(rows[1], 0), "Shared");
        assert_eq!(cell(rows[1], 8), "New");
        assert_eq!(&out[..bytes.len() - heap.len()][..4], &bytes[..4]);
        assert!(out.ends_with(&[b'w', 0, 0, 0, 0, 0]));
        assert_eq!(with_strings(&bytes, &[(2, 0, "x")]), None);
        assert_eq!(with_strings(&bytes, &[(0, 9, "x")]), None);
    }

    #[test]
    fn row_strings_collects_32_and_64_bit_offsets_once() {
        let mut heap = ROW_HEAP_MARKER.to_vec();
        let alpha = push_utf16(&mut heap, "Alpha");
        let beta = push_utf16(&mut heap, "Beta") as u32;
        let mut row = Vec::new();
        row.extend_from_slice(&alpha.to_le_bytes());
        row.extend_from_slice(&beta.to_le_bytes());
        row.extend_from_slice(&beta.to_le_bytes());

        assert_eq!(row_strings(&row, &heap), vec!["Alpha", "Beta"]);
    }
}

const ROW_HEAP_MARKER: [u8; 8] = [0xbb; 8];

pub struct DatTable<'a> {
    pub row_count: usize,
    pub row_len: usize,
    row_block: &'a [u8],
    heap: &'a [u8],
}

impl<'a> DatTable<'a> {
    pub fn rows(&self) -> impl Iterator<Item = &'a [u8]> {
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
    // The heap marker is 0xBB x8. Accept a candidate only when the preceding
    // row block splits cleanly into rows of >=8 bytes; otherwise it can be a
    // false positive inside fixed row data.
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

/// Reads a null-terminated UTF-16LE string starting at `offset` in `heap`.
/// Returns `None` for out-of-range offsets, unterminated runs, invalid UTF-16,
/// and empty strings; those cases usually mean the field was not a string.
/// `offset` may be any u64 row value — sentinel fields like
/// `0xFFFF_FFFF_FFFF_FFFF` (a common dat null) must not overflow.
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
        // 0xFFFF_FFFF_FFFF_FFFF row fields are a common dat "null"; they and
        // any other out-of-range offset must resolve to None, not panic
        // (mods.datc64 on a real install crashed here before the fix).
        assert_eq!(utf16le_string_at(&heap, usize::MAX), None);
        assert_eq!(utf16le_string_at(&heap, usize::MAX - 1), None);
        assert_eq!(utf16le_string_at(&heap, heap.len()), None);
        assert_eq!(utf16le_string_at(&heap, heap.len() - 1), None);

        let mut row = Vec::new();
        row.extend_from_slice(&u64::MAX.to_le_bytes());
        assert!(row_strings(&row, &heap).is_empty());
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

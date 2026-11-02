use anyhow::{Result, anyhow, bail};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use rayon::prelude::*;
use std::borrow::Cow;
use std::io::Cursor;
use std::os::raw::{c_int, c_void};

// Provided by vendor/ooz, built in build.rs.
unsafe extern "C" {
    fn Ooz_Decompress(src_buf: *const u8, src_len: u32, dst: *mut u8, dst_size: usize) -> i32;
    fn Ooz_CompressBlock(
        codec_id: c_int,
        src_in: *mut u8,
        dst_in: *mut u8,
        src_size: c_int,
        level: c_int,
        compressopts: *const c_void,
        src_window_base: *mut u8,
        lrm: *mut c_void,
    ) -> c_int;
    fn Ooz_GetCompressedBufferSizeNeeded(size: c_int) -> c_int;
}

const BUNDLE_CHUNK_SIZE: usize = 0x40000;
const BUNDLE_FIXED_HEAD_SIZE_AFTER_PREFIX: usize = 48;
const OODLE_MERMAID_COMPRESSOR: u32 = 9;
const OODLE_COMPRESS_LEVEL: c_int = 1;

// Oodle decoders may write up to 64 bytes past the output end.
pub(super) const OOZ_SAFE_SPACE: usize = 64;
const MAX_BUNDLE_HEAD_SIZE: usize = 16 << 20;
const MAX_RUN_CHUNKS: usize = 16;

struct BundleHead {
    uncompressed_size: usize,
    chunk_unpacked_size: usize,
    chunk_sizes: Vec<usize>,
    data_offset: usize,
}

impl BundleHead {
    fn chunk_dst_size(&self, index: usize) -> usize {
        let start = index * self.chunk_unpacked_size;
        self.chunk_unpacked_size
            .min(self.uncompressed_size.saturating_sub(start))
    }
}

fn parse_bundle_head(src: &[u8]) -> Result<BundleHead> {
    let mut cursor = Cursor::new(src);
    let _total_uncompressed_32 = cursor.read_u32::<LittleEndian>()?;
    let _total_compressed_32 = cursor.read_u32::<LittleEndian>()?;
    let head_size = cursor.read_u32::<LittleEndian>()? as usize;
    let _encoding = cursor.read_u32::<LittleEndian>()?;
    let _unknown = cursor.read_u32::<LittleEndian>()?;
    let uncompressed_size = cursor.read_u64::<LittleEndian>()? as usize;
    let _compressed_size = cursor.read_u64::<LittleEndian>()?;
    let chunk_count = cursor.read_u32::<LittleEndian>()? as usize;
    let chunk_unpacked_size = cursor.read_u32::<LittleEndian>()? as usize;
    for _ in 0..4 {
        let _ = cursor.read_u32::<LittleEndian>()?;
    }
    let mut chunk_sizes = Vec::with_capacity(chunk_count);
    for _ in 0..chunk_count {
        chunk_sizes.push(cursor.read_u32::<LittleEndian>()? as usize);
    }
    let data_offset = 12 + head_size;
    if data_offset < cursor.position() as usize {
        bail!("bundle head_size is smaller than chunk table");
    }
    if chunk_count > 0 && chunk_unpacked_size == 0 {
        bail!("bundle chunk size is zero");
    }
    Ok(BundleHead {
        uncompressed_size,
        chunk_unpacked_size,
        chunk_sizes,
        data_offset,
    })
}

fn decompress_chunk_into(chunk: &[u8], dst: *mut u8, dst_size: usize) -> Result<()> {
    let wrote =
        unsafe { Ooz_Decompress(chunk.as_ptr(), u32::try_from(chunk.len())?, dst, dst_size) };
    // A short write would leave stale bytes in the shared buffer.
    if wrote != i32::try_from(dst_size)? {
        bail!("Oodle decompression wrote {wrote} bytes, expected {dst_size}");
    }
    Ok(())
}

pub fn decompress_bundle(src: &[u8]) -> Result<Vec<u8>> {
    let head = parse_bundle_head(src)?;
    let chunk_count = match head.chunk_unpacked_size {
        0 => 0,
        size => head.uncompressed_size.div_ceil(size),
    };
    if head.chunk_sizes.len() != chunk_count {
        bail!(
            "bundle has {} chunks, expected {chunk_count} for {} bytes",
            head.chunk_sizes.len(),
            head.uncompressed_size
        );
    }
    let mut ranges = Vec::with_capacity(chunk_count);
    let mut offset = head.data_offset;
    for &size in &head.chunk_sizes {
        let end = offset
            .checked_add(size)
            .filter(|&end| end <= src.len())
            .ok_or_else(|| anyhow!("bundle chunk exceeds source length"))?;
        ranges.push(offset..end);
        offset = end;
    }
    let mut out = vec![0u8; head.uncompressed_size];
    if chunk_count == 0 {
        return Ok(out);
    }
    // Chunks are independent. Each decodes into its own scratch buffer, because Oodle may
    // write past the chunk end, then lands in its slot.
    out.par_chunks_mut(head.chunk_unpacked_size)
        .zip(ranges.par_iter())
        .try_for_each_init(
            || vec![0u8; head.chunk_unpacked_size + OOZ_SAFE_SPACE],
            |scratch, (dst, range)| -> Result<()> {
                decompress_chunk_into(&src[range.clone()], scratch.as_mut_ptr(), dst.len())?;
                dst.copy_from_slice(&scratch[..dst.len()]);
                Ok(())
            },
        )?;
    Ok(out)
}

pub fn decompress_bundle_slices(src: &[u8], spans: &[(u32, u32)]) -> Result<Vec<Vec<u8>>> {
    let mut out = vec![Vec::new(); spans.len()];
    visit_bundle_slices(src, spans, |i, bytes| out[i] = bytes)?;
    Ok(out)
}

/// Decodes one chunk at a time and hands each span to `visit` as soon as it is complete,
/// so memory stays near one chunk plus the spans themselves.
pub fn visit_bundle_slices(
    src: &[u8],
    spans: &[(u32, u32)],
    visit: impl FnMut(usize, Vec<u8>),
) -> Result<()> {
    visit_bundle_slices_from(
        src.len(),
        |offset, len| Ok(Cow::Borrowed(&src[offset..offset + len])),
        spans,
        visit,
    )
}

/// Like `visit_bundle_slices`, but reads through `read_at(offset, len)` and fetches only
/// the header and the compressed chunks the spans need. Reads never pass `src_len`, so a
/// corrupt chunk table can't ask for a huge buffer.
pub fn visit_bundle_slices_from<'a>(
    src_len: usize,
    mut read_at: impl FnMut(usize, usize) -> Result<Cow<'a, [u8]>>,
    spans: &[(u32, u32)],
    mut visit: impl FnMut(usize, Vec<u8>),
) -> Result<()> {
    let mut read_at = |offset: usize, len: usize| {
        if offset.checked_add(len).is_none_or(|end| end > src_len) {
            bail!("bundle chunk exceeds source length");
        }
        read_at(offset, len)
    };
    let head_size = u32::from_le_bytes(read_at(8, 4)?.as_ref().try_into()?) as usize;
    if head_size > MAX_BUNDLE_HEAD_SIZE {
        bail!("bundle head_size {head_size} is too large");
    }
    let head = parse_bundle_head(&read_at(0, 12 + head_size)?)?;

    for &(offset, size) in spans {
        if u64::from(offset) + u64::from(size) > head.uncompressed_size as u64 {
            bail!(
                "file span {offset}+{size} exceeds bundle uncompressed size {}",
                head.uncompressed_size
            );
        }
        if size > 0 && head.chunk_unpacked_size == 0 {
            bail!("bundle has data but no chunks");
        }
    }

    let mut chunk_offsets = Vec::with_capacity(head.chunk_sizes.len());
    let mut src_offset = head.data_offset;
    for &size in &head.chunk_sizes {
        chunk_offsets.push(src_offset);
        src_offset = src_offset
            .checked_add(size)
            .filter(|&end| end <= src_len)
            .ok_or_else(|| anyhow!("bundle chunk exceeds source length"))?;
    }

    let mut needed = vec![false; head.chunk_sizes.len()];
    for &(offset, size) in spans {
        if size == 0 {
            continue;
        }
        let first = offset as usize / head.chunk_unpacked_size;
        let last = (offset as usize + size as usize - 1) / head.chunk_unpacked_size;
        if last >= needed.len() {
            bail!("file span {offset}+{size} is past the last bundle chunk");
        }
        needed[first..=last].fill(true);
    }
    // Neighbouring chunks are read together, in pieces small enough to hold per thread.
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut chunk = 0;
    while chunk < needed.len() {
        if needed[chunk] {
            let start = chunk;
            while chunk < needed.len() && needed[chunk] && chunk - start < MAX_RUN_CHUNKS {
                chunk += 1;
            }
            runs.push((start, chunk));
        } else {
            chunk += 1;
        }
    }

    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by_key(|&i| spans[i].0);
    let mut run: Option<(usize, usize, Cow<'a, [u8]>)> = None;
    let mut chunk_buf = Vec::new();
    let mut decoded_chunk = None;
    for i in order {
        let (offset, size) = (spans[i].0 as usize, spans[i].1 as usize);
        if size == 0 {
            visit(i, Vec::new());
            continue;
        }
        let first = offset / head.chunk_unpacked_size;
        let last = (offset + size - 1) / head.chunk_unpacked_size;
        let mut bytes = Vec::with_capacity(size);
        for chunk in first..=last {
            let chunk_start = chunk * head.chunk_unpacked_size;
            let dst_size = head.chunk_dst_size(chunk);
            if decoded_chunk != Some(chunk) {
                if !run
                    .as_ref()
                    .is_some_and(|(start, end, _)| (*start..*end).contains(&chunk))
                {
                    let at = runs.partition_point(|&(_, end)| end <= chunk);
                    let (start, end) = runs[at];
                    let from = chunk_offsets[start];
                    let len = chunk_offsets[end - 1] + head.chunk_sizes[end - 1] - from;
                    run = Some((start, end, read_at(from, len)?));
                }
                let (start, _, data) = run.as_ref().expect("run loaded above");
                let at = chunk_offsets[chunk] - chunk_offsets[*start];
                chunk_buf.resize(dst_size + OOZ_SAFE_SPACE, 0);
                decompress_chunk_into(
                    &data[at..at + head.chunk_sizes[chunk]],
                    chunk_buf.as_mut_ptr(),
                    dst_size,
                )?;
                decoded_chunk = Some(chunk);
            }
            let from = offset.max(chunk_start) - chunk_start;
            let to = (offset + size).min(chunk_start + dst_size) - chunk_start;
            bytes.extend_from_slice(&chunk_buf[from..to]);
        }
        if bytes.len() != size {
            bail!("file span {offset}+{size} decoded to {} bytes", bytes.len());
        }
        visit(i, bytes);
    }
    Ok(())
}

pub fn pack_uncompressed_bundle(data: &[u8]) -> Result<Vec<u8>> {
    crate::timing!("bundle_compress");
    let chunks = data
        .par_chunks(BUNDLE_CHUNK_SIZE)
        .map(compress_chunk)
        .collect::<Result<Vec<_>>>()?;

    let compressed_len: usize = chunks.iter().map(Vec::len).sum();
    let head_size = BUNDLE_FIXED_HEAD_SIZE_AFTER_PREFIX + 4 * chunks.len();
    let total_file_size = 12 + head_size + compressed_len;
    let mut out = Vec::with_capacity(total_file_size);
    out.write_u32::<LittleEndian>(u32::try_from(data.len())?)?;
    out.write_u32::<LittleEndian>(u32::try_from(compressed_len)?)?;
    out.write_u32::<LittleEndian>(u32::try_from(head_size)?)?;
    out.write_u32::<LittleEndian>(OODLE_MERMAID_COMPRESSOR)?;
    out.write_u32::<LittleEndian>(1)?;
    out.write_u64::<LittleEndian>(data.len() as u64)?;
    out.write_u64::<LittleEndian>(compressed_len as u64)?;
    out.write_u32::<LittleEndian>(u32::try_from(chunks.len())?)?;
    out.write_u32::<LittleEndian>(BUNDLE_CHUNK_SIZE as u32)?;
    for _ in 0..4 {
        out.write_u32::<LittleEndian>(0)?;
    }
    for chunk in &chunks {
        out.write_u32::<LittleEndian>(u32::try_from(chunk.len())?)?;
    }
    for chunk in &chunks {
        out.extend_from_slice(chunk);
    }
    Ok(out)
}

fn compress_chunk(chunk: &[u8]) -> Result<Vec<u8>> {
    let capacity = unsafe { Ooz_GetCompressedBufferSizeNeeded(c_int::try_from(chunk.len())?) };
    if capacity <= 0 {
        bail!("Oodle compression reported invalid capacity {capacity}");
    }
    let mut src = chunk.to_vec();
    let mut out = vec![0; usize::try_from(capacity)?];
    let compressed_len = unsafe {
        Ooz_CompressBlock(
            c_int::try_from(OODLE_MERMAID_COMPRESSOR)?,
            src.as_mut_ptr(),
            out.as_mut_ptr(),
            c_int::try_from(src.len())?,
            OODLE_COMPRESS_LEVEL,
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if compressed_len <= 0 {
        bail!("Oodle compression failed with code {compressed_len}");
    }
    out.truncate(usize::try_from(compressed_len)?);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uncompressed_bundle_round_trip() {
        let data = b"hello poe2".repeat(100_000);
        let packed = pack_uncompressed_bundle(&data).unwrap();
        let unpacked = decompress_bundle(&packed).unwrap();
        assert_eq!(unpacked, data);
    }

    fn multi_chunk_data() -> Vec<u8> {
        let len = 2 * BUNDLE_CHUNK_SIZE + 12_345;
        let mut state = 0x2545F491_u32;
        (0..len)
            .map(|_| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                (state >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn partial_decompression_matches_full_decompression() {
        let data = multi_chunk_data();
        let packed = pack_uncompressed_bundle(&data).unwrap();
        let total = u32::try_from(data.len()).unwrap();
        let chunk = u32::try_from(BUNDLE_CHUNK_SIZE).unwrap();
        let spans: [(u32, u32); 7] = [
            (0, 10),
            (chunk + 100, 500), // inside the second chunk
            (chunk - 3, 7),     // crosses the first chunk boundary
            (total - 5, 5),     // tail of the bundle
            (chunk, chunk),     // exactly chunk-aligned
            (0, total),         // the whole bundle
            (1234, 0),          // empty span
        ];
        let slices = decompress_bundle_slices(&packed, &spans).unwrap();
        assert_eq!(slices.len(), spans.len());
        for (&(offset, size), slice) in spans.iter().zip(&slices) {
            let (offset, size) = (offset as usize, size as usize);
            assert_eq!(slice, &data[offset..offset + size], "span {offset}+{size}");
        }
    }

    #[test]
    fn visited_slices_keep_their_input_index() {
        let data = multi_chunk_data();
        let packed = pack_uncompressed_bundle(&data).unwrap();
        let chunk = u32::try_from(BUNDLE_CHUNK_SIZE).unwrap();
        let spans: [(u32, u32); 5] = [
            (2 * chunk + 9, 40),
            (100, 2 * chunk + 50), // crosses two chunk boundaries
            (5, 3),
            (100, 2 * chunk + 50), // duplicate span
            (chunk + 1, 0),
        ];
        let mut seen = vec![None; spans.len()];
        visit_bundle_slices(&packed, &spans, |i, bytes| {
            assert!(seen[i].is_none(), "span {i} visited twice");
            seen[i] = Some(bytes);
        })
        .unwrap();
        for (i, &(offset, size)) in spans.iter().enumerate() {
            let (offset, size) = (offset as usize, size as usize);
            assert_eq!(
                seen[i].as_deref(),
                Some(&data[offset..offset + size]),
                "span {i}"
            );
        }
    }

    #[test]
    fn reader_source_fetches_only_needed_chunks() {
        let data = multi_chunk_data();
        let packed = pack_uncompressed_bundle(&data).unwrap();
        let chunk = BUNDLE_CHUNK_SIZE;
        let span = (2 * chunk as u32 + 7, 100);
        let mut read = 0;
        let mut got = Vec::new();
        visit_bundle_slices_from(
            packed.len(),
            |offset, len| {
                read += len;
                Ok(Cow::Owned(packed[offset..offset + len].to_vec()))
            },
            &[span],
            |_, bytes| got = bytes,
        )
        .unwrap();
        let (offset, size) = (span.0 as usize, span.1 as usize);
        assert_eq!(got, &data[offset..offset + size]);
        let head = parse_bundle_head(&packed).unwrap();
        assert!(
            read < head.data_offset + head.chunk_sizes[2] + 64,
            "read {read} of {}",
            packed.len()
        );
    }

    #[test]
    fn reader_source_rejects_chunks_past_the_source() {
        let data = multi_chunk_data();
        let packed = pack_uncompressed_bundle(&data).unwrap();
        let span = [(0, 10)];
        let read_ok =
            |offset: usize, len: usize| Ok(Cow::Owned(packed[offset..offset + len].to_vec()));

        let truncated = packed.len() - 1;
        assert!(visit_bundle_slices_from(truncated, read_ok, &span, |_, _| ()).is_err());

        let mut oversized = packed.clone();
        let first_chunk_size = 12 + BUNDLE_FIXED_HEAD_SIZE_AFTER_PREFIX;
        oversized[first_chunk_size..first_chunk_size + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let read_oversized =
            |offset: usize, len: usize| Ok(Cow::Owned(oversized[offset..offset + len].to_vec()));
        assert!(
            visit_bundle_slices_from(oversized.len(), read_oversized, &span, |_, _| ()).is_err()
        );
    }

    #[test]
    fn partial_decompression_rejects_out_of_range_span() {
        let data = b"span check".repeat(1_000);
        let packed = pack_uncompressed_bundle(&data).unwrap();
        let total = u32::try_from(data.len()).unwrap();
        assert!(decompress_bundle_slices(&packed, &[(total - 1, 2)]).is_err());
        assert!(decompress_bundle_slices(&packed, &[(0, 1)]).is_ok());
    }

    #[test]
    fn packed_bundle_header_matches_libbundle_layout() {
        let data = b"header check".repeat(100_000);
        let packed = pack_uncompressed_bundle(&data).unwrap();
        let mut cursor = Cursor::new(packed.as_slice());
        let uncompressed_size = cursor.read_u32::<LittleEndian>().unwrap() as usize;
        let compressed_size = cursor.read_u32::<LittleEndian>().unwrap() as usize;
        let head_size = cursor.read_u32::<LittleEndian>().unwrap() as usize;
        let compressor = cursor.read_u32::<LittleEndian>().unwrap();
        let unknown = cursor.read_u32::<LittleEndian>().unwrap();
        let uncompressed_size_long = cursor.read_u64::<LittleEndian>().unwrap() as usize;
        let compressed_size_long = cursor.read_u64::<LittleEndian>().unwrap() as usize;
        let chunk_count = cursor.read_u32::<LittleEndian>().unwrap() as usize;

        assert_eq!(uncompressed_size, data.len());
        assert_eq!(uncompressed_size_long, data.len());
        assert_eq!(compressed_size, compressed_size_long);
        assert_eq!(head_size, 48 + 4 * chunk_count);
        assert_eq!(12 + head_size + compressed_size, packed.len());
        assert_eq!(compressor, OODLE_MERMAID_COMPRESSOR);
        assert_eq!(unknown, 1);
    }
}

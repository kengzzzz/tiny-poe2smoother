use anyhow::{anyhow, bail, Result};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use rayon::prelude::*;
use std::io::Cursor;
use std::os::raw::{c_int, c_void};

// Symbols from vendor/ooz, compiled by build.rs; the compression entry points
// are unmangled wrappers from vendor/ooz/ooz_shim.cpp.
extern "C" {
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

// Oodle decoders may write up to 64 bytes past the destination end; every
// buffer chunks decompress into must carry this slack past the last byte.
const OOZ_SAFE_SPACE: usize = 64;

struct BundleHead {
    uncompressed_size: usize,
    chunk_unpacked_size: usize,
    chunk_sizes: Vec<usize>,
    data_offset: usize,
}

impl BundleHead {
    /// Uncompressed size of chunk `index` (the last chunk is usually short).
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
    let wrote = unsafe {
        Ooz_Decompress(
            chunk.as_ptr(),
            u32::try_from(chunk.len())?,
            dst,
            dst_size,
        )
    };
    // Anything short of the expected size would leave stale bytes in a shared
    // output buffer, so treat it as corruption rather than tolerating it.
    if wrote != i32::try_from(dst_size)? {
        bail!("Oodle decompression wrote {wrote} bytes, expected {dst_size}");
    }
    Ok(())
}

pub fn decompress_bundle(src: &[u8]) -> Result<Vec<u8>> {
    let head = parse_bundle_head(src)?;
    let mut out = vec![0u8; head.uncompressed_size + OOZ_SAFE_SPACE];
    let mut offset = head.data_offset;
    let mut covered = 0usize;
    // Chunks must decompress in order: each may spill up to OOZ_SAFE_SPACE
    // bytes past its end into the next chunk's region, which the next
    // iteration then overwrites. Do not parallelize within this buffer.
    for (i, &size) in head.chunk_sizes.iter().enumerate() {
        if offset + size > src.len() {
            bail!("bundle chunk exceeds source length");
        }
        let dst_size = head.chunk_dst_size(i);
        decompress_chunk_into(&src[offset..offset + size], unsafe {
            out.as_mut_ptr().add(i * head.chunk_unpacked_size)
        }, dst_size)?;
        covered += dst_size;
        offset += size;
    }
    if covered != head.uncompressed_size {
        bail!(
            "bundle decompressed to {} bytes, expected {}",
            covered,
            head.uncompressed_size
        );
    }
    out.truncate(head.uncompressed_size);
    Ok(out)
}

/// Decompress only the chunks of `src` (a compressed bundle) that cover the
/// requested `(offset, size)` spans in uncompressed space, returning the bytes
/// of each span in input order. Peak memory is proportional to the covered
/// chunks rather than the whole bundle.
pub fn decompress_bundle_slices(src: &[u8], spans: &[(u32, u32)]) -> Result<Vec<Vec<u8>>> {
    let head = parse_bundle_head(src)?;

    for &(offset, size) in spans {
        if u64::from(offset) + u64::from(size) > head.uncompressed_size as u64 {
            bail!(
                "file span {offset}+{size} exceeds bundle uncompressed size {}",
                head.uncompressed_size
            );
        }
    }

    // Absolute source offset of each chunk, bounds-checked once.
    let mut chunk_offsets = Vec::with_capacity(head.chunk_sizes.len());
    let mut src_offset = head.data_offset;
    for &size in &head.chunk_sizes {
        if src_offset + size > src.len() {
            bail!("bundle chunk exceeds source length");
        }
        chunk_offsets.push(src_offset);
        src_offset += size;
    }

    let mut needed = vec![false; head.chunk_sizes.len()];
    for &(offset, size) in spans {
        if size == 0 {
            continue;
        }
        let first = offset as usize / head.chunk_unpacked_size;
        let last = (offset as usize + size as usize - 1) / head.chunk_unpacked_size;
        for slot in &mut needed[first..=last] {
            *slot = true;
        }
    }

    // Decompress each maximal run of contiguous needed chunks into one buffer.
    // Chunks within a run must stay sequential (see decompress_bundle).
    let mut runs: Vec<(usize, Vec<u8>)> = Vec::new(); // (first uncompressed byte, bytes)
    let mut chunk = 0;
    while chunk < needed.len() {
        if !needed[chunk] {
            chunk += 1;
            continue;
        }
        let run_start = chunk;
        while chunk < needed.len() && needed[chunk] {
            chunk += 1;
        }
        let run_len: usize = (run_start..chunk).map(|i| head.chunk_dst_size(i)).sum();
        let mut buf = vec![0u8; run_len + OOZ_SAFE_SPACE];
        let mut written = 0usize;
        for i in run_start..chunk {
            let dst_size = head.chunk_dst_size(i);
            let chunk_src = &src[chunk_offsets[i]..chunk_offsets[i] + head.chunk_sizes[i]];
            decompress_chunk_into(chunk_src, unsafe { buf.as_mut_ptr().add(written) }, dst_size)?;
            written += dst_size;
        }
        buf.truncate(run_len);
        runs.push((run_start * head.chunk_unpacked_size, buf));
    }

    spans
        .iter()
        .map(|&(offset, size)| {
            if size == 0 {
                return Ok(Vec::new());
            }
            let (offset, size) = (offset as usize, size as usize);
            // A span's chunks are contiguous, so it lies within a single run.
            let (run_start, buf) = runs
                .iter()
                .find(|(start, buf)| offset >= *start && offset + size <= start + buf.len())
                .ok_or_else(|| anyhow!("file span {offset}+{size} missing from decompressed runs"))?;
            Ok(buf[offset - run_start..offset - run_start + size].to_vec())
        })
        .collect()
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

    /// Varied, poorly-compressible bytes spanning a bit over two chunks.
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
            (chunk + 100, 500),      // inside the second chunk
            (chunk - 3, 7),          // crosses the first chunk boundary
            (total - 5, 5),          // tail of the bundle
            (chunk, chunk),          // exactly chunk-aligned
            (0, total),              // the whole bundle
            (1234, 0),               // empty span
        ];
        let slices = decompress_bundle_slices(&packed, &spans).unwrap();
        assert_eq!(slices.len(), spans.len());
        for (&(offset, size), slice) in spans.iter().zip(&slices) {
            let (offset, size) = (offset as usize, size as usize);
            assert_eq!(slice, &data[offset..offset + size], "span {offset}+{size}");
        }
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

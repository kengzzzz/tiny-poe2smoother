#[derive(Debug, Clone, Copy)]
pub(super) enum HashMode {
    Murmur64A,
    Fnv1A,
}

pub fn filepath_hash(path: &str) -> u64 {
    fnv1a_bundle_hash(path)
}

pub(super) fn fnv1a_bundle_hash(path: &str) -> u64 {
    hash_fnv1a(format!("{}++", path.trim_end_matches('/').to_ascii_lowercase()).as_bytes())
}

pub(super) fn hash_path(hash_mode: HashMode, path: &str) -> u64 {
    match hash_mode {
        HashMode::Murmur64A => {
            murmur_hash64a(path.trim_end_matches('/').to_ascii_lowercase().as_bytes())
        }
        HashMode::Fnv1A => fnv1a_bundle_hash(path),
    }
}

pub(super) fn hash_fnv1a(data: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in data {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn murmur_hash64a(data: &[u8]) -> u64 {
    if data.is_empty() {
        return 0xF42A94E69CFF42FE;
    }
    const M: u64 = 0xC6A4A7935BD1E995;
    const R: u32 = 47;
    let mut hash = 0x1337B33Fu64 ^ ((data.len() as u64).wrapping_mul(M));
    let mut chunks = data.chunks_exact(8);
    for chunk in &mut chunks {
        let mut k = u64::from_le_bytes(chunk.try_into().expect("chunk size"));
        k = k.wrapping_mul(M);
        k ^= k >> R;
        k = k.wrapping_mul(M);
        hash ^= k;
        hash = hash.wrapping_mul(M);
    }
    let rem = chunks.remainder();
    if !rem.is_empty() {
        let mut tail = 0u64;
        for (i, byte) in rem.iter().enumerate() {
            tail |= u64::from(*byte) << (i * 8);
        }
        hash ^= tail;
        hash = hash.wrapping_mul(M);
    }
    hash ^= hash >> R;
    hash = hash.wrapping_mul(M);
    hash ^ (hash >> R)
}

pub(super) fn murmur2_32(data: &[u8]) -> u32 {
    let m = 0x5bd1_e995u32;
    let r = 24u32;
    let len = data.len() as u32;
    let mut h = len;
    let chunks = data.chunks_exact(4);
    let rem = chunks.remainder();

    for chunk in chunks {
        let mut k = u32::from_le_bytes(chunk.try_into().expect("chunk size"));
        k = k.wrapping_mul(m);
        k ^= k >> r;
        k = k.wrapping_mul(m);
        h = h.wrapping_mul(m);
        h ^= k;
    }

    match rem.len() {
        3 => {
            h ^= u32::from(rem[2]) << 16;
            h ^= u32::from(rem[1]) << 8;
            h ^= u32::from(rem[0]);
            h = h.wrapping_mul(m);
        }
        2 => {
            h ^= u32::from(rem[1]) << 8;
            h ^= u32::from(rem[0]);
            h = h.wrapping_mul(m);
        }
        1 => {
            h ^= u32::from(rem[0]);
            h = h.wrapping_mul(m);
        }
        _ => {}
    }

    h ^= h >> 13;
    h = h.wrapping_mul(m);
    h ^ (h >> 15)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filepath_hash_is_case_insensitive() {
        assert_eq!(
            filepath_hash("Metadata/Foo.ot"),
            filepath_hash("metadata/foo.ot")
        );
    }
}

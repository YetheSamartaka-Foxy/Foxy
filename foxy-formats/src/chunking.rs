//! Content-defined part boundaries for files no container format describes.
//!
//! Fixed-size parts stop matching after the first inserted or removed byte,
//! because every later boundary moves. A boundary chosen from the bytes around
//! it moves with the content instead, so a rebuilt game executable or data
//! archive keeps most of its parts and a client only downloads the ones that
//! changed. This is a gear-hash chunker with normalized chunking (FastCDC).
//!
//! The table, masks, and sizes are part of what a published manifest means:
//! changing any of them moves every boundary, and the next publish re-sends
//! whole files to every client.

pub const CONTENT_CHUNK_MIN_SIZE: u64 = 256 * 1024;
pub const CONTENT_CHUNK_AVG_SIZE: u64 = 1024 * 1024;
pub const CONTENT_CHUNK_MAX_SIZE: u64 = 4 * 1024 * 1024;

/// The gear hash forgets a byte after 64 shifts, so bytes earlier than this in
/// a part cannot influence a boundary and are skipped without hashing.
const HASH_SKIP_LEN: u64 = CONTENT_CHUNK_MIN_SIZE - 64;
const MASK_BEFORE_AVG: u64 = top_bits(22);
const MASK_AFTER_AVG: u64 = top_bits(18);

const fn top_bits(bits: u32) -> u64 {
    !0u64 << (64 - bits)
}

const GEAR: [u64; 256] = {
    let mut table = [0u64; 256];
    let mut state: u64 = 0x666F_7879_6364_6331;
    let mut index = 0;
    while index < table.len() {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        table[index] = z ^ (z >> 31);
        index += 1;
    }
    table
};

/// Streaming boundary finder. Feed it a file's bytes in order, in slices of
/// any size; the boundaries depend only on the bytes, never on the slicing.
#[derive(Clone, Debug, Default)]
pub struct ContentChunker {
    hash: u64,
    len: u64,
}

impl ContentChunker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Scan the next bytes of the file. Returns the length of the prefix of
    /// `data` that completes the current part, after which the chunker starts
    /// a new part; `None` when all of `data` belongs to the current part.
    pub fn find_boundary(&mut self, data: &[u8]) -> Option<usize> {
        let mut index = 0;
        while index < data.len() {
            if self.len < HASH_SKIP_LEN {
                let skip = ((HASH_SKIP_LEN - self.len) as usize).min(data.len() - index);
                self.len += skip as u64;
                index += skip;
                continue;
            }
            self.hash = (self.hash << 1).wrapping_add(GEAR[data[index] as usize]);
            self.len += 1;
            index += 1;
            if self.len < CONTENT_CHUNK_MIN_SIZE {
                continue;
            }
            let mask = if self.len < CONTENT_CHUNK_AVG_SIZE {
                MASK_BEFORE_AVG
            } else {
                MASK_AFTER_AVG
            };
            if self.hash & mask == 0 || self.len >= CONTENT_CHUNK_MAX_SIZE {
                self.hash = 0;
                self.len = 0;
                return Some(index);
            }
        }
        None
    }

    /// Bytes scanned since the last boundary: the length of the final part
    /// once the whole file has been fed.
    pub fn pending_len(&self) -> u64 {
        self.len
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::hash::{DefaultHasher, Hash, Hasher};

    fn pseudo_random_bytes(len: usize, seed: u64) -> Vec<u8> {
        let mut state = seed;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 24) as u8
            })
            .collect()
    }

    fn chunk_lengths(data: &[u8], slice_len: usize) -> Vec<usize> {
        let mut chunker = ContentChunker::new();
        let mut lengths = Vec::new();
        let mut current = 0;
        for slice in data.chunks(slice_len) {
            let mut rest = slice;
            while let Some(cut) = chunker.find_boundary(rest) {
                lengths.push(current + cut);
                current = 0;
                rest = &rest[cut..];
            }
            current += rest.len();
        }
        assert_eq!(current as u64, chunker.pending_len());
        if current > 0 {
            lengths.push(current);
        }
        lengths
    }

    fn chunk_digests(data: &[u8]) -> Vec<u64> {
        let mut start = 0;
        chunk_lengths(data, 1 << 20)
            .into_iter()
            .map(|len| {
                let mut hasher = DefaultHasher::new();
                data[start..start + len].hash(&mut hasher);
                start += len;
                hasher.finish()
            })
            .collect()
    }

    #[test]
    fn parts_cover_the_file_within_the_size_limits() {
        let data = pseudo_random_bytes(12 << 20, 7);
        let lengths = chunk_lengths(&data, 1 << 20);
        assert_eq!(lengths.iter().sum::<usize>(), data.len());
        assert!(lengths.len() > 3);
        let (last, full) = lengths.split_last().unwrap();
        assert!(*last as u64 <= CONTENT_CHUNK_MAX_SIZE);
        for len in full {
            let len = *len as u64;
            assert!((CONTENT_CHUNK_MIN_SIZE..=CONTENT_CHUNK_MAX_SIZE).contains(&len));
        }
    }

    #[test]
    fn boundaries_do_not_depend_on_how_the_bytes_are_fed() {
        let data = pseudo_random_bytes(6 << 20, 11);
        let whole = chunk_lengths(&data, data.len());
        assert_eq!(chunk_lengths(&data, 4096), whole);
        assert_eq!(chunk_lengths(&data, 777_777), whole);
    }

    #[test]
    fn constant_data_is_cut_at_the_maximum_size() {
        let data = vec![0u8; 9 << 20];
        let lengths = chunk_lengths(&data, 1 << 20);
        assert_eq!(lengths[0] as u64, CONTENT_CHUNK_MAX_SIZE);
        assert_eq!(lengths[1] as u64, CONTENT_CHUNK_MAX_SIZE);
        assert_eq!(lengths[2], data.len() - 2 * CONTENT_CHUNK_MAX_SIZE as usize);
    }

    #[test]
    fn an_insertion_only_changes_the_parts_around_it() {
        let original = pseudo_random_bytes(16 << 20, 3);
        let mut edited = original[..(5 << 20)].to_vec();
        edited.extend_from_slice(&pseudo_random_bytes(1234, 99));
        edited.extend_from_slice(&original[(5 << 20)..]);

        let before = chunk_digests(&original);
        let after = chunk_digests(&edited);
        let kept: HashSet<_> = before.iter().collect();
        let reused = after.iter().filter(|digest| kept.contains(digest)).count();
        assert!(
            reused + 2 >= after.len(),
            "reused {reused} of {} parts",
            after.len()
        );
    }
}

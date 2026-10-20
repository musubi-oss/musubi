//! Content-defined chunking for build artifacts (FastCDC-style gear hashing with normalized
//! cut points). Both the server (`PUT` of a raw artifact) and uploaders (`musubi build push`,
//! launchers fetching only the chunks they lack) split bytes with the same parameters, so a
//! chunk list computed anywhere names the same stored blobs: a one-line change in a 4 GiB build
//! moves one or two chunks, everything else is reused by hash.

use serde::{Deserialize, Serialize};
use sha2::Digest;

/// Cut-point parameters. Boundaries are chosen by the rolling gear hash so inserted or removed
/// bytes only disturb the chunks around them; `min`/`max` bound the chunk size, `avg` sets the
/// expected size (power of two).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkParams {
    pub min: usize,
    pub avg: usize,
    pub max: usize,
}

impl Default for ChunkParams {
    fn default() -> Self {
        Self {
            min: 256 << 10,
            avg: 1 << 20,
            max: 4 << 20,
        }
    }
}

impl ChunkParams {
    pub fn validate(&self) -> Result<(), String> {
        if self.min < 64 || self.min > self.avg || self.avg > self.max {
            return Err("chunk sizes must satisfy 64 ≤ min ≤ avg ≤ max".into());
        }
        if !self.avg.is_power_of_two() {
            return Err("avg chunk size must be a power of two".into());
        }
        if self.max > 64 << 20 {
            return Err("max chunk size is capped at 64 MiB".into());
        }
        Ok(())
    }

    fn masks(&self) -> (u64, u64) {
        let bits = self.avg.trailing_zeros();
        // Normalized chunking (FastCDC): a stricter mask before `avg`, a looser one after, which
        // pulls the size distribution towards `avg` and away from `min`/`max` clamping.
        let small = mask_bits(bits.saturating_add(1).min(63));
        let large = mask_bits(bits.saturating_sub(1).max(1));
        (small, large)
    }
}

fn mask_bits(bits: u32) -> u64 {
    // Spread the bits across the word so the boundary test looks at the whole hash, not the low
    // byte only (gear hashes shift left, so low bits reflect only the newest bytes).
    let mut m = 0u64;
    let mut set = 0;
    let mut i = 0u32;
    while set < bits && i < 64 {
        if i % 2 == 1 {
            m |= 1 << (63 - i);
            set += 1;
        }
        i += 1;
    }
    m
}

static GEAR: std::sync::OnceLock<[u64; 256]> = std::sync::OnceLock::new();

/// Deterministic gear table (splitmix64 from a fixed seed) so every build of the binary chunks
/// identically — the table is part of the on-the-wire format.
fn gear() -> &'static [u64; 256] {
    GEAR.get_or_init(|| {
        let mut t = [0u64; 256];
        let mut s: u64 = 0x6d75_7375_6269_2d63; // "musubi-c"
        for v in t.iter_mut() {
            s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = s;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            *v = z ^ (z >> 31);
        }
        t
    })
}

/// Length of the first chunk of `data` (all of it when no cut point qualifies before the end).
pub fn cut(data: &[u8], p: &ChunkParams) -> usize {
    let n = data.len();
    if n <= p.min {
        return n;
    }
    let end = n.min(p.max);
    let (small, large) = p.masks();
    let g = gear();
    let mut h: u64 = 0;
    let mid = p.avg.min(end);
    let mut i = p.min;
    while i < mid {
        h = (h << 1).wrapping_add(g[data[i] as usize]);
        if h & small == 0 {
            return i + 1;
        }
        i += 1;
    }
    while i < end {
        h = (h << 1).wrapping_add(g[data[i] as usize]);
        if h & large == 0 {
            return i + 1;
        }
        i += 1;
    }
    end
}

/// One chunk of an artifact: where it sits in the file and the sha256 of its bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunk {
    pub offset: u64,
    pub size: u32,
    pub hash: String,
}

/// Split `data` fully; chunks are contiguous and cover the whole input.
pub fn split(data: &[u8], p: &ChunkParams) -> Vec<Chunk> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off < data.len() {
        let len = cut(&data[off..], p);
        out.push(Chunk {
            offset: off as u64,
            size: len as u32,
            hash: hex::encode(sha2::Sha256::digest(&data[off..off + len])),
        });
        off += len;
    }
    out
}

/// Chunk list plus whole-file digest: what an uploader sends before any bytes and what a
/// launcher fetches to learn which chunks it still lacks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub params: ChunkParams,
    pub size: u64,
    pub sha256: String,
    pub chunks: Vec<Chunk>,
}

impl Manifest {
    pub fn of(data: &[u8], p: &ChunkParams) -> Self {
        Self {
            params: *p,
            size: data.len() as u64,
            sha256: hex::encode(sha2::Sha256::digest(data)),
            chunks: split(data, p),
        }
    }

    /// Structural checks on a client-supplied manifest: sizes add up, offsets are contiguous,
    /// hashes are lowercase sha256 hex, chunk sizes respect the parameters.
    pub fn validate(&self) -> Result<(), String> {
        self.params.validate()?;
        if self.chunks.is_empty() || self.size == 0 {
            return Err("manifest has no chunks".into());
        }
        if self.sha256.len() != 64 || !is_hex(&self.sha256) {
            return Err("sha256 must be 64 lowercase hex chars".into());
        }
        let mut off = 0u64;
        for (i, c) in self.chunks.iter().enumerate() {
            if c.offset != off {
                return Err(format!("chunk {i}: offset {} ≠ expected {off}", c.offset));
            }
            if c.size == 0 || c.size as usize > self.params.max {
                return Err(format!("chunk {i}: size {} outside params", c.size));
            }
            if c.hash.len() != 64 || !is_hex(&c.hash) {
                return Err(format!("chunk {i}: hash must be 64 lowercase hex chars"));
            }
            off += c.size as u64;
        }
        if off != self.size {
            return Err(format!(
                "chunks sum to {off} bytes, manifest says {}",
                self.size
            ));
        }
        Ok(())
    }

    /// Chunks of `self` whose hash is not in `have` — what a holder of `have` must transfer.
    pub fn missing<'a>(&'a self, have: &std::collections::HashSet<&str>) -> Vec<&'a Chunk> {
        let mut seen = std::collections::HashSet::new();
        self.chunks
            .iter()
            .filter(|c| !have.contains(c.hash.as_str()) && seen.insert(c.hash.as_str()))
            .collect()
    }

    /// Reuse statistics against an older manifest (the previous version of the same build):
    /// how many chunks/bytes are shared and how many must be transferred.
    pub fn delta_from(&self, old: &Manifest) -> Delta {
        let old: std::collections::HashSet<&str> =
            old.chunks.iter().map(|c| c.hash.as_str()).collect();
        let mut d = Delta {
            chunks: self.chunks.len() as u64,
            bytes: self.size,
            ..Default::default()
        };
        let mut seen = std::collections::HashSet::new();
        for c in &self.chunks {
            if old.contains(c.hash.as_str()) {
                d.reused_chunks += 1;
                d.reused_bytes += c.size as u64;
            } else {
                if seen.insert(c.hash.as_str()) {
                    d.new_unique_bytes += c.size as u64;
                }
                d.new_chunks += 1;
                d.new_bytes += c.size as u64;
            }
        }
        d.new_pct = if self.size == 0 {
            0.0
        } else {
            (d.new_bytes as f64 * 100.0 / self.size as f64 * 10.0).round() / 10.0
        };
        d
    }
}

/// What a new version costs on top of an old one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Delta {
    pub chunks: u64,
    pub bytes: u64,
    pub reused_chunks: u64,
    pub reused_bytes: u64,
    pub new_chunks: u64,
    pub new_bytes: u64,
    /// Bytes an uploader actually transfers (new chunks, deduplicated within the file).
    pub new_unique_bytes: u64,
    /// Share of the file that changed, in percent (one decimal).
    pub new_pct: f64,
}

fn is_hex(s: &str) -> bool {
    s.bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(seed: u64, n: usize) -> Vec<u8> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn chunking_is_deterministic_bounded_and_local() {
        let p = ChunkParams {
            min: 2 << 10,
            avg: 8 << 10,
            max: 32 << 10,
        };
        p.validate().unwrap();
        let data = noise(7, 1 << 20);
        let a = split(&data, &p);
        let b = split(&data, &p);
        assert_eq!(a, b);
        assert!(a.len() > 60 && a.len() < 300, "{} chunks", a.len());
        let mut off = 0u64;
        for c in &a {
            assert_eq!(c.offset, off);
            assert!(c.size as usize <= p.max);
            off += c.size as u64;
        }
        assert_eq!(off, data.len() as u64);
        // all but the last chunk respect min
        assert!(a[..a.len() - 1].iter().all(|c| c.size as usize >= p.min));
        let avg = data.len() / a.len();
        assert!(avg > 4 << 10 && avg < 16 << 10, "avg chunk {avg}");

        // edit 100 bytes in the middle: almost everything is reused
        let mut edited = data.clone();
        edited[600_000..600_100].copy_from_slice(&noise(99, 100));
        let m0 = Manifest::of(&data, &p);
        let m1 = Manifest::of(&edited, &p);
        let d = m1.delta_from(&m0);
        assert!(d.new_chunks <= 3, "{d:?}");
        assert!(d.reused_chunks >= a.len() as u64 - 3, "{d:?}");
        assert!(d.new_pct < 5.0, "{d:?}");
        assert_eq!(d.reused_bytes + d.new_bytes, edited.len() as u64);

        // insert bytes: chunks after the insert shift but keep their hashes
        let mut inserted = data.clone();
        inserted.splice(300_000..300_000, noise(5, 777));
        let m2 = Manifest::of(&inserted, &p);
        let d = m2.delta_from(&m0);
        assert!(d.new_chunks <= 3, "{d:?}");

        // unrelated data: nothing reused
        let m3 = Manifest::of(&noise(1234, 1 << 20), &p);
        let d = m3.delta_from(&m0);
        assert_eq!(d.reused_chunks, 0);
        assert_eq!(d.new_pct, 100.0);

        // missing() dedups repeated chunks and honours what the holder has
        let have: std::collections::HashSet<&str> =
            m0.chunks.iter().map(|c| c.hash.as_str()).collect();
        let miss = m1.missing(&have);
        assert_eq!(miss.len() as u64, m1.delta_from(&m0).new_chunks);
        let twice = [data[..10_000].to_vec(), data[..10_000].to_vec()].concat();
        let m4 = Manifest::of(
            &twice,
            &ChunkParams {
                min: 1 << 10,
                avg: 4 << 10,
                max: 8 << 10,
            },
        );
        let miss = m4.missing(&Default::default());
        assert!(
            miss.len() < m4.chunks.len(),
            "{} vs {}",
            miss.len(),
            m4.chunks.len()
        );
    }

    #[test]
    fn small_inputs_and_manifest_validation() {
        let p = ChunkParams::default();
        assert!(split(b"", &p).is_empty());
        let one = split(b"tiny build", &p);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].size, 10);
        let m = Manifest::of(b"tiny build", &p);
        m.validate().unwrap();
        assert_eq!(m.chunks[0].hash, m.sha256);

        let mut bad = m.clone();
        bad.size += 1;
        assert!(bad.validate().unwrap_err().contains("sum"));
        let mut bad = m.clone();
        bad.chunks[0].hash = "XYZ".into();
        assert!(bad.validate().is_err());
        let mut bad = m.clone();
        bad.chunks.push(Chunk {
            offset: 5,
            size: 1,
            hash: m.sha256.clone(),
        });
        assert!(bad.validate().unwrap_err().contains("offset"));
        let mut bad = m.clone();
        bad.chunks.clear();
        assert!(bad.validate().is_err());
        assert!(ChunkParams {
            min: 10,
            avg: 8,
            max: 100
        }
        .validate()
        .is_err());
        assert!(ChunkParams {
            min: 64,
            avg: 100,
            max: 1000
        }
        .validate()
        .is_err());
        assert!(ChunkParams {
            min: 64,
            avg: 128,
            max: 128 << 20
        }
        .validate()
        .is_err());
        assert_eq!(mask_bits(3).count_ones(), 3);
    }
}

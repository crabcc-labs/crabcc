//! FSST string codec — gated by the `compress` feature.
//!
//! [Fast Static Symbol Table](https://www.vldb.org/pvldb/vol13/p2649-boncz.pdf)
//! gives us per-row decompressible string compression with 2–4× ratios on
//! repetitive corpora (function signatures, file paths) at decompression
//! speeds well past 1 GB/s. We use it to shrink the `symbols.signature`
//! column without losing the SQLite single-column read path.
//!
//! ## API surface
//!
//! [`Codec`] is the only thing this module exports. Build one by `train`-ing
//! on a representative sample, persist it with [`Codec::save`], reload at
//! `Store::open` time with [`Codec::load`]. Each row is independently
//! compressible/decompressible — there is no streaming state.
//!
//! ## Persistence format (v1)
//!
//! ```text
//! +--------+----+--------+--------+----------+----------+
//! | "CCFS" | 01 | _pad   | count  | symbols  | lengths  |
//! | 4 B    | 1B | 3 B    | u32 LE | n×u64 LE | n×u8     |
//! +--------+----+--------+--------+----------+----------+
//! ```
//!
//! `fsst-rs` itself does not ship a serializer (see upstream issue tracker)
//! so we roll our own. The format is intentionally trivial — there's no
//! length-prefix on individual entries because all symbols are 8 bytes wide
//! at the wire level (`Symbol` is `pub struct Symbol(u64)`); the per-symbol
//! `length` field is stored separately and tells the decoder how many of
//! those 8 bytes are meaningful.

use anyhow::{anyhow, Context, Result};
use fsst::{Compressor, Symbol};
use std::fs;
use std::io::{Read, Write};
use std::path::Path;

const MAGIC: &[u8; 4] = b"CCFS";
const VERSION: u8 = 0x01;
const HEADER_LEN: usize = 4 /* magic */ + 1 /* version */ + 3 /* pad */ + 4 /* count */;

/// Trained FSST symbol table + the matching compressor / decompressor.
///
/// `Codec` is `Send + Sync`; clone with `Compressor::rebuild_from` if you
/// need an owned copy on another thread (clones are cheap — 256 symbols max).
pub struct Codec {
    inner: Compressor,
}

impl Codec {
    /// Train a fresh codec on a representative `samples` slice. Pass the
    /// signatures (or whatever column) you intend to encode in production —
    /// FSST's quality is dominated by training-set fidelity. 10–50k rows is
    /// usually plenty; the upstream paper finds diminishing returns past ~50k.
    ///
    /// `samples` is borrowed; we collect into the `&Vec<&[u8]>` that the
    /// fsst-rs 0.5 API expects internally and drop the temporary Vec on return.
    pub fn train(samples: &[&[u8]]) -> Result<Self> {
        if samples.is_empty() {
            return Err(anyhow!("Codec::train: empty samples"));
        }
        // fsst-rs requires `&Vec<&[u8]>` (not `&[&[u8]]`).
        let inner = Compressor::train(&samples.to_vec());
        Ok(Self { inner })
    }

    /// Read a codec previously written by [`Codec::save`]. Validates magic,
    /// version, and that the byte-count for each section matches `count`.
    pub fn load(path: &Path) -> Result<Self> {
        let mut f = fs::File::open(path)
            .with_context(|| format!("Codec::load: open {}", path.display()))?;
        let mut header = [0u8; HEADER_LEN];
        f.read_exact(&mut header).context("Codec::load: header")?;
        if &header[0..4] != MAGIC {
            return Err(anyhow!(
                "Codec::load: bad magic, got {:?} want {:?}",
                &header[0..4],
                MAGIC
            ));
        }
        if header[4] != VERSION {
            return Err(anyhow!(
                "Codec::load: unsupported version {} (expected {})",
                header[4],
                VERSION
            ));
        }
        // header[5..8] is reserved padding; ignore.
        let count = u32::from_le_bytes([header[8], header[9], header[10], header[11]]) as usize;

        let mut sym_buf = vec![0u8; count * 8];
        f.read_exact(&mut sym_buf).context("Codec::load: symbols")?;
        let symbols: Vec<Symbol> = sym_buf
            .chunks_exact(8)
            .map(|chunk| Symbol::from_slice(chunk.try_into().expect("chunks_exact(8)")))
            .collect();

        let mut lengths = vec![0u8; count];
        f.read_exact(&mut lengths).context("Codec::load: lengths")?;

        let inner = Compressor::rebuild_from(symbols, lengths);
        Ok(Self { inner })
    }

    /// Atomically write the codec to `path`. The format is described at the
    /// top of this module. Writes via a sibling `*.tmp` file then renames so
    /// concurrent readers never observe a half-written symbol table.
    pub fn save(&self, path: &Path) -> Result<()> {
        let symbols: &[Symbol] = self.inner.symbol_table();
        let lengths: &[u8] = self.inner.symbol_lengths();
        // Sanity: fsst-rs guarantees these match, but check anyway — a
        // mismatch here would corrupt every subsequent decompress.
        if symbols.len() != lengths.len() {
            return Err(anyhow!(
                "Codec::save: symbol/length mismatch ({} vs {})",
                symbols.len(),
                lengths.len()
            ));
        }
        let count = symbols.len();
        let count_u32 = u32::try_from(count)
            .map_err(|_| anyhow!("Codec::save: symbol table > u32::MAX entries"))?;

        let tmp = path.with_extension("tmp");
        {
            let mut f = fs::File::create(&tmp)
                .with_context(|| format!("Codec::save: create {}", tmp.display()))?;
            f.write_all(MAGIC)?;
            f.write_all(&[VERSION])?;
            f.write_all(&[0u8; 3])?; // pad
            f.write_all(&count_u32.to_le_bytes())?;
            for sym in symbols {
                f.write_all(&sym.to_u64().to_le_bytes())?;
            }
            f.write_all(lengths)?;
            f.sync_all().ok();
        }
        fs::rename(&tmp, path)
            .with_context(|| format!("Codec::save: rename to {}", path.display()))?;
        Ok(())
    }

    /// Encode a single plaintext slice. Empty input returns empty output —
    /// callers can safely skip the `signature_enc` flag flip for empty rows.
    pub fn compress(&self, plain: &[u8]) -> Vec<u8> {
        if plain.is_empty() {
            return Vec::new();
        }
        self.inner.compress(plain)
    }

    /// Decode a single FSST-encoded slice. Empty input returns empty output.
    ///
    /// Corrupt or foreign streams degrade to empty rather than risking memory
    /// unsafety. fsst-rs decodes each code with `symbols.get_unchecked(code)`,
    /// so a code referencing a symbol this codec lacks — DB corruption, a
    /// codec-generation mismatch, or a non-FSST blob mis-flagged as encoded —
    /// is an out-of-bounds read (UB in release, where the `get_unchecked`
    /// precondition check is compiled out). `codes_in_range` rejects such
    /// streams first; it is O(bytes) and negligible beside the decode.
    ///
    /// `#[inline]` — called once per decoded signature on every read
    /// path. Crossing the crate boundary without inlining sacrifices
    /// 5-15% on hot lookup loops once LTO sees the call site.
    #[inline]
    pub fn decompress(&self, encoded: &[u8]) -> Vec<u8> {
        if encoded.is_empty() {
            return Vec::new();
        }
        if !codes_in_range(encoded, self.inner.symbol_table().len()) {
            return Vec::new();
        }
        self.inner.decompressor().decompress(encoded)
    }
}

/// True iff every symbol code in `encoded` indexes within an `n_symbols`-entry
/// table — the precondition fsst-rs's unchecked decode loop assumes but never
/// verifies. The escape code (`fsst::ESCAPE_CODE`, 255) consumes the following
/// byte as a raw literal, so that byte is skipped rather than range-checked; a
/// trailing escape is a truncated stream and is rejected.
fn codes_in_range(encoded: &[u8], n_symbols: usize) -> bool {
    let mut i = 0;
    while i < encoded.len() {
        if encoded[i] == fsst::ESCAPE_CODE {
            if i + 1 >= encoded.len() {
                return false;
            }
            i += 2;
        } else {
            if encoded[i] as usize >= n_symbols {
                return false;
            }
            i += 1;
        }
    }
    true
}

#[cfg(all(test, feature = "compress"))]
mod tests {
    use super::*;

    /// Tiny xorshift64 RNG — avoids pulling in `rand` for one test. Seed
    /// from wall-clock so the seed varies per run; if a future failure needs
    /// repro, capture the seed printed on assertion failure.
    struct XorShift64(u64);
    impl XorShift64 {
        fn new(seed: u64) -> Self {
            Self(if seed == 0 {
                0xdead_beef_cafe_babe
            } else {
                seed
            })
        }
        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
        fn next_byte(&mut self) -> u8 {
            (self.next_u64() & 0xFF) as u8
        }
        fn next_len(&mut self, max: usize) -> usize {
            (self.next_u64() as usize) % (max + 1)
        }
    }

    fn seed_from_clock() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0xfeed_face)
    }

    fn small_corpus() -> Vec<Vec<u8>> {
        // Repeats are essential — FSST learns nothing from random bytes.
        let phrases = [
            b"fn foo(x: u32) -> u32".as_slice(),
            b"fn bar(y: &str) -> String",
            b"fn baz(z: bool) -> bool",
            b"impl Display for Foo",
            b"impl Debug for Foo",
            b"impl Clone for Foo",
            b"pub struct Bar { name: String }",
            b"pub enum Kind { A, B, C }",
            b"async fn handler(req: Request) -> Response",
            b"fn main() -> anyhow::Result<()>",
        ];
        phrases.iter().map(|p| p.to_vec()).collect()
    }

    #[test]
    fn roundtrip_empty() {
        let corpus = small_corpus();
        let refs: Vec<&[u8]> = corpus.iter().map(|v| v.as_slice()).collect();
        let codec = Codec::train(&refs).unwrap();
        // Empty in / empty out, both directions.
        assert_eq!(codec.compress(b""), b"");
        assert_eq!(codec.decompress(b""), b"");
    }

    #[test]
    fn decompress_rejects_out_of_range_code_without_oob() {
        // Regression (fuzz `fsst_decompress_arbitrary`): decoding bytes that
        // reference a symbol code beyond this codec's table hits fsst-rs's
        // `symbols.get_unchecked(code)` — an OOB read (UB in release; caught as
        // a precondition panic under the debug-assertions fuzz build). The
        // decode must instead degrade to empty.
        let corpus = small_corpus();
        let refs: Vec<&[u8]> = corpus.iter().map(|v| v.as_slice()).collect();
        let codec = Codec::train(&refs).unwrap();
        let n = codec.inner.symbol_table().len();
        assert!(n < 255, "tiny corpus should train < 255 symbols (got {n})");
        // Code == n is one past the table; pre-fix this OOB'd the symbol slice.
        assert!(
            codec.decompress(&[n as u8]).is_empty(),
            "out-of-range code must degrade to empty, not OOB"
        );
    }

    #[test]
    fn roundtrip_random_1000() {
        let seed = seed_from_clock();
        let mut rng = XorShift64::new(seed);

        // Generate 1000 random byte strings of length 0..=512.
        let mut all: Vec<Vec<u8>> = Vec::with_capacity(1000);
        for _ in 0..1000 {
            let n = rng.next_len(512);
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(rng.next_byte());
            }
            all.push(v);
        }
        // Train on the first 100; FSST is robust enough to encode the rest.
        let train_refs: Vec<&[u8]> = all.iter().take(100).map(|v| v.as_slice()).collect();
        let codec = Codec::train(&train_refs).unwrap();

        for (i, plain) in all.iter().enumerate() {
            let enc = codec.compress(plain);
            let back = codec.decompress(&enc);
            assert_eq!(
                back,
                *plain,
                "mismatch at row {i} (seed=0x{seed:016x}): plain.len={} enc.len={}",
                plain.len(),
                enc.len()
            );
        }
    }

    #[test]
    fn save_load_roundtrip() {
        let corpus = small_corpus();
        let refs: Vec<&[u8]> = corpus.iter().map(|v| v.as_slice()).collect();
        let original = Codec::train(&refs).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fsst.symbols");
        original.save(&path).unwrap();

        let loaded = Codec::load(&path).unwrap();

        // Cross-check: encode with original, decode with loaded — and vice
        // versa. Both directions must yield byte-identical output for every
        // sample, otherwise the symbol table didn't survive serialization.
        for plain in &corpus {
            let enc_orig = original.compress(plain);
            assert_eq!(loaded.decompress(&enc_orig), *plain);

            let enc_loaded = loaded.compress(plain);
            assert_eq!(original.decompress(&enc_loaded), *plain);
        }
    }

    #[test]
    fn save_load_bad_magic_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("garbage.symbols");
        // 32 bytes of obvious non-CCFS junk.
        fs::write(&path, b"NOT_A_VALID_FSST_TABLE_FILE_____").unwrap();
        let err = match Codec::load(&path) {
            Ok(_) => panic!("garbage file must not load"),
            Err(e) => e,
        };
        let msg = format!("{err:#}");
        assert!(
            msg.contains("bad magic"),
            "expected magic-mismatch error, got: {msg}"
        );
    }
}

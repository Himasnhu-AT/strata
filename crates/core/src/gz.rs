//! `.gz` files: decompressed by a job into a temp file, which is then opened like any log. Owned
//! by the `index` stream.
//!
//! # Why a temp file, not an index into the stream
//!
//! Deflate has no random access: reading line 40 M of a compressed stream means decompressing
//! from the start, or from a checkpoint that stores the 32 KB window the decoder had there (the
//! `zran` approach). Checkpoints every few MB would make random access possible, but every scroll,
//! search and parse would go through a decoder (around 300–500 MB/s, far below the 1 GB/s index
//! and the zero-copy search over a map), and every reader would need a second code path. A
//! decompressed copy costs disk space, which is bounded (`max_bytes`, chosen by the caller from
//! free space) and temporary, and buys the same map, index, search and follow code as a plain log.
//! Concatenated members (`cat a.gz b.gz`, what `logrotate` and `gzip -c >>` produce) are read as
//! one stream.

use std::fs::File;
use std::io::{BufReader, BufWriter, ErrorKind, Read, Write};
use std::path::Path;

use flate2::read::MultiGzDecoder;

use crate::{Error, Result};

/// Decompressed bytes between two `progress` calls.
const REPORT_EVERY: u64 = 4 << 20;

/// Whether `head` starts with the gzip magic bytes.
pub fn is_gzip(head: &[u8]) -> bool {
    head.starts_with(&[0x1f, 0x8b])
}

/// Counts the compressed bytes read, for progress.
struct Counting<R> {
    inner: R,
    read: u64,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read = self.read.saturating_add(n as u64);
        Ok(n)
    }
}

/// Decompress `src` into `dst` (streamed), writing at most `max_bytes` (free disk space minus a
/// margin) and failing with [`Error::TooLarge`] beyond. `progress(read, total)` is called now and
/// then with compressed bytes read; returning `false` cancels ([`Error::Cancelled`]). Returns the
/// decompressed size.
///
/// On any error (including a cancel) the partial `dst` is removed.
pub fn decompress(src: &Path, dst: &Path, max_bytes: u64, progress: &mut dyn FnMut(u64, u64) -> bool) -> Result<u64> {
    let r = decompress_inner(src, dst, max_bytes, progress);
    if r.is_err() {
        let _ = std::fs::remove_file(dst);
    }
    r
}

fn decompress_inner(src: &Path, dst: &Path, max_bytes: u64, progress: &mut dyn FnMut(u64, u64) -> bool) -> Result<u64> {
    let input = File::open(src).map_err(|e| Error::io(src, &e))?;
    let total = input.metadata().map_err(|e| Error::io(src, &e))?.len();
    let mut dec = MultiGzDecoder::new(BufReader::with_capacity(256 * 1024, Counting { inner: input, read: 0 }));
    let output = File::create(dst).map_err(|e| Error::io(dst, &e))?;
    let mut out = BufWriter::with_capacity(1 << 20, output);
    let mut buf = vec![0u8; 256 * 1024];
    let mut written = 0u64;
    let mut next_report = 0u64;
    loop {
        let n = match dec.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => {
                return Err(match e.kind() {
                    ErrorKind::InvalidData | ErrorKind::InvalidInput | ErrorKind::UnexpectedEof => {
                        Error::Corrupt { path: src.to_path_buf(), message: e.to_string() }
                    }
                    _ => Error::io(src, &e),
                });
            }
        };
        written = written.saturating_add(n as u64);
        if written > max_bytes {
            return Err(Error::TooLarge { path: src.to_path_buf(), limit: max_bytes });
        }
        out.write_all(buf.get(..n).unwrap_or_default()).map_err(|e| Error::io(dst, &e))?;
        if written >= next_report {
            next_report = written.saturating_add(REPORT_EVERY);
            if !progress(dec.get_ref().get_ref().read, total) {
                return Err(Error::Cancelled);
            }
        }
    }
    out.flush().map_err(|e| Error::io(dst, &e))?;
    let _ = progress(total, total);
    Ok(written)
}

#[cfg(test)]
mod tests {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use proptest::prelude::*;

    use super::*;

    fn gz(data: &[u8]) -> Vec<u8> {
        let mut e = GzEncoder::new(Vec::new(), Compression::fast());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    fn run(src_bytes: &[u8], max: u64) -> (Result<u64>, tempfile::TempDir) {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("in.log.gz");
        std::fs::write(&src, src_bytes).unwrap();
        let r = decompress(&src, &d.path().join("out.log"), max, &mut |_, _| true);
        (r, d)
    }

    #[test]
    fn round_trip_and_members() {
        let mut both = gz(b"first member\n");
        both.extend(gz(b"second member\n"));
        assert!(is_gzip(&both));
        let (r, d) = run(&both, 1 << 20);
        assert_eq!(r.unwrap(), 27);
        assert_eq!(std::fs::read(d.path().join("out.log")).unwrap(), b"first member\nsecond member\n");
    }

    #[test]
    fn limits_and_corruption() {
        let data = gz(&vec![b'a'; 100_000]);
        let (r, d) = run(&data, 1000);
        assert!(matches!(r, Err(Error::TooLarge { limit: 1000, .. })));
        assert!(!d.path().join("out.log").exists());

        let (r, _d) = run(data.get(..data.len() / 2).unwrap(), u64::MAX);
        assert!(matches!(r, Err(Error::Corrupt { .. })), "{r:?}");
        let (r, _d) = run(b"not gzip at all", u64::MAX);
        assert!(matches!(r, Err(Error::Corrupt { .. })), "{r:?}");
        let (r, _d) = run(b"", u64::MAX);
        assert!(matches!(r, Ok(0) | Err(Error::Corrupt { .. })), "{r:?}");
        let d = tempfile::tempdir().unwrap();
        let r = decompress(&d.path().join("missing.gz"), &d.path().join("o"), 10, &mut |_, _| true);
        assert!(matches!(r, Err(Error::NotFound { .. })));
    }

    #[test]
    fn cancel_and_progress() {
        let data = gz(&vec![b'x'; 20 << 20]);
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("in.gz");
        std::fs::write(&src, &data).unwrap();
        let mut calls = 0;
        let r = decompress(&src, &d.path().join("out"), u64::MAX, &mut |_, _| {
            calls += 1;
            calls < 2
        });
        assert_eq!(r, Err(Error::Cancelled));
        assert!(!d.path().join("out").exists());
        let mut last = (0, 0);
        let n = decompress(&src, &d.path().join("out"), u64::MAX, &mut |a, b| {
            last = (a, b);
            true
        })
        .unwrap();
        assert_eq!(n, 20 << 20);
        assert_eq!(last, (data.len() as u64, data.len() as u64));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn random_input_never_crashes(mut bytes in proptest::collection::vec(any::<u8>(), 0..4096), magic in any::<bool>()) {
            if magic && bytes.len() >= 3 {
                bytes[0] = 0x1f;
                bytes[1] = 0x8b;
                bytes[2] = 8;
            }
            let (_r, _d) = run(&bytes, 1 << 20);
        }
    }
}

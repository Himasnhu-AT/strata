//! `.gz` files: decompressed by a job into a temp file, which is then opened like any log. Owned
//! by the `index` stream.

use std::path::Path;

use crate::{Error, Result};

/// Whether `head` starts with the gzip magic bytes.
pub fn is_gzip(head: &[u8]) -> bool {
    head.starts_with(&[0x1f, 0x8b])
}

/// Decompress `src` into `dst` (streamed), writing at most `max_bytes` (free disk space minus a
/// margin) and failing with [`Error::TooLarge`] beyond. `progress(read, total)` is called now and
/// then with compressed bytes read; returning `false` cancels ([`Error::Cancelled`]). Returns the
/// decompressed size.
pub fn decompress(src: &Path, dst: &Path, max_bytes: u64, progress: &mut dyn FnMut(u64, u64) -> bool) -> Result<u64> {
    let _ = (src, dst, max_bytes, progress);
    Err(Error::NotYet { what: "gz" })
}

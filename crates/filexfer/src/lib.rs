//! Streaming file transfer over the KVM link.
//!
//! A batch of one or more files is offered on the same capture transition as
//! clipboard text. Each file is read in `FILE_CHUNK_BYTES` slices and framed as
//! `FileBatchStart` → (`FileHeader` → `FileChunk`* → `FileEnd`)* → `FileBatchEnd`.
//! The receiver reassembles each file into the system temp dir, verifies its
//! CRC-32, and then points its local clipboard at every file that arrived
//! intact.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use yzendris_protocol::{Event, FILE_CHUNK_BYTES};

/// Received files older than this are pruned when a new batch starts, so the
/// temp dir doesn't grow without bound across sessions.
const PRUNE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// Identity of a clipboard file set: (path, size, mtime) per file. Used to avoid
/// re-sending the same selection on every capture transition, and to break the
/// receive→resend ping-pong once received files land on the local clipboard.
pub type FileSig = Vec<(PathBuf, u64, SystemTime)>;

/// Directory where received files are written: `<system temp>/yzendris`.
/// On Windows that's `%TEMP%\yzendris`, on Linux `/tmp/yzendris` (or `$TMPDIR`).
pub fn incoming_dir() -> PathBuf {
    std::env::temp_dir().join("yzendris")
}

/// Signature of the files at `paths` (skipping any that can't be stat'd).
pub fn signature_of(paths: &[PathBuf]) -> FileSig {
    paths
        .iter()
        .filter_map(|p| {
            let md = std::fs::metadata(p).ok()?;
            Some((p.clone(), md.len(), md.modified().ok()?))
        })
        .collect()
}

/// Decide whether to offer `paths` for transfer. Returns an opened batch, or
/// `None` when the selection is unchanged since `last_sig`, holds no regular
/// file, or every file is over `max_bytes`. `last_sig` is updated so an
/// unchanged/oversized selection isn't re-evaluated on every crossing.
pub async fn prepare_offer(
    paths: &[PathBuf],
    last_sig: &mut Option<FileSig>,
    max_bytes: u64,
) -> Option<OutgoingBatch> {
    if paths.is_empty() {
        return None;
    }
    let sig = signature_of(paths);
    if last_sig.as_ref() == Some(&sig) {
        return None; // already handled this exact selection
    }
    *last_sig = Some(sig);

    // Keep only regular files within the size limit; warn about the rest.
    let mut keep = Vec::new();
    for p in paths {
        match std::fs::metadata(p) {
            Ok(md) if md.is_file() && md.len() <= max_bytes => keep.push(p.clone()),
            Ok(md) if md.is_file() => tracing::warn!(
                "file '{}' is {} bytes (over the {} byte limit) — skipped",
                p.display(),
                md.len(),
                max_bytes
            ),
            Ok(_) => tracing::warn!("'{}' is not a regular file — skipped", p.display()),
            Err(e) => tracing::warn!("can't stat '{}': {e} — skipped", p.display()),
        }
    }
    if keep.is_empty() {
        return None;
    }
    Some(OutgoingBatch::new(keep))
}

// ─── Sending ───────────────────────────────────────────────────────────────

/// One file currently being streamed out.
struct OutgoingCurrent {
    file: File,
    hasher: crc32fast::Hasher,
    sent: u64,
    size: u64,
    name: String,
    last_pct: u8,
}

/// A batch of local files streamed out one after another. Drives the event
/// sequence via [`OutgoingBatch::next_event`].
pub struct OutgoingBatch {
    paths: Vec<PathBuf>,
    idx: usize,
    current: Option<OutgoingCurrent>,
}

impl OutgoingBatch {
    fn new(paths: Vec<PathBuf>) -> Self {
        Self { paths, idx: 0, current: None }
    }

    /// Number of files in this batch (for the opening `FileBatchStart`).
    pub fn count(&self) -> u32 {
        self.paths.len() as u32
    }

    /// The next protocol event, or `None` when every file has been sent (the
    /// caller then sends `FileBatchEnd`). Emits `FileHeader` when opening a
    /// file, `FileChunk` for each slice, and `FileEnd` (with CRC-32) at EOF.
    pub async fn next_event(&mut self) -> Result<Option<Event>> {
        match self.current.as_mut() {
            None => {
                if self.idx >= self.paths.len() {
                    return Ok(None);
                }
                let path = self.paths[self.idx].clone();
                let md = std::fs::metadata(&path)
                    .with_context(|| format!("stat {}", path.display()))?;
                let size = md.len();
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "yzendris-file".to_owned());
                let file = File::open(&path)
                    .await
                    .with_context(|| format!("open {}", path.display()))?;
                tracing::info!("sending file '{name}' ({size} bytes)");
                self.current = Some(OutgoingCurrent {
                    file,
                    hasher: crc32fast::Hasher::new(),
                    sent: 0,
                    size,
                    name: name.clone(),
                    last_pct: 0,
                });
                Ok(Some(Event::FileHeader { name, size }))
            }
            Some(cur) => {
                let mut buf = vec![0u8; FILE_CHUNK_BYTES];
                let n = cur.file.read(&mut buf).await.context("read file chunk")?;
                if n == 0 {
                    let crc = cur.hasher.clone().finalize();
                    self.current = None;
                    self.idx += 1;
                    return Ok(Some(Event::FileEnd { crc32: crc }));
                }
                buf.truncate(n);
                cur.hasher.update(&buf);
                cur.sent += n as u64;
                log_progress("sending", &cur.name, cur.sent, cur.size, &mut cur.last_pct);
                Ok(Some(Event::FileChunk { data: buf }))
            }
        }
    }
}

// ─── Receiving ───────────────────────────────────────────────────────────────

/// One file currently being reassembled.
struct IncomingCurrent {
    file: File,
    path: PathBuf,
    name: String,
    written: u64,
    size: u64,
    hasher: crc32fast::Hasher,
    last_pct: u8,
}

/// A batch of incoming files. Files that pass their CRC-32 check are collected
/// in `completed`; the caller reads them back with [`IncomingBatch::finish`].
pub struct IncomingBatch {
    dir: PathBuf,
    completed: Vec<PathBuf>,
    current: Option<IncomingCurrent>,
}

impl IncomingBatch {
    /// Start a batch: ensure the temp dir exists and prune stale files.
    pub async fn start() -> Result<Self> {
        let dir = incoming_dir();
        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("create dir {}", dir.display()))?;
        prune_old(&dir);
        Ok(Self { dir, completed: Vec::new(), current: None })
    }

    /// Begin receiving one file. `name` is sanitized to a bare file name and
    /// disambiguated so it never escapes the temp dir or clobbers an existing
    /// file.
    pub async fn begin_file(&mut self, name: &str, size: u64) -> Result<()> {
        // Abandon any half-written previous file first (shouldn't happen on a
        // well-formed stream, but never leak a dangling handle).
        self.discard_current();

        let path = unique_path(&self.dir, &sanitize_name(name));
        let file = File::create(&path)
            .await
            .with_context(|| format!("create {}", path.display()))?;
        tracing::info!("receiving file '{}' ({} bytes)", name, size);
        self.current = Some(IncomingCurrent {
            file,
            path,
            name: name.to_owned(),
            written: 0,
            size,
            hasher: crc32fast::Hasher::new(),
            last_pct: 0,
        });
        Ok(())
    }

    /// Append a chunk to the current file, never writing past its declared size.
    pub async fn write_chunk(&mut self, data: &[u8]) -> Result<()> {
        if let Some(cur) = self.current.as_mut() {
            let take = (data.len() as u64).min(cur.size - cur.written) as usize;
            if take > 0 {
                cur.file.write_all(&data[..take]).await.context("write chunk")?;
                cur.hasher.update(&data[..take]);
                cur.written += take as u64;
                log_progress("receiving", &cur.name, cur.written, cur.size, &mut cur.last_pct);
            }
        }
        Ok(())
    }

    /// Finish the current file: keep it only if the full size arrived and the
    /// CRC-32 matches; otherwise delete the partial/corrupt file.
    pub async fn end_file(&mut self, expected_crc: u32) -> Result<()> {
        let Some(mut cur) = self.current.take() else {
            return Ok(());
        };
        cur.file.flush().await.context("flush file")?;
        let got_crc = cur.hasher.finalize();

        if cur.written != cur.size {
            tracing::error!(
                "file '{}' truncated ({} of {} bytes) — discarding",
                cur.name, cur.written, cur.size
            );
            let _ = tokio::fs::remove_file(&cur.path).await;
        } else if got_crc != expected_crc {
            tracing::error!(
                "file '{}' failed checksum (got {:08x}, expected {:08x}) — discarding",
                cur.name, got_crc, expected_crc
            );
            let _ = tokio::fs::remove_file(&cur.path).await;
        } else {
            tracing::info!("file saved to {}", cur.path.display());
            self.completed.push(cur.path);
        }
        Ok(())
    }

    /// Consume the batch, returning the intact files (for the clipboard).
    pub fn finish(mut self) -> Vec<PathBuf> {
        self.discard_current();
        self.completed
    }

    /// Delete an in-progress partial file (on abort / connection loss).
    pub fn discard_current(&mut self) {
        if let Some(cur) = self.current.take() {
            let _ = std::fs::remove_file(&cur.path);
        }
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Log at each 25% step so a large transfer shows progress without spamming.
fn log_progress(verb: &str, name: &str, done: u64, total: u64, last_pct: &mut u8) {
    if total == 0 {
        return;
    }
    let pct = ((done.min(total) * 100) / total) as u8;
    if pct >= *last_pct + 25 && pct < 100 {
        *last_pct = pct - (pct % 25);
        tracing::info!("{verb} '{name}': {}%", *last_pct);
    }
}

/// Delete files in `dir` last modified more than [`PRUNE_AFTER`] ago.
fn prune_old(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let Ok(md) = entry.metadata() else { continue };
        if !md.is_file() {
            continue;
        }
        if let Ok(modified) = md.modified() {
            if now.duration_since(modified).map(|d| d > PRUNE_AFTER).unwrap_or(false) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// A path under `dir` for `name` that doesn't already exist, appending
/// ` (1)`, ` (2)`, … before the extension on collision.
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_owned(), format!(".{e}")),
        _ => (name.to_owned(), String::new()),
    };
    for i in 1..u32::MAX {
        let candidate = dir.join(format!("{stem} ({i}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    first // unreachable in practice
}

/// Reduce a peer-supplied name to a safe bare file name.
fn sanitize_name(name: &str) -> String {
    let base = name
        .rsplit(|c| c == '/' || c == '\\')
        .next()
        .unwrap_or(name)
        .trim();
    if base.is_empty() || base == "." || base == ".." {
        "yzendris-file".to_owned()
    } else {
        base.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive an `OutgoingBatch` to completion, feeding every event into a fresh
    /// `IncomingBatch`. If `corrupt_chunk` is set, flip a byte in that chunk to
    /// simulate wire corruption. Returns the files that survived reassembly.
    async fn roundtrip(mut batch: OutgoingBatch, corrupt_chunk: Option<usize>) -> Vec<PathBuf> {
        let mut inc = IncomingBatch::start().await.unwrap();
        let mut chunk_no = 0usize;
        while let Some(ev) = batch.next_event().await.unwrap() {
            match ev {
                Event::FileHeader { name, size } => inc.begin_file(&name, size).await.unwrap(),
                Event::FileChunk { mut data } => {
                    if Some(chunk_no) == corrupt_chunk && !data.is_empty() {
                        data[0] ^= 0xFF;
                    }
                    chunk_no += 1;
                    inc.write_chunk(&data).await.unwrap();
                }
                Event::FileEnd { crc32 } => inc.end_file(crc32).await.unwrap(),
                other => panic!("unexpected event {other:?}"),
            }
        }
        inc.finish()
    }

    async fn make_source(name: &str, bytes: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join("yzendris-test-src");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join(name);
        tokio::fs::write(&path, bytes).await.unwrap();
        path
    }

    async fn read(path: &Path) -> Vec<u8> {
        tokio::fs::read(path).await.unwrap()
    }

    /// Multi-file batch, including a file larger than one chunk, round-trips
    /// intact.
    #[tokio::test]
    async fn multi_file_roundtrip_is_byte_exact() {
        let small = b"hola cachyos".to_vec();
        let big: Vec<u8> = (0..FILE_CHUNK_BYTES * 2 + 123).map(|i| (i % 251) as u8).collect();
        let p1 = make_source("small.txt", &small).await;
        let p2 = make_source("big.bin", &big).await;

        let batch = OutgoingBatch::new(vec![p1, p2]);
        assert_eq!(batch.count(), 2);
        let got = roundtrip(batch, None).await;

        assert_eq!(got.len(), 2, "both files should arrive");
        let contents: Vec<Vec<u8>> = {
            let mut v = Vec::new();
            for p in &got {
                v.push(read(p).await);
            }
            v
        };
        assert!(contents.contains(&small));
        assert!(contents.contains(&big));
    }

    /// A corrupted chunk fails the CRC-32 check and the file is discarded.
    #[tokio::test]
    async fn corrupt_chunk_is_rejected() {
        let data = vec![7u8; FILE_CHUNK_BYTES + 50];
        let p = make_source("corrupt-me.bin", &data).await;

        let batch = OutgoingBatch::new(vec![p]);
        let got = roundtrip(batch, Some(0)).await; // flip a byte in the first chunk

        assert!(got.is_empty(), "corrupt file must not survive");
    }

    /// Two files with the same name don't clobber each other on the receiver.
    #[tokio::test]
    async fn same_name_files_are_disambiguated() {
        let a = b"first".to_vec();
        let b = b"second, different".to_vec();
        // Distinct source dirs so both can share the base name "dup.txt".
        let da = std::env::temp_dir().join("yzendris-test-a");
        let db = std::env::temp_dir().join("yzendris-test-b");
        tokio::fs::create_dir_all(&da).await.unwrap();
        tokio::fs::create_dir_all(&db).await.unwrap();
        let pa = da.join("dup.txt");
        let pb = db.join("dup.txt");
        tokio::fs::write(&pa, &a).await.unwrap();
        tokio::fs::write(&pb, &b).await.unwrap();

        let batch = OutgoingBatch::new(vec![pa, pb]);
        let got = roundtrip(batch, None).await;

        assert_eq!(got.len(), 2, "both same-named files should be kept");
        assert_ne!(got[0], got[1], "receiver paths must differ");
        let c0 = read(&got[0]).await;
        let c1 = read(&got[1]).await;
        assert!((c0 == a && c1 == b) || (c0 == b && c1 == a));
    }
}

//! Carrying copied files across the link.
//!
//! The sender streams each file as `FileBegin → FileData* → FileEnd`; the
//! receiver rebuilds them in a private, per-transfer directory and only then
//! points its clipboard at them. Nothing received is ever opened or executed.
//!
//! Everything about an incoming file is treated as hostile input: its name is
//! reduced to a safe bare file name, its size is checked against the user's
//! limit before a byte is written, and the stream may not exceed what it
//! declared.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{bail, Context, Result};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use yzendris_core::config::create_private_dir;
use yzendris_core::proto::{Msg, FILE_CHUNK_BYTES, MAX_FILES_PER_BATCH};

/// Received transfers older than this are deleted.
const KEEP_FOR: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_NAME_BYTES: usize = 180;

/// Identity of a clipboard file selection, to avoid resending it.
pub type FileSig = Vec<(PathBuf, u64, SystemTime)>;

pub fn signature(paths: &[PathBuf]) -> FileSig {
    paths
        .iter()
        .filter_map(|p| {
            let md = std::fs::metadata(p).ok()?;
            Some((p.clone(), md.len(), md.modified().ok()?))
        })
        .collect()
}

// ── Sending ─────────────────────────────────────────────────────────────────

struct Reading {
    file: File,
    crc: crc32fast::Hasher,
    left: u64,
}

/// A batch of local files being streamed out.
pub struct Outgoing {
    files: Vec<(PathBuf, u64)>,
    index: usize,
    reading: Option<Reading>,
}

impl Outgoing {
    /// Select the regular files in `paths` that fit under `max_bytes` each.
    /// Returns `None` when nothing qualifies.
    pub fn prepare(paths: &[PathBuf], max_bytes: u64) -> Option<Self> {
        let mut files = Vec::new();
        for path in paths.iter().take(MAX_FILES_PER_BATCH as usize) {
            match std::fs::metadata(path) {
                Ok(md) if md.is_file() && md.len() <= max_bytes => {
                    files.push((path.clone(), md.len()))
                }
                Ok(md) if md.is_file() => {
                    tracing::warn!("a copied file is over the size limit and was skipped")
                }
                Ok(_) => tracing::info!("folders are not carried across; skipped one"),
                Err(e) => tracing::debug!("skipping unreadable clipboard entry: {e}"),
            }
        }
        (!files.is_empty()).then_some(Self {
            files,
            index: 0,
            reading: None,
        })
    }

    pub fn begin(&self) -> Msg {
        Msg::FilesBegin {
            count: self.files.len() as u32,
            total: self.files.iter().map(|f| f.1).sum(),
        }
    }

    /// The next message of the stream, or `None` once every file is sent.
    pub async fn next(&mut self) -> Result<Option<Msg>> {
        let Some(reading) = self.reading.as_mut() else {
            let Some((path, size)) = self.files.get(self.index).cloned() else {
                return Ok(None);
            };
            let file = File::open(&path).await.context("opening a copied file")?;
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "file".into());
            self.reading = Some(Reading {
                file,
                crc: crc32fast::Hasher::new(),
                left: size,
            });
            return Ok(Some(Msg::FileBegin { name, size }));
        };

        // Send exactly the size announced, even if the file grew meanwhile.
        let want = reading.left.min(FILE_CHUNK_BYTES as u64) as usize;
        let mut chunk = vec![0u8; want];
        let got = if want == 0 {
            0
        } else {
            reading.file.read(&mut chunk).await?
        };
        if got == 0 {
            let crc32 = reading.crc.clone().finalize();
            self.reading = None;
            self.index += 1;
            return Ok(Some(Msg::FileEnd { crc32 }));
        }
        chunk.truncate(got);
        reading.crc.update(&chunk);
        reading.left -= got as u64;
        Ok(Some(Msg::FileData(chunk)))
    }
}

// ── Receiving ───────────────────────────────────────────────────────────────

struct Writing {
    file: File,
    path: PathBuf,
    crc: crc32fast::Hasher,
    size: u64,
    written: u64,
}

/// A batch of files being received.
pub struct Incoming {
    dir: PathBuf,
    max_bytes: u64,
    files_left: u32,
    writing: Option<Writing>,
    done: Vec<PathBuf>,
}

impl Incoming {
    /// Start receiving `count` files under `base`, each at most `max_bytes`.
    pub async fn start(base: &Path, count: u32, max_bytes: u64) -> Result<Self> {
        if count == 0 || count > MAX_FILES_PER_BATCH {
            bail!("refusing a transfer of {count} files");
        }
        create_private_dir(base)?;
        prune(base);
        let stamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default();
        let mut salt = [0u8; 4];
        getrandom::fill(&mut salt).map_err(|e| anyhow::anyhow!("no randomness: {e}"))?;
        let dir = base.join(format!("{stamp}-{:08x}", u32::from_le_bytes(salt)));
        create_private_dir(&dir)?;
        Ok(Self {
            dir,
            max_bytes,
            files_left: count,
            writing: None,
            done: Vec::new(),
        })
    }

    pub async fn begin_file(&mut self, name: &str, size: u64) -> Result<()> {
        self.discard_partial().await;
        if self.files_left == 0 {
            bail!("peer sent more files than it announced");
        }
        self.files_left -= 1;
        if size > self.max_bytes {
            tracing::warn!("an incoming file is over the size limit and was skipped");
            return Ok(());
        }
        let path = free_path(&self.dir, &safe_name(name));
        let mut opts = tokio::fs::OpenOptions::new();
        // `create_new` never follows a symlink or replaces an existing file.
        opts.write(true).create_new(true);
        #[cfg(unix)]
        opts.mode(0o600);
        let file = opts.open(&path).await.context("creating incoming file")?;
        self.writing = Some(Writing {
            file,
            path,
            crc: crc32fast::Hasher::new(),
            size,
            written: 0,
        });
        Ok(())
    }

    pub async fn data(&mut self, chunk: &[u8]) -> Result<()> {
        let Some(w) = self.writing.as_mut() else {
            return Ok(()); // part of a skipped file
        };
        if w.written + chunk.len() as u64 > w.size {
            self.discard_partial().await;
            bail!("peer sent more data than the file size it announced");
        }
        w.file
            .write_all(chunk)
            .await
            .context("writing incoming file")?;
        w.crc.update(chunk);
        w.written += chunk.len() as u64;
        Ok(())
    }

    pub async fn end_file(&mut self, crc32: u32) -> Result<()> {
        let Some(mut w) = self.writing.take() else {
            return Ok(());
        };
        w.file.flush().await?;
        drop(w.file);
        if w.written == w.size && w.crc.finalize() == crc32 {
            self.done.push(w.path);
        } else {
            tracing::warn!("an incoming file arrived incomplete or corrupt and was discarded");
            let _ = tokio::fs::remove_file(&w.path).await;
        }
        Ok(())
    }

    async fn discard_partial(&mut self) {
        if let Some(w) = self.writing.take() {
            drop(w.file);
            let _ = tokio::fs::remove_file(&w.path).await;
        }
    }

    /// The files that arrived intact.
    pub async fn finish(mut self) -> Vec<PathBuf> {
        self.discard_partial().await;
        if self.done.is_empty() {
            let _ = tokio::fs::remove_dir(&self.dir).await;
        }
        std::mem::take(&mut self.done)
    }
}

impl Drop for Incoming {
    fn drop(&mut self) {
        // Connection lost mid-file: do not leave a truncated file behind.
        if let Some(w) = self.writing.take() {
            let _ = std::fs::remove_file(&w.path);
        }
    }
}

/// Delete transfers older than [`KEEP_FOR`].
pub fn prune(base: &Path) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let Ok(md) = entry.metadata() else { continue };
        let old = md
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age > KEEP_FOR);
        if old && md.is_dir() {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

fn free_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
        _ => (name, String::new()),
    };
    (2u32..)
        .map(|i| dir.join(format!("{stem} ({i}){ext}")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

/// Is `name` one Windows would open as a device rather than a file?
///
/// Windows decides this from the part before the *first* dot, ignoring
/// trailing spaces, so `NUL`, `nul.txt`, `NUL .txt` and `COM1.tar.gz` are all
/// devices. `COM`/`LPT` also accept the superscript digits ¹ ² ³.
fn is_windows_device(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    let mut chars = stem.chars();
    let prefix: String = chars.by_ref().take(3).collect();
    matches!(prefix.as_str(), "COM" | "LPT")
        && matches!((chars.next(), chars.next()), (Some(d), None) if d.is_ascii_digit() || "¹²³".contains(d))
}

/// Strip what Windows silently drops from the end of a name.
fn trim_tail(name: &mut String) {
    name.truncate(name.trim_end_matches(['.', ' ']).len());
}

/// Reduce a peer-supplied name to a bare file name that is safe on every
/// supported file system. The result is checked the way the *strictest*
/// system interprets it, so a name that is harmless where it was sent from
/// cannot turn into something else where it lands.
pub fn safe_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let mut name: String = base
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    trim_tail(&mut name);
    let mut name = name.trim_start().to_owned();
    if name.len() > MAX_NAME_BYTES {
        let ext = name
            .rfind('.')
            .map(|i| name[i..].to_owned())
            .filter(|e| e.len() <= 16)
            .unwrap_or_default();
        let mut cut = MAX_NAME_BYTES - ext.len();
        while !name.is_char_boundary(cut) {
            cut -= 1;
        }
        name.truncate(cut);
        name.push_str(&ext);
        // Cutting can expose a new trailing dot or space.
        trim_tail(&mut name);
    }
    if name.is_empty() {
        return "file".into();
    }
    // Checked last, on the final name, so no earlier step can re-create one.
    if is_windows_device(&name) {
        name.insert(0, '_');
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn transfer(src: &[PathBuf], base: &Path, limit: u64, corrupt: bool) -> Vec<PathBuf> {
        let mut out = Outgoing::prepare(src, limit).expect("something to send");
        let Msg::FilesBegin { count, .. } = out.begin() else {
            unreachable!()
        };
        let mut inc = Incoming::start(base, count, limit).await.unwrap();
        let mut first_chunk = true;
        while let Some(msg) = out.next().await.unwrap() {
            match msg {
                Msg::FileBegin { name, size } => inc.begin_file(&name, size).await.unwrap(),
                Msg::FileData(mut d) => {
                    if corrupt && std::mem::take(&mut first_chunk) {
                        d[0] ^= 0xff;
                    }
                    inc.data(&d).await.unwrap();
                }
                Msg::FileEnd { crc32 } => inc.end_file(crc32).await.unwrap(),
                other => panic!("unexpected {other:?}"),
            }
        }
        inc.finish().await
    }

    #[tokio::test]
    async fn files_round_trip_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let big: Vec<u8> = (0..FILE_CHUNK_BYTES * 2 + 123)
            .map(|i| (i % 251) as u8)
            .collect();
        let (a, b, empty) = (
            dir.path().join("a.txt"),
            dir.path().join("b.bin"),
            dir.path().join("e"),
        );
        std::fs::write(&a, b"hola").unwrap();
        std::fs::write(&b, &big).unwrap();
        std::fs::write(&empty, b"").unwrap();

        let got = transfer(&[a, b, empty], &dir.path().join("in"), u64::MAX, false).await;
        assert_eq!(got.len(), 3);
        assert_eq!(std::fs::read(&got[0]).unwrap(), b"hola");
        assert_eq!(std::fs::read(&got[1]).unwrap(), big);
        assert_eq!(got[1].file_name().unwrap(), "b.bin");
        assert_eq!(std::fs::read(&got[2]).unwrap(), b"");
    }

    #[tokio::test]
    async fn corrupt_file_is_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("x.bin");
        std::fs::write(&src, vec![7u8; 5000]).unwrap();
        let base = dir.path().join("in");
        assert!(transfer(&[src], &base, u64::MAX, true).await.is_empty());
        // Nothing — not even the batch directory — is left behind.
        assert_eq!(std::fs::read_dir(&base).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn oversized_and_non_files_are_not_offered() {
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("big");
        std::fs::write(&big, vec![0u8; 2048]).unwrap();
        assert!(Outgoing::prepare(&[big, dir.path().to_path_buf()], 1024).is_none());
    }

    #[tokio::test]
    async fn receiver_enforces_announced_sizes_and_counts() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Incoming::start(dir.path(), 0, 10).await.is_err());
        assert!(Incoming::start(dir.path(), MAX_FILES_PER_BATCH + 1, 10)
            .await
            .is_err());

        let mut inc = Incoming::start(dir.path(), 1, 100).await.unwrap();
        inc.begin_file("a", 4).await.unwrap();
        assert!(
            inc.data(b"12345").await.is_err(),
            "more data than announced"
        );
        assert!(
            inc.begin_file("b", 1).await.is_err(),
            "more files than announced"
        );

        // A file over the limit is skipped without touching the disk.
        let mut inc = Incoming::start(dir.path(), 1, 100).await.unwrap();
        inc.begin_file("huge", 101).await.unwrap();
        inc.data(&[0; 64]).await.unwrap();
        inc.end_file(0).await.unwrap();
        assert!(inc.finish().await.is_empty());
    }

    #[tokio::test]
    async fn hostile_names_stay_inside_the_transfer_directory() {
        let dir = tempfile::tempdir().unwrap();
        let mut inc = Incoming::start(dir.path(), 3, 100).await.unwrap();
        for name in ["../../etc/passwd", "C:\\Windows\\evil.exe", "/abs/olute"] {
            inc.begin_file(name, 0).await.unwrap();
            inc.end_file(crc32fast::hash(b"")).await.unwrap();
        }
        let batch = inc.dir.clone();
        let got = inc.finish().await;
        assert_eq!(got.len(), 3);
        assert!(got.iter().all(|p| p.parent() == Some(batch.as_path())));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn received_files_are_private_and_never_follow_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let mut inc = Incoming::start(dir.path(), 2, 100).await.unwrap();
        // An attacker-planted symlink with the incoming name must not be written through.
        let victim = dir.path().join("victim");
        std::fs::write(&victim, b"keep").unwrap();
        std::os::unix::fs::symlink(&victim, inc.dir.join("note.txt")).unwrap();
        inc.begin_file("note.txt", 2).await.unwrap();
        inc.data(b"hi").await.unwrap();
        inc.end_file(crc32fast::hash(b"hi")).await.unwrap();
        let got = inc.finish().await;
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
        assert_eq!(got[0].file_name().unwrap(), "note (2).txt");
        let mode = std::fs::metadata(&got[0]).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn names_are_sanitized() {
        assert_eq!(safe_name("../../.ssh/authorized_keys"), "authorized_keys");
        assert_eq!(safe_name("..\\..\\boot.ini"), "boot.ini");
        assert_eq!(safe_name(".."), "file");
        assert_eq!(safe_name(""), "file");
        assert_eq!(safe_name("a<b>:c|d?.txt"), "a_b__c_d_.txt");
        assert_eq!(safe_name("trailing. . "), "trailing");
        assert_eq!(safe_name("nul.txt"), "_nul.txt");
        assert_eq!(safe_name("COM7"), "_COM7");
        assert_eq!(safe_name("comet.txt"), "comet.txt");
        // Forms Windows also resolves to a device.
        assert_eq!(safe_name("NUL .txt"), "_NUL .txt");
        assert_eq!(safe_name("com1.tar.gz"), "_com1.tar.gz");
        assert_eq!(safe_name("LPT².log"), "_LPT².log");
        assert_eq!(safe_name("conin$"), "_conin$");
        assert_eq!(safe_name("aux."), "_aux");
        assert_eq!(safe_name("COM10.txt"), "COM10.txt");
        assert_eq!(safe_name("console.log"), "console.log");
        assert_eq!(safe_name(" . "), "file");
        assert_eq!(safe_name("tab\there"), "tab_here");
        let long = safe_name(&format!("{}.tar.gz", "ñ".repeat(400)));
        assert!(long.len() <= MAX_NAME_BYTES && long.ends_with(".gz"));
    }
}

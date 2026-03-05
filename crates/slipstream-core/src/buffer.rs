use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use parking_lot::RwLock;

/// Default maximum file size: 1MB.
const DEFAULT_MAX_FILE_SIZE: usize = 1_048_576;

/// A file loaded into memory as lines.
#[derive(Debug)]
pub struct FileBuffer {
    pub path: PathBuf,
    pub lines: Vec<String>,
    /// Whether the original file ended with a newline.
    pub trailing_newline: bool,
    /// Incremented on every flush that modifies this buffer.
    pub(crate) version: u64,
    /// Hash of the canonical representation (lines joined with \n, optional trailing \n).
    /// Used to detect external modifications between load and flush.
    pub(crate) disk_hash: u64,
}

/// Entry in the buffer pool: wraps a `FileBuffer` with a lock-free reference count.
///
/// `ref_count` lives outside the `FileBuffer` lock so that `open()` can increment it
/// without acquiring a read lock on the buffer (which would block if a flush holds
/// the write lock).
#[derive(Debug)]
struct PoolEntry {
    buffer: Arc<RwLock<FileBuffer>>,
    /// Number of sessions currently referencing this buffer.
    ref_count: AtomicUsize,
}

impl FileBuffer {
    /// Load a file from disk into a line-indexed buffer.
    pub fn load(path: &Path, max_file_size: usize) -> Result<Self, BufferError> {
        // Auto-create: if file doesn't exist, start with an empty buffer.
        // The file will be created on disk when the session is flushed.
        match std::fs::metadata(path) {
            Ok(metadata) => {
                let size = metadata.len() as usize;
                if size > max_file_size {
                    return Err(BufferError::FileTooLarge {
                        path: path.to_path_buf(),
                        size_bytes: size,
                        limit_bytes: max_file_size,
                    });
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(FileBuffer {
                    path: path.to_path_buf(),
                    lines: Vec::new(),
                    trailing_newline: true,
                    version: 1,
                    disk_hash: hash_content(""),
                });
            }
            Err(e) => return Err(BufferError::Io(e)),
        }

        let content = std::fs::read_to_string(path).map_err(|e| {
            if e.kind() == io::ErrorKind::InvalidData {
                BufferError::NotUtf8(path.to_path_buf())
            } else {
                BufferError::Io(e)
            }
        })?;

        let trailing_newline = content.ends_with('\n');
        let disk_hash = hash_content(&content);
        let lines: Vec<String> = content.lines().map(String::from).collect();

        Ok(FileBuffer {
            path: path.to_path_buf(),
            lines,
            trailing_newline,
            version: 1,
            disk_hash,
        })
    }

    /// Total number of lines.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Current buffer version.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Reconstruct file content from lines, preserving trailing newline state.
    pub fn reconstruct(lines: &[String], trailing_newline: bool) -> String {
        if lines.is_empty() {
            return String::new();
        }
        let mut content = lines.join("\n");
        if trailing_newline {
            content.push('\n');
        }
        content
    }

    /// Reconstruct this buffer's content as a string.
    pub fn to_content(&self) -> String {
        Self::reconstruct(&self.lines, self.trailing_newline)
    }
}

/// Extract just the file name from a path to avoid leaking full absolute paths
/// in error messages returned over RPC.
fn sanitize_path(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// Errors that can occur during buffer operations.
#[derive(Debug, thiserror::Error)]
pub enum BufferError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("file too large: {} is {size_bytes} bytes (limit: {limit_bytes}). Use direct filesystem access.", sanitize_path(path))]
    FileTooLarge {
        path: PathBuf,
        size_bytes: usize,
        limit_bytes: usize,
    },

    #[error("file is not valid UTF-8: {}", sanitize_path(.0))]
    NotUtf8(PathBuf),

    #[error("file not loaded: {}", sanitize_path(.0))]
    NotLoaded(PathBuf),
}

/// Simple content hash using FNV-1a for fast, non-cryptographic hashing.
pub(crate) fn hash_content(content: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in content.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Pool of shared file buffers.
///
/// Uses interior mutability (`RwLock`) so the pool can be shared across
/// async tasks and sessions without requiring `&mut self`.
#[derive(Debug)]
pub struct BufferPool {
    buffers: RwLock<HashMap<PathBuf, PoolEntry>>,
    path_cache: RwLock<HashMap<PathBuf, PathBuf>>,
    max_file_size: usize,
}

impl BufferPool {
    /// Create a new buffer pool with default settings.
    pub fn new() -> Self {
        BufferPool {
            buffers: RwLock::new(HashMap::new()),
            path_cache: RwLock::new(HashMap::new()),
            max_file_size: DEFAULT_MAX_FILE_SIZE,
        }
    }

    /// Create a new buffer pool with a custom file size limit.
    pub fn with_max_file_size(max_file_size: usize) -> Self {
        BufferPool {
            buffers: RwLock::new(HashMap::new()),
            path_cache: RwLock::new(HashMap::new()),
            max_file_size,
        }
    }

    /// Resolve a path to its canonical form, using cache to avoid repeated syscalls.
    pub fn canonicalize(&self, path: &Path) -> Result<PathBuf, BufferError> {
        // Fast path: check cache
        {
            let cache = self.path_cache.read();
            if let Some(canonical) = cache.get(path) {
                return Ok(canonical.clone());
            }
        }

        // Slow path: syscall + cache
        // For non-existent files, canonicalize parent dir + filename
        let canonical = match std::fs::canonicalize(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let parent = path.parent().unwrap_or(Path::new("."));
                let name = path.file_name().ok_or(BufferError::Io(e))?;
                let canonical_parent = std::fs::canonicalize(parent).map_err(BufferError::Io)?;
                canonical_parent.join(name)
            }
            Err(e) => return Err(BufferError::Io(e)),
        };
        let mut cache = self.path_cache.write();
        // Bounded cache: clear entirely if exceeding 1024 entries to prevent unbounded growth.
        if cache.len() >= 1024 {
            cache.clear();
        }
        cache.insert(path.to_path_buf(), canonical.clone());
        Ok(canonical)
    }

    /// Number of loaded buffers.
    pub fn loaded_count(&self) -> usize {
        self.buffers.read().len()
    }

    /// Load a file into the pool. If already loaded, increments ref_count and returns existing buffer.
    pub fn open(&self, path: &Path) -> Result<Arc<RwLock<FileBuffer>>, BufferError> {
        let canonical = self.canonicalize(path)?;

        // Fast path: check if already loaded (read lock on pool only, no FileBuffer lock needed)
        {
            let buffers = self.buffers.read();
            if let Some(entry) = buffers.get(&canonical) {
                entry.ref_count.fetch_add(1, Ordering::Relaxed);
                return Ok(Arc::clone(&entry.buffer));
            }
        }

        // Slow path: load file and insert (write lock on pool)
        let buffer = FileBuffer::load(&canonical, self.max_file_size)?;
        let arc = Arc::new(RwLock::new(buffer));

        let mut buffers = self.buffers.write();
        // Double-check: another thread may have inserted while we loaded
        if let Some(existing) = buffers.get(&canonical) {
            existing.ref_count.fetch_add(1, Ordering::Relaxed);
            return Ok(Arc::clone(&existing.buffer));
        }
        let entry = PoolEntry {
            buffer: Arc::clone(&arc),
            ref_count: AtomicUsize::new(1),
        };
        buffers.insert(canonical, entry);
        Ok(arc)
    }

    /// Release a reference to a buffer. Removes from pool when ref_count reaches 0.
    pub fn release(&self, path: &Path) -> Result<(), BufferError> {
        let canonical = self.canonicalize(path)?;

        let mut buffers = self.buffers.write();

        let should_remove = if let Some(entry) = buffers.get(&canonical) {
            // Release ordering ensures all prior accesses from this thread are visible
            // to the thread that observes the count reaching zero.
            let prev = entry.ref_count.fetch_sub(1, Ordering::Release);
            if prev == 1 {
                // Acquire fence synchronizes with all prior Release stores from other
                // threads, ensuring we see all their writes before we drop the buffer.
                // This follows the standard Arc drop pattern.
                std::sync::atomic::fence(Ordering::Acquire);
                true
            } else {
                false
            }
        } else {
            return Err(BufferError::NotLoaded(path.to_path_buf()));
        };

        if should_remove {
            buffers.remove(&canonical);
        }
        Ok(())
    }

    /// Get a buffer by path (must already be loaded).
    pub fn get(&self, path: &Path) -> Result<Arc<RwLock<FileBuffer>>, BufferError> {
        let canonical = self.canonicalize(path)?;
        let buffers = self.buffers.read();
        buffers
            .get(&canonical)
            .map(|entry| Arc::clone(&entry.buffer))
            .ok_or_else(|| BufferError::NotLoaded(path.to_path_buf()))
    }

    /// Get the current ref_count for a buffer (for testing/diagnostics).
    pub fn ref_count(&self, path: &Path) -> Result<usize, BufferError> {
        let canonical = self.canonicalize(path)?;
        let buffers = self.buffers.read();
        buffers
            .get(&canonical)
            .map(|entry| entry.ref_count.load(Ordering::Relaxed))
            .ok_or_else(|| BufferError::NotLoaded(path.to_path_buf()))
    }
}

impl Default for BufferPool {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn temp_file(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn load_file_into_lines() {
        let f = temp_file("line one\nline two\nline three\n");
        let buf = FileBuffer::load(f.path(), DEFAULT_MAX_FILE_SIZE).unwrap();
        assert_eq!(buf.lines, vec!["line one", "line two", "line three"]);
        assert_eq!(buf.version, 1);
        assert_eq!(buf.line_count(), 3);
    }

    #[test]
    fn load_empty_file() {
        let f = temp_file("");
        let buf = FileBuffer::load(f.path(), DEFAULT_MAX_FILE_SIZE).unwrap();
        assert_eq!(buf.lines, Vec::<String>::new());
        assert_eq!(buf.line_count(), 0);
    }

    #[test]
    fn load_file_no_trailing_newline() {
        let f = temp_file("line one\nline two");
        let buf = FileBuffer::load(f.path(), DEFAULT_MAX_FILE_SIZE).unwrap();
        assert_eq!(buf.lines, vec!["line one", "line two"]);
        assert!(!buf.trailing_newline);
    }

    #[test]
    fn load_file_with_trailing_newline() {
        let f = temp_file("line one\nline two\n");
        let buf = FileBuffer::load(f.path(), DEFAULT_MAX_FILE_SIZE).unwrap();
        assert_eq!(buf.lines, vec!["line one", "line two"]);
        assert!(buf.trailing_newline);
    }

    #[test]
    fn reconstruct_round_trips_with_trailing_newline() {
        let content = "hello\nworld\n";
        let f = temp_file(content);
        let buf = FileBuffer::load(f.path(), DEFAULT_MAX_FILE_SIZE).unwrap();
        assert_eq!(buf.to_content(), content);
    }

    #[test]
    fn reconstruct_round_trips_without_trailing_newline() {
        let content = "hello\nworld";
        let f = temp_file(content);
        let buf = FileBuffer::load(f.path(), DEFAULT_MAX_FILE_SIZE).unwrap();
        assert_eq!(buf.to_content(), content);
    }

    #[test]
    fn hash_matches_after_round_trip() {
        let content = "line one\nline two\nline three\n";
        let f = temp_file(content);
        let buf = FileBuffer::load(f.path(), DEFAULT_MAX_FILE_SIZE).unwrap();
        let reconstructed = buf.to_content();
        assert_eq!(hash_content(&reconstructed), buf.disk_hash);
    }

    #[test]
    fn reject_file_too_large() {
        let f = temp_file("hello world");
        let result = FileBuffer::load(f.path(), 5);
        match result {
            Err(BufferError::FileTooLarge { size_bytes, limit_bytes, .. }) => {
                assert_eq!(size_bytes, 11);
                assert_eq!(limit_bytes, 5);
            }
            other => panic!("expected FileTooLarge, got: {other:?}"),
        }
    }

    #[test]
    fn reject_non_utf8() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(&[0xFF, 0xFE, 0x00, 0x01]).unwrap();
        f.flush().unwrap();
        let result = FileBuffer::load(f.path(), DEFAULT_MAX_FILE_SIZE);
        assert!(matches!(result, Err(BufferError::NotUtf8(_))));
    }

    #[test]
    fn disk_hash_is_deterministic() {
        let f = temp_file("hello\nworld\n");
        let b1 = FileBuffer::load(f.path(), DEFAULT_MAX_FILE_SIZE).unwrap();
        let b2 = FileBuffer::load(f.path(), DEFAULT_MAX_FILE_SIZE).unwrap();
        assert_eq!(b1.disk_hash, b2.disk_hash);
    }

    #[test]
    fn disk_hash_changes_with_content() {
        let f1 = temp_file("hello\n");
        let f2 = temp_file("world\n");
        let b1 = FileBuffer::load(f1.path(), DEFAULT_MAX_FILE_SIZE).unwrap();
        let b2 = FileBuffer::load(f2.path(), DEFAULT_MAX_FILE_SIZE).unwrap();
        assert_ne!(b1.disk_hash, b2.disk_hash);
    }

    #[test]
    fn pool_open_and_get() {
        let f = temp_file("content\n");
        let pool = BufferPool::new();
        let buf = pool.open(f.path()).unwrap();
        assert_eq!(pool.ref_count(f.path()).unwrap(), 1);

        let buf2 = pool.get(f.path()).unwrap();
        assert_eq!(Arc::as_ptr(&buf), Arc::as_ptr(&buf2));
    }

    #[test]
    fn pool_open_twice_increments_ref_count() {
        let f = temp_file("content\n");
        let pool = BufferPool::new();
        pool.open(f.path()).unwrap();
        pool.open(f.path()).unwrap();

        assert_eq!(pool.ref_count(f.path()).unwrap(), 2);
    }

    #[test]
    fn pool_release_decrements_ref_count() {
        let f = temp_file("content\n");
        let pool = BufferPool::new();
        pool.open(f.path()).unwrap();
        pool.open(f.path()).unwrap();

        pool.release(f.path()).unwrap();
        assert_eq!(pool.ref_count(f.path()).unwrap(), 1);
    }

    #[test]
    fn pool_release_removes_at_zero() {
        let f = temp_file("content\n");
        let pool = BufferPool::new();
        pool.open(f.path()).unwrap();
        pool.release(f.path()).unwrap();

        assert!(matches!(pool.get(f.path()), Err(BufferError::NotLoaded(_))));
    }

    #[test]
    fn pool_respects_size_limit() {
        let f = temp_file("this is more than 5 bytes");
        let pool = BufferPool::with_max_file_size(5);
        assert!(matches!(pool.open(f.path()), Err(BufferError::FileTooLarge { .. })));
    }

    #[test]
    fn pool_file_not_found() {
        let pool = BufferPool::new();
        assert!(matches!(pool.open(Path::new("/nonexistent/file.txt")), Err(BufferError::Io(_))));
    }
}

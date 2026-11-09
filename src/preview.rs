use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Debug)]
pub enum PreviewError {
    Binary,
    Unsupported,
    Directory,
    Truncated,
    Cancelled,
    Io(std::io::Error),
}

impl PartialEq for PreviewError {
    fn eq(&self, other: &Self) -> bool {
        use std::mem::discriminant;
        if discriminant(self) == discriminant(other) {
            match (self, other) {
                (PreviewError::Io(a), PreviewError::Io(b)) => a.kind() == b.kind() && a.to_string() == b.to_string(),
                _ => true,
            }
        } else {
            false
        }
    }
}

impl Eq for PreviewError {}

impl std::fmt::Display for PreviewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PreviewError::Binary => write!(f, "file looks binary"),
            PreviewError::Unsupported => write!(f, "unsupported file type"),
            PreviewError::Directory => write!(f, "is a directory"),
            PreviewError::Truncated => write!(f, "preview truncated"),
            PreviewError::Cancelled => write!(f, "cancelled"),
            PreviewError::Io(e) => write!(f, "io error: {}", e),
        }
    }
}

impl From<std::io::Error> for PreviewError {
    fn from(e: std::io::Error) -> Self {
        PreviewError::Io(e)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    pub content: String,
    pub truncated: bool,
    pub line_count: usize,
}

#[derive(Debug, Clone)]
pub struct PreviewLimits {
    pub max_bytes: usize,
    pub max_lines: usize,
}

impl Default for PreviewLimits {
    fn default() -> Self {
        Self {
            max_bytes: 256 * 1024,
            max_lines: 2000,
        }
    }
}

impl PreviewLimits {
    pub fn new(max_bytes: usize, max_lines: usize) -> Self {
        Self { max_bytes, max_lines }
    }
}

const BINARY_SNIFF_BYTES: usize = 8192;

const ALLOWED_EXTENSIONS: &[&str] = &[
    "txt", "md", "log", "csv", "json", "toml", "yaml", "yml", "ini", "rs", "py", "js", "ts", "c", "h", "cpp",
    "cs", "xml", "html", "css", "sh", "ps1", "bat",
];

pub fn is_text_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| ALLOWED_EXTENSIONS.contains(&e))
        .unwrap_or(false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Generation {
    pub value: u64,
}

impl Generation {
    pub fn new(value: u64) -> Self {
        Self { value }
    }
}

#[derive(Debug, Clone)]
pub struct CancelToken {
    next_gen: Arc<AtomicU64>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self {
            next_gen: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn generate(&self) -> Generation {
        Generation {
            value: self.next_gen.fetch_add(1, Ordering::SeqCst),
        }
    }

    pub fn current_generation(&self) -> Generation {
        Generation {
            value: self.next_gen.load(Ordering::SeqCst).saturating_sub(1),
        }
    }

    pub fn is_cancelled(&self, gen: &Generation) -> bool {
        self.next_gen.load(Ordering::SeqCst) > gen.value.saturating_add(1)
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

pub fn load_text_preview<P: AsRef<Path>>(
    path: P,
    limits: PreviewLimits,
    cancel: &CancelToken,
) -> Result<Preview, PreviewError> {
    let path = path.as_ref();
    if path.is_dir() {
        return Err(PreviewError::Directory);
    }
    if !is_text_extension(path) {
        return Err(PreviewError::Unsupported);
    }

    let gen = cancel.current_generation();
    let mut file = File::open(path).map_err(PreviewError::Io)?;
    let mut reader = BufReader::new(&mut file);
    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut truncated = false;

    loop {
        if cancel.is_cancelled(&gen) {
            return Err(PreviewError::Cancelled);
        }
        if buffer.len() >= limits.max_bytes {
            truncated = true;
            break;
        }
        let remaining = (limits.max_bytes - buffer.len()).min(chunk.len());
        let n = reader.read(&mut chunk[..remaining])?;
        if n == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..n]);
    }

    let sniff_len = buffer.len().min(BINARY_SNIFF_BYTES);
    if buffer[..sniff_len].contains(&0) {
        return Err(PreviewError::Binary);
    }

    let mut bytes = buffer;
    if bytes.starts_with(b"\xEF\xBB\xBF") {
        bytes.drain(0..3);
    }

    let content = String::from_utf8_lossy(&bytes).into_owned();

    let mut lines: Vec<&str> = content.lines().collect();
    let mut line_count = lines.len();
    if line_count > limits.max_lines {
        lines.truncate(limits.max_lines);
        line_count = limits.max_lines;
        truncated = true;
    }
    let content = lines.join("\n");

    Ok(Preview {
        content,
        truncated,
        line_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("spacepeek_test_{}", name))
    }

    fn write_file(name: &str, bytes: &[u8]) -> PathBuf {
        let p = temp_path(name);
        fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn bounded_read_truncates_oversized_file() {
        let mut big = Vec::new();
        big.extend(std::iter::repeat(b'a').take(300));
        let p = write_file("trunc.txt", &big);
        let limits = PreviewLimits::new(64, 10);
        let token = CancelToken::new();
        let preview = load_text_preview(&p, limits, &token).unwrap();
        assert!(preview.content.len() <= 64);
        assert!(preview.truncated);
        assert_eq!(preview.line_count, 1);
    }

    #[test]
    fn binary_file_rejected() {
        let p = write_file("bin.txt", b"ok\x00binary");
        let token = CancelToken::new();
        assert_eq!(
            load_text_preview(&p, PreviewLimits::default(), &token),
            Err(PreviewError::Binary)
        );
    }

    #[test]
    fn unsupported_extension_rejected() {
        let p = temp_path("unsupported.xyz");
        fs::write(&p, b"hello").unwrap();
        let token = CancelToken::new();
        assert_eq!(
            load_text_preview(&p, PreviewLimits::default(), &token),
            Err(PreviewError::Unsupported)
        );
    }

    #[test]
    fn bom_is_stripped() {
        let p = write_file("bom.txt", b"\xEF\xBB\xBFhello world");
        let token = CancelToken::new();
        let preview = load_text_preview(&p, PreviewLimits::default(), &token).unwrap();
        assert_eq!(preview.content, "hello world");
    }

    #[test]
    fn invalid_utf8_is_lossy() {
        let p = write_file("lossy.txt", b"hello \xFF world");
        let token = CancelToken::new();
        let preview = load_text_preview(&p, PreviewLimits::default(), &token).unwrap();
        assert!(preview.content.contains('\u{FFFD}'));
    }

    #[test]
    fn line_cap_applied() {
        let mut data = String::new();
        for i in 0..3000 {
            data.push_str(&format!("line{}\n", i));
        }
        let p = write_file("lines.txt", data.as_bytes());
        let token = CancelToken::new();
        let preview = load_text_preview(&p, PreviewLimits::default(), &token).unwrap();
        assert_eq!(preview.line_count, 2000);
        assert!(preview.truncated);
    }

    #[test]
    fn cancelled_token_returns_cancelled() {
        let token = CancelToken::new();
        token.generate(); // gen0 live
        token.generate(); // gen1 live; gen0 is now stale
        assert!(token.is_cancelled(&Generation::new(0)));
        assert!(!token.is_cancelled(&token.current_generation()));

        let p = write_file("cancelled.txt", b"hello");
        let live_token = CancelToken::new();
        live_token.generate();
        assert_eq!(
            load_text_preview(&p, PreviewLimits::default(), &live_token).unwrap().content,
            "hello"
        );
    }

    #[test]
    fn directory_rejected() {
        let dir = std::env::temp_dir().join("spacepeek_test_dir");
        std::fs::create_dir_all(&dir).unwrap();
        let token = CancelToken::new();
        assert_eq!(
            load_text_preview(&dir, PreviewLimits::default(), &token),
            Err(PreviewError::Directory)
        );
    }
}

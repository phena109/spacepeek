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
    Cancelled,
    Io(std::io::Error),
}

impl PartialEq for PreviewError {
    fn eq(&self, other: &Self) -> bool {
        use std::mem::discriminant;
        if discriminant(self) == discriminant(other) {
            match (self, other) {
                (PreviewError::Io(a), PreviewError::Io(b)) => {
                    a.kind() == b.kind() && a.to_string() == b.to_string()
                }
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
        Self {
            max_bytes,
            max_lines,
        }
    }
}

const BINARY_SNIFF_BYTES: usize = 8192;

const ALLOWED_EXTENSIONS: &[&str] = &[
    "txt", "md", "log", "csv", "json", "toml", "yaml", "yml", "ini", "rs", "py", "js", "ts", "c",
    "h", "cpp", "cs", "xml", "html", "css", "sh", "ps1", "bat",
];

pub fn is_text_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| ALLOWED_EXTENSIONS.iter().any(|a| a.eq_ignore_ascii_case(e)))
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
    gen: Generation,
) -> Result<Preview, PreviewError> {
    let path = path.as_ref();
    if path.is_dir() {
        return Err(PreviewError::Directory);
    }
    if !is_text_extension(path) {
        return Err(PreviewError::Unsupported);
    }

    if cancel.is_cancelled(&gen) {
        return Err(PreviewError::Cancelled);
    }

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

    // UTF-16 BOM must be recognised before the NUL sniff: ASCII in UTF-16 is
    // full of 0x00 bytes. UTF-32LE is FF FE 00 00; we do not support UTF-32
    // and decode that prefix as UTF-16LE.
    let content = if buffer.starts_with(&[0xFF, 0xFE]) {
        decode_utf16_payload(&buffer[2..], true, &mut truncated)
    } else if buffer.starts_with(&[0xFE, 0xFF]) {
        decode_utf16_payload(&buffer[2..], false, &mut truncated)
    } else {
        let sniff_len = buffer.len().min(BINARY_SNIFF_BYTES);
        if buffer[..sniff_len].contains(&0) {
            return Err(PreviewError::Binary);
        }
        let mut bytes = buffer;
        if bytes.starts_with(b"\xEF\xBB\xBF") {
            bytes.drain(0..3);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    };

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

fn decode_utf16_payload(payload: &[u8], little_endian: bool, truncated: &mut bool) -> String {
    let payload = if payload.len() % 2 == 1 {
        *truncated = true;
        &payload[..payload.len() - 1]
    } else {
        payload
    };
    let units: Vec<u16> = payload
        .chunks_exact(2)
        .map(|pair| {
            if little_endian {
                u16::from_le_bytes([pair[0], pair[1]])
            } else {
                u16::from_be_bytes([pair[0], pair[1]])
            }
        })
        .collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn write_file(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.path().join(name);
        fs::write(&p, bytes).unwrap();
        p
    }

    fn encode_utf16(s: &str, little_endian: bool) -> Vec<u8> {
        let mut out = if little_endian {
            vec![0xFF, 0xFE]
        } else {
            vec![0xFE, 0xFF]
        };
        for unit in s.encode_utf16() {
            if little_endian {
                out.extend_from_slice(&unit.to_le_bytes());
            } else {
                out.extend_from_slice(&unit.to_be_bytes());
            }
        }
        out
    }

    fn live_token() -> (CancelToken, Generation) {
        let token = CancelToken::new();
        let gen = token.generate();
        (token, gen)
    }

    #[test]
    fn bounded_read_truncates_oversized_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut big = Vec::new();
        big.extend(std::iter::repeat_n(b'a', 300));
        let p = write_file(&dir, "trunc.txt", &big);
        let limits = PreviewLimits::new(64, 10);
        let (token, gen) = live_token();
        let preview = load_text_preview(&p, limits, &token, gen).unwrap();
        assert!(preview.content.len() <= 64);
        assert!(preview.truncated);
        assert_eq!(preview.line_count, 1);
    }

    #[test]
    fn binary_file_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_file(&dir, "bin.txt", b"ok\x00binary");
        let (token, gen) = live_token();
        assert_eq!(
            load_text_preview(&p, PreviewLimits::default(), &token, gen),
            Err(PreviewError::Binary)
        );
    }

    #[test]
    fn unsupported_extension_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("unsupported.xyz");
        fs::write(&p, b"hello").unwrap();
        let (token, gen) = live_token();
        assert_eq!(
            load_text_preview(&p, PreviewLimits::default(), &token, gen),
            Err(PreviewError::Unsupported)
        );
    }

    #[test]
    fn extensionless_file_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("extensionless");
        fs::write(&p, b"hello").unwrap();
        let (token, gen) = live_token();
        assert_eq!(
            load_text_preview(&p, PreviewLimits::default(), &token, gen),
            Err(PreviewError::Unsupported)
        );
    }

    #[test]
    fn case_insensitive_extensions() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = write_file(&dir, "README.TXT", b"readme content");
        let p2 = write_file(&dir, "notes.Md", b"notes content");
        let p3 = write_file(&dir, "SETUP.LOG", b"setup log");
        let (token, gen) = live_token();
        assert_eq!(
            load_text_preview(&p1, PreviewLimits::default(), &token, gen)
                .unwrap()
                .content,
            "readme content"
        );
        assert_eq!(
            load_text_preview(&p2, PreviewLimits::default(), &token, gen)
                .unwrap()
                .content,
            "notes content"
        );
        assert_eq!(
            load_text_preview(&p3, PreviewLimits::default(), &token, gen)
                .unwrap()
                .content,
            "setup log"
        );
    }

    #[test]
    fn bom_is_stripped() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_file(&dir, "bom.txt", b"\xEF\xBB\xBFhello world");
        let (token, gen) = live_token();
        let preview = load_text_preview(&p, PreviewLimits::default(), &token, gen).unwrap();
        assert_eq!(preview.content, "hello world");
    }

    #[test]
    fn utf16le_bom_is_decoded() {
        let dir = tempfile::tempdir().unwrap();
        let text = "hello world é\nsecond line";
        let p = write_file(&dir, "utf16le.txt", &encode_utf16(text, true));
        let (token, gen) = live_token();
        let preview = load_text_preview(&p, PreviewLimits::default(), &token, gen).unwrap();
        assert_eq!(preview.content, text);
        assert!(!preview.truncated);
        assert_eq!(preview.line_count, 2);
    }

    #[test]
    fn utf16be_bom_is_decoded() {
        let dir = tempfile::tempdir().unwrap();
        let text = "hello world é";
        let p = write_file(&dir, "utf16be.txt", &encode_utf16(text, false));
        let (token, gen) = live_token();
        let preview = load_text_preview(&p, PreviewLimits::default(), &token, gen).unwrap();
        assert_eq!(preview.content, text);
        assert!(!preview.truncated);
    }

    #[test]
    fn utf16_odd_byte_cap_is_truncated_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_file(&dir, "utf16odd.txt", &encode_utf16("hello world", true));
        // BOM (2) + 'h' (2) + first byte of 'e' = 5, so the last code unit is cut.
        let limits = PreviewLimits::new(5, 10);
        let (token, gen) = live_token();
        let preview = load_text_preview(&p, limits, &token, gen).unwrap();
        assert!(preview.truncated);
        assert_eq!(preview.content, "h");
    }

    #[test]
    fn invalid_utf8_is_lossy() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_file(&dir, "lossy.txt", b"hello \xFF world");
        let (token, gen) = live_token();
        let preview = load_text_preview(&p, PreviewLimits::default(), &token, gen).unwrap();
        assert!(preview.content.contains('\u{FFFD}'));
    }

    #[test]
    fn line_cap_applied() {
        let dir = tempfile::tempdir().unwrap();
        let mut data = String::new();
        for i in 0..3000 {
            data.push_str(&format!("line{}\n", i));
        }
        let p = write_file(&dir, "lines.txt", data.as_bytes());
        let (token, gen) = live_token();
        let preview = load_text_preview(&p, PreviewLimits::default(), &token, gen).unwrap();
        assert_eq!(preview.line_count, 2000);
        assert!(preview.truncated);
    }

    #[test]
    fn stale_generation_returns_cancelled() {
        let token = CancelToken::new();
        let stale = token.generate();
        let _live = token.generate();
        assert!(token.is_cancelled(&stale));

        let dir = tempfile::tempdir().unwrap();
        let p = write_file(&dir, "cancelled.txt", b"hello");
        assert_eq!(
            load_text_preview(&p, PreviewLimits::default(), &token, stale),
            Err(PreviewError::Cancelled)
        );
    }

    #[test]
    fn directory_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let sub_dir = dir.path().join("spacepeek_test_dir");
        std::fs::create_dir_all(&sub_dir).unwrap();
        let (token, gen) = live_token();
        assert_eq!(
            load_text_preview(&sub_dir, PreviewLimits::default(), &token, gen),
            Err(PreviewError::Directory)
        );
    }
}

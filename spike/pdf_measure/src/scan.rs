//! Best-effort scan for a PDF `/Encrypt` dictionary.
//!
//! The trailer that points at `/Encrypt` is not itself encrypted. This does not
//! prove a user password is required; `PdfDocument.IsPasswordProtected` is the
//! authority when the document loads.

#![cfg_attr(not(windows), allow(dead_code))]

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const WINDOW: u64 = 1024 * 1024;

pub fn encrypt_dict_present(path: &Path) -> std::io::Result<bool> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut buf = Vec::new();
    if len <= WINDOW.saturating_mul(2) {
        file.read_to_end(&mut buf)?;
    } else {
        let mut head = vec![0_u8; WINDOW as usize];
        let n = file.read(&mut head)?;
        head.truncate(n);
        let tail_len = WINDOW.min(len);
        file.seek(SeekFrom::End(-(tail_len as i64)))?;
        let mut tail = Vec::new();
        file.read_to_end(&mut tail)?;
        buf = head;
        buf.extend_from_slice(&tail);
    }
    Ok(contains_encrypt(&buf))
}

fn contains_encrypt(buf: &[u8]) -> bool {
    buf.windows(b"/Encrypt".len())
        .any(|window| window == b"/Encrypt")
}

#[cfg(test)]
mod tests {
    use super::contains_encrypt;
    use super::encrypt_dict_present;
    use std::io::Write;

    #[test]
    fn detects_trailer_token() {
        assert!(contains_encrypt(b"%PDF-1.6\ntrailer <</Encrypt 2 0 R>>"));
        assert!(!contains_encrypt(b"%PDF-1.6\ntrailer <</Size 3>>"));
    }

    #[test]
    fn reads_a_small_file() {
        let path =
            std::env::temp_dir().join(format!("pdf_measure_scan_{}.pdf", std::process::id()));
        {
            let mut file = std::fs::File::create(&path).unwrap();
            file.write_all(b"%PDF-1.4\n1 0 obj <</Encrypt 2 0 R>> endobj\n%%EOF")
                .unwrap();
        }
        assert_eq!(encrypt_dict_present(&path).unwrap(), true);
        let _ = std::fs::remove_file(&path);
    }
}

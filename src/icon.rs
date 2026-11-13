//! Icon sizes and the on-disk ICO directory.
//!
//! Pixel sizes are `MulDiv` of the 96 DPI sizes, the same way preview layout
//! scales. Win32 loads the embedded resource; this module does not decode
//! pixels. The directory parser exists so tests can check `assets/spacepeek.ico`
//! without a Windows loader.

use crate::geometry::{effective_dpi, scale_for_dpi};

/// Resource id `winresource` assigns in `WindowsResource::set_icon`.
pub const ICON_RESOURCE_ID: u16 = 1;

/// Tray and caption icon at 96 DPI.
pub const SMALL_ICON_PX_96: i32 = 16;
/// Window-class and Alt-Tab icon at 96 DPI.
pub const LARGE_ICON_PX_96: i32 = 32;

/// `ICONDIRENTRY` is 16 bytes. Width or height `0` means 256.
#[cfg(test)]
const ICONDIR_LEN: usize = 6;
#[cfg(test)]
const ENTRY_LEN: usize = 16;

/// Small icon side, in physical pixels, for `dpi`. A missing DPI (`0`) is 96.
pub fn small_icon_px(dpi: u32) -> i32 {
    scale_for_dpi(SMALL_ICON_PX_96, effective_dpi(dpi))
}

/// Large icon side, in physical pixels, for `dpi`. A missing DPI (`0`) is 96.
pub fn large_icon_px(dpi: u32) -> i32 {
    scale_for_dpi(LARGE_ICON_PX_96, effective_dpi(dpi))
}

/// One image in an ICO directory. `width` and `height` are pixels; a stored
/// `0` is reported as 256. `offset` is the byte offset of that image.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IconDirectoryEntry {
    width: u16,
    height: u16,
    offset: u32,
}

/// `ICONDIR` plus its entries. Image bytes are not decoded.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct IconDirectory {
    reserved: u16,
    /// `1` is an icon, `2` is a cursor.
    kind: u16,
    entries: Vec<IconDirectoryEntry>,
}

/// Read the ICO header and directory. `None` when the buffer is shorter than
/// the directory it claims. Bytes after the directory are not inspected.
#[cfg(test)]
fn parse_icon_directory(data: &[u8]) -> Option<IconDirectory> {
    if data.len() < ICONDIR_LEN {
        return None;
    }
    let reserved = u16::from_le_bytes([data[0], data[1]]);
    let kind = u16::from_le_bytes([data[2], data[3]]);
    let count = usize::from(u16::from_le_bytes([data[4], data[5]]));
    let entries_len = count.checked_mul(ENTRY_LEN)?;
    let directory_end = ICONDIR_LEN.checked_add(entries_len)?;
    if data.len() < directory_end {
        return None;
    }
    let mut entries = Vec::with_capacity(count);
    for index in 0..count {
        let at = ICONDIR_LEN + index * ENTRY_LEN;
        entries.push(IconDirectoryEntry {
            width: icon_dim(data[at]),
            height: icon_dim(data[at + 1]),
            offset: u32::from_le_bytes([
                data[at + 12],
                data[at + 13],
                data[at + 14],
                data[at + 15],
            ]),
        });
    }
    Some(IconDirectory {
        reserved,
        kind,
        entries,
    })
}

#[cfg(test)]
fn icon_dim(raw: u8) -> u16 {
    if raw == 0 {
        256
    } else {
        u16::from(raw)
    }
}

/// `true` when the image at `entry.offset` begins with a PNG signature.
/// Anything else, including a short buffer, is `false`. No pixel data is read.
#[cfg(test)]
fn icon_image_is_png(data: &[u8], entry: &IconDirectoryEntry) -> bool {
    const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let Ok(start) = usize::try_from(entry.offset) else {
        return false;
    };
    let Some(end) = start.checked_add(PNG.len()) else {
        return false;
    };
    data.get(start..end) == Some(&PNG)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn icon_resource_id_matches_winresource_default() {
        assert_eq!(ICON_RESOURCE_ID, 1);
    }

    #[test]
    fn icon_px_follows_dpi_steps() {
        assert_eq!(small_icon_px(96), 16);
        assert_eq!(large_icon_px(96), 32);
        assert_eq!(small_icon_px(0), 16);
        assert_eq!(large_icon_px(0), 32);

        // 125%, 150%, 200%, 250%. The shipped ICO has 20, 24, 40, 48, and 64
        // so the small sizes and most large sizes land on a real image.
        assert_eq!(small_icon_px(120), 20);
        assert_eq!(large_icon_px(120), 40);
        assert_eq!(small_icon_px(144), 24);
        assert_eq!(large_icon_px(144), 48);
        assert_eq!(small_icon_px(192), 32);
        assert_eq!(large_icon_px(192), 64);
        assert_eq!(small_icon_px(240), 40);
        assert_eq!(large_icon_px(240), 80);
    }

    #[test]
    fn spacepeek_ico_lists_eight_sizes_and_png_256() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("spacepeek.ico");
        let data = std::fs::read(&path).expect("read assets/spacepeek.ico");
        let dir = parse_icon_directory(&data).expect("icon directory");
        assert_eq!(dir.reserved, 0);
        assert_eq!(dir.kind, 1);
        assert_eq!(dir.entries.len(), 8);
        let sizes: Vec<(u16, u16)> = dir.entries.iter().map(|e| (e.width, e.height)).collect();
        assert_eq!(
            sizes,
            vec![
                (16, 16),
                (20, 20),
                (24, 24),
                (32, 32),
                (40, 40),
                (48, 48),
                (64, 64),
                (256, 256),
            ]
        );
        let large = dir
            .entries
            .iter()
            .find(|entry| entry.width == 256)
            .expect("256 entry");
        assert!(icon_image_is_png(&data, large));
    }

    #[test]
    fn truncated_directory_is_rejected_and_zero_means_256() {
        assert!(parse_icon_directory(&[]).is_none());
        assert!(parse_icon_directory(&[0, 0, 1, 0, 1, 0]).is_none());

        let mut data = vec![0, 0, 1, 0, 1, 0];
        data.extend_from_slice(&[
            0, 0, // width and height bytes: 0 means 256
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ]);
        let dir = parse_icon_directory(&data).expect("one entry");
        assert_eq!(dir.entries.len(), 1);
        assert_eq!(dir.entries[0].width, 256);
        assert_eq!(dir.entries[0].height, 256);
        assert!(!icon_image_is_png(&data, &dir.entries[0]));
    }
}

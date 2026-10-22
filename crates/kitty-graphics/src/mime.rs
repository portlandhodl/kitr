// SPDX-License-Identifier: GPL-3.0-or-later
//! MIME detection for image files and image data.
//!
//! Ported from the lookup policy in `kitty/mime-cache.c`, restricted to the
//! image types the graphics protocol understands. Kitty consults the
//! shared-mime-info binary cache (`mime.cache`) via `mmap`; this port embeds
//! the small set of literal globs and suffix rules relevant to images so that
//! it works without the XDG MIME database, filesystem access or a network.
//!
//! The lookup order mirrors xdgmime/kitty: literal names, suffix rules,
//! glob rules, then highest weight wins (ties go to the earliest match). Since
//! our table only contains suffix rules, a single case-insensitive suffix
//! lookup is sufficient.
//!
//! Copyright (C) 2026 Kovid Goyal <kovid at kovidgoyal.net>

/// Canonical MIME type for an image path, or `None` for unknown extensions.
///
/// Only the final path component is considered. Matching is case-insensitive,
/// like the lowercased suffix pass in xdgmime.
pub fn mime_type_for_filename(path: &str) -> Option<&'static str> {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    if name.is_empty() || name.contains('\0') {
        return None;
    }
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty() && name.starts_with('.') {
        // A dotfile such as `.png` has no suffix according to xdgmime.
        return None;
    }
    let ext = ext.to_ascii_lowercase();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "apng" => "image/apng",
        "jpg" | "jpeg" | "jpe" | "jfif" | "pjpeg" | "pjp" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" | "svgz" | "svg.gz" => "image/svg+xml",
        "bmp" | "dib" => "image/bmp",
        "ico" => "image/vnd.microsoft.icon",
        "tif" | "tiff" => "image/tiff",
        "pnm" | "pbm" | "pgm" | "ppm" | "pam" => "image/x-portable-anymap",
        "tga" | "icb" | "vda" | "vst" => "image/x-tga",
        "xcf" => "image/x-xcf",
        "psd" => "image/vnd.adobe.photoshop",
        "dds" => "image/vnd-ms.dds",
        "exr" => "image/x-exr",
        "hdr" => "image/vnd.radiance",
        "avif" => "image/avif",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "ktx" | "ktx2" => "image/ktx",
        "qoi" => "image/qoi",
        "jxl" => "image/jxl",
        _ => return None,
    };
    Some(mime)
}

/// Returns the canonical name first, followed by known aliases/related names.
///
/// This is the subset of kitty's `mime_type_aliases()` relevant to images.
/// For an unknown MIME type the input is returned unchanged as the only item.
pub fn mime_type_aliases(mime: &str) -> Vec<String> {
    let lower = mime.to_ascii_lowercase();
    let canonical = match lower.as_str() {
        "image/jpg" | "image/pjpeg" | "image/x-jpeg" => "image/jpeg",
        "image/x-png" => "image/png",
        "image/svg" => "image/svg+xml",
        "image/x-bmp" | "image/x-ms-bmp" | "image/bmp" => "image/bmp",
        "image/x-icon" | "image/ico" => "image/vnd.microsoft.icon",
        "image/x-tiff" | "image/tif" => "image/tiff",
        "image/x-webp" => "image/webp",
        "image/x-portable-bitmap" | "image/x-portable-graymap" | "image/x-portable-pixmap" => {
            "image/x-portable-anymap"
        }
        other => other,
    };
    let mut ans = vec![canonical.to_string()];
    let aliases: &[&str] = match canonical {
        "image/png" => &["image/x-png"],
        "image/jpeg" => &["image/jpg", "image/pjpeg", "image/x-jpeg"],
        "image/svg+xml" => &["image/svg"],
        "image/bmp" => &["image/x-bmp", "image/x-ms-bmp"],
        "image/vnd.microsoft.icon" => &["image/x-icon", "image/ico"],
        "image/tiff" => &["image/x-tiff", "image/tif"],
        "image/webp" => &["image/x-webp"],
        "image/x-portable-anymap" => &[
            "image/x-portable-bitmap",
            "image/x-portable-graymap",
            "image/x-portable-pixmap",
        ],
        _ => &[],
    };
    for alias in aliases {
        if !ans.iter().any(|x| x == alias) {
            ans.push((*alias).to_string());
        }
    }
    let original = mime.to_ascii_lowercase();
    if !ans.contains(&original) {
        ans.push(mime.to_string());
    }
    ans
}

/// Sniff an image MIME type from the leading bytes of a file/data stream.
///
/// Only formats with reliable magic bytes are detected. SVG is detected
/// textually (XML declaration or `<svg` in the first 4096 bytes).
pub fn mime_type_for_data(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if data.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if data.starts_with(b"BM") {
        return Some("image/bmp");
    }
    if data.starts_with(b"II*\0") || data.starts_with(b"MM\0*") {
        return Some("image/tiff");
    }
    if data.starts_with(&[0x00, 0x00, 0x01, 0x00]) {
        return Some("image/vnd.microsoft.icon");
    }
    if data.len() >= 12 && &data[4..8] == b"ftyp" {
        match &data[8..12] {
            b"avif" | b"avis" => return Some("image/avif"),
            b"heic" | b"heix" | b"hevc" | b"hevx" | b"mif1" | b"msf1" => return Some("image/heif"),
            _ => {}
        }
    }
    if data.len() >= 4
        && data[0] == b'P'
        && (b'1'..=b'6').contains(&data[1])
        && (data.len() == 2 || data[2].is_ascii_whitespace())
    {
        return Some("image/x-portable-anymap");
    }
    if data.starts_with(b"DDS ") {
        return Some("image/vnd-ms.dds");
    }
    if data.starts_with(&[0x76, 0x2f, 0x31, 0x01]) {
        return Some("image/x-exr");
    }
    if data.starts_with(b"#?RADIANCE") || data.starts_with(b"#?RGBE") {
        return Some("image/vnd.radiance");
    }
    if looks_like_svg(data) {
        return Some("image/svg+xml");
    }
    None
}

fn looks_like_svg(data: &[u8]) -> bool {
    let mut start = 0;
    if data.starts_with(&[0xef, 0xbb, 0xbf]) {
        start = 3;
    }
    while start < data.len() && data[start].is_ascii_whitespace() {
        start += 1;
    }
    let head = &data[start..data.len().min(start + 4096)];
    let lower: Vec<u8> = head.iter().map(u8::to_ascii_lowercase).collect();
    lower.starts_with(b"<svg")
        || lower.starts_with(b"<?xml")
        || lower.starts_with(b"<!doctype svg")
        || (lower.starts_with(b"<!--") && lower.windows(4).any(|w| w == b"<svg"))
}

/// True if the canonical MIME type is an image type supported by the graphics
/// protocol/loader.
pub fn is_image_mime(mime: &str) -> bool {
    matches!(
        mime,
        "image/png"
            | "image/apng"
            | "image/jpeg"
            | "image/gif"
            | "image/webp"
            | "image/svg+xml"
            | "image/bmp"
            | "image/vnd.microsoft.icon"
            | "image/tiff"
            | "image/x-portable-anymap"
            | "image/x-tga"
            | "image/x-xcf"
            | "image/vnd.adobe.photoshop"
            | "image/vnd-ms.dds"
            | "image/x-exr"
            | "image/vnd.radiance"
            | "image/avif"
            | "image/heic"
            | "image/heif"
            | "image/ktx"
            | "image/qoi"
            | "image/jxl"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_are_case_insensitive_and_path_aware() {
        assert_eq!(mime_type_for_filename("foo.PNG"), Some("image/png"));
        assert_eq!(
            mime_type_for_filename("/tmp/a.b/c.JpEg"),
            Some("image/jpeg")
        );
        assert_eq!(mime_type_for_filename("dir.svg"), Some("image/svg+xml"));
        assert_eq!(mime_type_for_filename("dir.webp"), Some("image/webp"));
        assert_eq!(mime_type_for_filename("dir.gif"), Some("image/gif"));
        assert_eq!(mime_type_for_filename("noext"), None);
        assert_eq!(mime_type_for_filename(".png"), None);
        assert_eq!(mime_type_for_filename("a."), None);
    }

    #[test]
    fn file_extension_mapping_covers_common_images() {
        let cases = [
            ("x.png", "image/png"),
            ("x.apng", "image/apng"),
            ("x.jpg", "image/jpeg"),
            ("x.jpeg", "image/jpeg"),
            ("x.gif", "image/gif"),
            ("x.webp", "image/webp"),
            ("x.svg", "image/svg+xml"),
            ("x.bmp", "image/bmp"),
            ("x.ico", "image/vnd.microsoft.icon"),
            ("x.tiff", "image/tiff"),
            ("x.pnm", "image/x-portable-anymap"),
            ("x.tga", "image/x-tga"),
            ("x.avif", "image/avif"),
        ];
        for (name, mime) in cases {
            assert_eq!(mime_type_for_filename(name), Some(mime), "{name}");
        }
    }

    #[test]
    fn sniffs_magic_bytes() {
        assert_eq!(
            mime_type_for_data(b"\x89PNG\r\n\x1a\nrest"),
            Some("image/png")
        );
        assert_eq!(mime_type_for_data(b"\xff\xd8\xff\xe0"), Some("image/jpeg"));
        assert_eq!(mime_type_for_data(b"GIF89a...."), Some("image/gif"));
        assert_eq!(
            mime_type_for_data(b"RIFF\0\0\0\0WEBPVP8 "),
            Some("image/webp")
        );
        assert_eq!(mime_type_for_data(b"BM...."), Some("image/bmp"));
        assert_eq!(mime_type_for_data(b"II*\0...."), Some("image/tiff"));
        assert_eq!(
            mime_type_for_data(b"P6\n2 2\n255\n"),
            Some("image/x-portable-anymap")
        );
        assert_eq!(mime_type_for_data(b"not an image"), None);
    }

    #[test]
    fn sniffs_svg() {
        assert_eq!(
            mime_type_for_data(b"<?xml version=\"1.0\"?><svg xmlns=\"...\"/>"),
            Some("image/svg+xml")
        );
        assert_eq!(
            mime_type_for_data(b"  \n<svg xmlns=\"http://www.w3.org/2000/svg\"/>"),
            Some("image/svg+xml")
        );
        assert_eq!(mime_type_for_data(b"<html><svg></svg></html>"), None);
    }

    #[test]
    fn aliases_are_canonicalised() {
        assert_eq!(
            mime_type_aliases("image/jpg"),
            vec!["image/jpeg", "image/jpg", "image/pjpeg", "image/x-jpeg"]
        );
        assert_eq!(
            mime_type_aliases("image/x-png"),
            vec!["image/png", "image/x-png"]
        );
        assert_eq!(mime_type_aliases("text/plain"), vec!["text/plain"]);
    }

    #[test]
    fn image_mime_predicate() {
        assert!(is_image_mime("image/png"));
        assert!(is_image_mime("image/svg+xml"));
        assert!(!is_image_mime("text/plain"));
        assert!(!is_image_mime("image/x-unknown"));
    }
}

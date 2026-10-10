//! Media previews: what a file card says about its image, video, audio or voice message, so
//! members see it in the chat instead of only as a file. Pure Rust, no browser APIs.
//!
//! - `MediaInfo` travels inside the signed `FileOffer`: kind, size in pixels, length, a small
//!   thumbnail and, for voice messages, a waveform. Every member checks it with
//!   `MediaInfo::is_valid` before showing or logging the offer, so logs never differ.
//! - `preview_type` is the allow-list of types shown inline. SVG is never in it: a `blob:` URL
//!   has dchat's own origin, so an SVG opened from one could run scripts as dchat.
//! - `strip_jpeg_gps` removes a photo's location before sending, without re-encoding it.

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};

/// Largest thumbnail, decoded: a small blurred preview, never the picture itself.
pub const THUMB_MAX_BYTES: usize = 12 * 1024;
/// Thumbnail encodings members accept.
pub const THUMB_TYPES: [&str; 2] = ["image/webp", "image/jpeg"];
/// Bars in a voice message's waveform.
pub const WAVEFORM_BARS: usize = 64;
/// Larger widths or heights are refused.
pub const MAX_MEDIA_DIMENSION: u32 = 16_384;
/// Longer durations are refused.
pub const MAX_MEDIA_DURATION_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum MediaKind {
    Image,
    Video,
    Audio,
    /// Recorded in dchat: shown as a voice message with its waveform.
    Voice,
}

/// A small preview image, base64 in `data`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct Thumbnail {
    pub mime_type: String,
    pub data: String,
}

/// What the author says about a shared image, video or audio file. `width`/`height` are 0
/// when unknown, as is `duration_ms` (and always for images).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct MediaInfo {
    pub kind: MediaKind,
    pub width: u32,
    pub height: u32,
    pub duration_ms: u64,
    pub thumb: Option<Thumbnail>,
    /// Voice messages only: `WAVEFORM_BARS` levels, 0–255.
    pub waveform: Option<Vec<u8>>,
}

impl MediaInfo {
    /// Whether every member may show and log an offer of `mime_type` carrying this. A rule
    /// all members apply alike: one refusing what another keeps would make their logs differ.
    pub fn is_valid(&self, mime_type: &str) -> bool {
        let family_ok = match (self.kind, media_family(mime_type)) {
            (MediaKind::Image, Some(MediaKind::Image)) => true,
            (MediaKind::Video, Some(MediaKind::Video)) => true,
            (MediaKind::Audio | MediaKind::Voice, Some(MediaKind::Audio)) => true,
            _ => false,
        };
        let thumb_ok = self.thumb.as_ref().is_none_or(|thumb| {
            THUMB_TYPES.contains(&thumb.mime_type.as_str())
                // Base64 of THUMB_MAX_BYTES, before decoding anything.
                && thumb.data.len() <= THUMB_MAX_BYTES.div_ceil(3) * 4
                && STANDARD.decode(&thumb.data).is_ok_and(|bytes| !bytes.is_empty() && bytes.len() <= THUMB_MAX_BYTES)
        });
        let waveform_ok = match &self.waveform {
            None => true,
            Some(bars) => self.kind == MediaKind::Voice && bars.len() == WAVEFORM_BARS,
        };
        family_ok
            && thumb_ok
            && waveform_ok
            && self.width <= MAX_MEDIA_DIMENSION
            && self.height <= MAX_MEDIA_DIMENSION
            && self.duration_ms <= MAX_MEDIA_DURATION_MS
    }
}

/// The type part of a MIME type, lowercased, without parameters (`audio/webm;codecs=opus` →
/// `audio/webm`).
fn essence(mime_type: &str) -> String {
    mime_type.split(';').next().unwrap_or_default().trim().to_ascii_lowercase()
}

/// The type a preview is built with, when `mime_type` is one dchat shows inline; `None`
/// otherwise (SVG included). The browser still has to decode it: whatever fails shows as a
/// plain file.
pub fn preview_type(mime_type: &str) -> Option<&'static str> {
    Some(match essence(mime_type).as_str() {
        "image/png" => "image/png",
        "image/jpeg" | "image/jpg" | "image/pjpeg" => "image/jpeg",
        "image/gif" => "image/gif",
        "image/webp" => "image/webp",
        "image/avif" => "image/avif",
        "image/bmp" | "image/x-ms-bmp" => "image/bmp",
        "video/mp4" => "video/mp4",
        "video/webm" => "video/webm",
        "video/ogg" => "video/ogg",
        "video/quicktime" => "video/quicktime",
        "audio/mpeg" | "audio/mp3" => "audio/mpeg",
        "audio/mp4" | "audio/x-m4a" | "audio/m4a" => "audio/mp4",
        "audio/aac" => "audio/aac",
        "audio/ogg" | "audio/opus" => "audio/ogg",
        "audio/webm" => "audio/webm",
        "audio/wav" | "audio/x-wav" | "audio/wave" => "audio/wav",
        "audio/flac" | "audio/x-flac" => "audio/flac",
        _ => return None,
    })
}

/// Image, video or audio (never `Voice`), for a type `preview_type` accepts.
pub fn media_family(mime_type: &str) -> Option<MediaKind> {
    let preview = preview_type(mime_type)?;
    Some(if preview.starts_with("image/") {
        MediaKind::Image
    } else if preview.starts_with("video/") {
        MediaKind::Video
    } else {
        MediaKind::Audio
    })
}

/// A type for a file the browser gave none, from its name's extension.
pub fn mime_from_name(name: &str) -> Option<&'static str> {
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" | "jfif" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "ogv" => "video/ogg",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        _ => return None,
    })
}

/// Level samples taken while recording (any count, any scale) → `WAVEFORM_BARS` bars, the
/// loudest at 255. Each bar is the loudest sample of its stretch.
pub fn waveform_from_levels(levels: &[f32]) -> Vec<u8> {
    let clean: Vec<f32> = levels.iter().map(|l| if l.is_finite() { l.abs() } else { 0.0 }).collect();
    if clean.is_empty() {
        return vec![0; WAVEFORM_BARS];
    }
    let bars: Vec<f32> = (0..WAVEFORM_BARS)
        .map(|bar| {
            let start = bar * clean.len() / WAVEFORM_BARS;
            let end = ((bar + 1) * clean.len() / WAVEFORM_BARS).max(start + 1).min(clean.len());
            clean[start.min(clean.len() - 1)..end].iter().copied().fold(0.0, f32::max)
        })
        .collect();
    let peak = bars.iter().copied().fold(0.0, f32::max);
    if peak <= f32::EPSILON {
        return vec![0; WAVEFORM_BARS];
    }
    bars.into_iter().map(|b| (b / peak * 255.0).round().clamp(0.0, 255.0) as u8).collect()
}

// ---- Photo location ---------------------------------------------------------------------

const GPS_IFD_TAG: u16 = 0x8825;

/// Where a JPEG keeps a location.
#[derive(Debug, Default, PartialEq, Eq)]
struct GpsSpots {
    /// Byte ranges of the EXIF GPS directory and the values it points to.
    exif: Vec<std::ops::Range<usize>>,
    /// XMP segments (whole, marker included) naming a GPS position.
    xmp: Vec<std::ops::Range<usize>>,
}

/// Whether the JPEG `bytes` (its first few hundred KB are enough) record where it was taken.
pub fn jpeg_has_gps(bytes: &[u8]) -> bool {
    scan_jpeg(bytes).is_some_and(|spots| !spots.exif.is_empty() || !spots.xmp.is_empty())
}

/// The JPEG `bytes` without its location: the EXIF GPS directory is zeroed in place (the
/// picture, its orientation and every offset stay as they were) and XMP segments naming a
/// position are dropped. `None` when `bytes` isn't a JPEG this can read.
pub fn strip_jpeg_gps(bytes: &[u8]) -> Option<Vec<u8>> {
    let spots = scan_jpeg(bytes)?;
    let mut out = bytes.to_vec();
    for range in &spots.exif {
        out[range.clone()].fill(0);
    }
    for range in spots.xmp.iter().rev() {
        out.drain(range.clone());
    }
    Some(out)
}

/// Walk the segments before the image data.
fn scan_jpeg(bytes: &[u8]) -> Option<GpsSpots> {
    if bytes.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut spots = GpsSpots::default();
    let mut at = 2;
    while at + 4 <= bytes.len() {
        if bytes[at] != 0xFF {
            return None;
        }
        let marker = bytes[at + 1];
        // Fill bytes between segments.
        if marker == 0xFF {
            at += 1;
            continue;
        }
        // Start of scan (image data) or end of image: no metadata after this.
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        // Markers without a length.
        if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            at += 2;
            continue;
        }
        let len = u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]) as usize;
        if len < 2 {
            return None;
        }
        let data_start = at + 4;
        let end = at + 2 + len;
        // A truncated read (a file's first bytes only) ends the scan.
        let data = &bytes[data_start..end.min(bytes.len())];
        if marker == 0xE1 {
            if let Some(tiff) = data.strip_prefix(b"Exif\0\0") {
                if let Some(ranges) = exif_gps_ranges(tiff) {
                    let base = data_start + 6;
                    spots.exif.extend(ranges.into_iter().map(|r| r.start + base..r.end + base));
                }
            } else if data.starts_with(b"http://ns.adobe.com/xap/1.0/") && end <= bytes.len() && xmp_has_gps(data) {
                spots.xmp.push(at..end);
            }
        }
        if end > bytes.len() {
            break;
        }
        at = end;
    }
    Some(spots)
}

fn xmp_has_gps(data: &[u8]) -> bool {
    [b"GPSLatitude".as_slice(), b"GPSLongitude"].iter().any(|needle| data.windows(needle.len()).any(|w| w == *needle))
}

/// Byte ranges (from the TIFF header) of the GPS directory and its values; `None` without one.
fn exif_gps_ranges(tiff: &[u8]) -> Option<Vec<std::ops::Range<usize>>> {
    let little = match tiff.get(..4)? {
        [b'I', b'I', 42, 0] => true,
        [b'M', b'M', 0, 42] => false,
        _ => return None,
    };
    let u16_at = |at: usize| -> Option<u16> {
        let b = tiff.get(at..at.checked_add(2)?)?;
        Some(if little { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let b = tiff.get(at..at.checked_add(4)?)?;
        let b = [b[0], b[1], b[2], b[3]];
        Some(if little { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
    };
    // Offsets come from the file: every sum is checked (usize is 32 bits in wasm).
    let entry_at = |dir: usize, i: usize| dir.checked_add(2)?.checked_add(i.checked_mul(12)?);
    let ifd0 = u32_at(4)? as usize;
    let count0 = u16_at(ifd0)? as usize;
    let gps = (0..count0).find_map(|i| {
        let entry = entry_at(ifd0, i)?;
        (u16_at(entry)? == GPS_IFD_TAG).then(|| u32_at(entry.checked_add(8)?)).flatten()
    })? as usize;
    let count = u16_at(gps)? as usize;
    // An empty directory (one this already cleaned) holds no location.
    if count == 0 {
        return None;
    }
    let entries_end = entry_at(gps, count)?;
    tiff.get(gps..entries_end)?;
    // The directory itself: count, entries and the next-directory offset (all zero after).
    let mut ranges = vec![gps..entries_end.saturating_add(4).min(tiff.len())];
    for i in 0..count {
        let entry = entry_at(gps, i)?;
        let unit: usize = match u16_at(entry + 2)? {
            1 | 2 | 6 | 7 => 1,
            3 | 8 => 2,
            4 | 9 | 11 => 4,
            5 | 10 | 12 => 8,
            _ => continue,
        };
        let Some(size) = unit.checked_mul(u32_at(entry + 4)? as usize) else {
            continue;
        };
        // Values of four bytes or fewer sit in the entry itself.
        if size > 4 {
            let at = u32_at(entry + 8)? as usize;
            if let Some(end) = at.checked_add(size).filter(|end| *end <= tiff.len()) {
                ranges.push(at..end);
            }
        }
    }
    Some(ranges)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thumb(bytes: usize) -> Thumbnail {
        Thumbnail { mime_type: "image/webp".into(), data: STANDARD.encode(vec![7u8; bytes]) }
    }

    fn image() -> MediaInfo {
        MediaInfo { kind: MediaKind::Image, width: 4000, height: 3000, duration_ms: 0, thumb: Some(thumb(2000)), waveform: None }
    }

    #[test]
    fn test_preview_types_are_an_allow_list() {
        assert_eq!(preview_type("image/png"), Some("image/png"));
        assert_eq!(preview_type("IMAGE/JPG"), Some("image/jpeg"));
        assert_eq!(preview_type("audio/webm;codecs=opus"), Some("audio/webm"));
        assert_eq!(preview_type(" video/mp4 ; codecs=\"avc1\""), Some("video/mp4"));
        for refused in ["image/svg+xml", "text/html", "application/pdf", "", "image/heic", "image/x-icon"] {
            assert_eq!(preview_type(refused), None, "{refused}");
        }
        assert_eq!(media_family("audio/x-m4a"), Some(MediaKind::Audio));
        assert_eq!(media_family("image/gif"), Some(MediaKind::Image));
        assert_eq!(media_family("video/quicktime"), Some(MediaKind::Video));
        assert_eq!(mime_from_name("IMG_0001.JPG"), Some("image/jpeg"));
        assert_eq!(mime_from_name("drawing.svg"), None);
        assert_eq!(mime_from_name("noext"), None);
    }

    #[test]
    fn test_media_info_limits() {
        assert!(image().is_valid("image/jpeg"));
        assert!(MediaInfo { thumb: None, ..image() }.is_valid("image/png"));
        assert!(MediaInfo { thumb: Some(thumb(THUMB_MAX_BYTES)), ..image() }.is_valid("image/png"));
        assert!(!MediaInfo { thumb: Some(thumb(THUMB_MAX_BYTES + 1)), ..image() }.is_valid("image/png"));
        assert!(!MediaInfo { thumb: Some(Thumbnail { mime_type: "image/svg+xml".into(), data: thumb(10).data }), ..image() }
            .is_valid("image/png"));
        assert!(!MediaInfo { thumb: Some(Thumbnail { mime_type: "image/webp".into(), data: "%%%".into() }), ..image() }
            .is_valid("image/png"));
        assert!(!MediaInfo { thumb: Some(Thumbnail { mime_type: "image/webp".into(), data: String::new() }), ..image() }
            .is_valid("image/png"));
        assert!(!MediaInfo { width: MAX_MEDIA_DIMENSION + 1, ..image() }.is_valid("image/png"));
        assert!(!MediaInfo { duration_ms: MAX_MEDIA_DURATION_MS + 1, ..image() }.is_valid("image/png"));
        // The kind must match the file's type: an "image" that is a web page is refused.
        assert!(!image().is_valid("text/html"));
        assert!(!image().is_valid("image/svg+xml"));
        assert!(!image().is_valid("video/mp4"));
        let voice = MediaInfo {
            kind: MediaKind::Voice,
            width: 0,
            height: 0,
            duration_ms: 12_000,
            thumb: None,
            waveform: Some(vec![9; WAVEFORM_BARS]),
        };
        assert!(voice.is_valid("audio/webm;codecs=opus"));
        assert!(voice.is_valid("audio/mp4"));
        assert!(!voice.is_valid("video/webm"));
        assert!(!MediaInfo { waveform: Some(vec![9; WAVEFORM_BARS + 1]), ..voice.clone() }.is_valid("audio/mp4"));
        assert!(!MediaInfo { kind: MediaKind::Audio, ..voice.clone() }.is_valid("audio/mp4"), "only voice messages carry a waveform");
        assert!(!MediaInfo { kind: MediaKind::Video, waveform: None, ..voice }.is_valid("audio/mp4"));
    }

    #[test]
    fn test_waveform_from_levels() {
        assert_eq!(waveform_from_levels(&[]), vec![0; WAVEFORM_BARS]);
        assert_eq!(waveform_from_levels(&[0.0; 500]), vec![0; WAVEFORM_BARS]);
        // Fewer samples than bars: each stretches over several bars.
        let few = waveform_from_levels(&[0.1, 0.4, f32::NAN, 0.2]);
        assert_eq!(few.len(), WAVEFORM_BARS);
        assert_eq!(*few.iter().max().unwrap(), 255);
        assert_eq!(few[0], 64);
        assert_eq!(few[WAVEFORM_BARS / 2], 0, "not-a-number counts as silence");
        // Many samples: the loudest of each stretch, scaled to the loudest overall.
        let ramp: Vec<f32> = (0..6400).map(|i| i as f32).collect();
        let bars = waveform_from_levels(&ramp);
        assert_eq!(bars.len(), WAVEFORM_BARS);
        assert!(bars.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(bars[WAVEFORM_BARS - 1], 255);
    }

    /// A little-endian EXIF block: Orientation = 6 and a GPS directory with a latitude
    /// (rational ×3, out of line) and its reference letter (inline).
    fn exif_with_gps() -> Vec<u8> {
        let mut t = Vec::new();
        t.extend_from_slice(b"II\x2a\x00");
        t.extend_from_slice(&8u32.to_le_bytes()); // IFD0 at 8
        // IFD0: 2 entries.
        t.extend_from_slice(&2u16.to_le_bytes());
        t.extend_from_slice(&0x0112u16.to_le_bytes()); // Orientation
        t.extend_from_slice(&3u16.to_le_bytes()); // SHORT
        t.extend_from_slice(&1u32.to_le_bytes());
        t.extend_from_slice(&[6, 0, 0, 0]);
        t.extend_from_slice(&GPS_IFD_TAG.to_le_bytes());
        t.extend_from_slice(&4u16.to_le_bytes()); // LONG
        t.extend_from_slice(&1u32.to_le_bytes());
        t.extend_from_slice(&38u32.to_le_bytes()); // GPS IFD at 38
        t.extend_from_slice(&0u32.to_le_bytes()); // no next IFD
        assert_eq!(t.len(), 38);
        // GPS IFD: 2 entries.
        t.extend_from_slice(&2u16.to_le_bytes());
        t.extend_from_slice(&1u16.to_le_bytes()); // GPSLatitudeRef
        t.extend_from_slice(&2u16.to_le_bytes()); // ASCII
        t.extend_from_slice(&2u32.to_le_bytes());
        t.extend_from_slice(b"N\0\0\0");
        t.extend_from_slice(&2u16.to_le_bytes()); // GPSLatitude
        t.extend_from_slice(&5u16.to_le_bytes()); // RATIONAL
        t.extend_from_slice(&3u32.to_le_bytes());
        t.extend_from_slice(&68u32.to_le_bytes()); // values at 68
        t.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(t.len(), 68);
        for v in [48u32, 1, 51, 1, 2_400, 100] {
            t.extend_from_slice(&v.to_le_bytes());
        }
        t
    }

    fn jpeg(app_segments: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let mut j = vec![0xFF, 0xD8];
        for (marker, data) in app_segments {
            j.extend_from_slice(&[0xFF, *marker]);
            j.extend_from_slice(&((data.len() + 2) as u16).to_be_bytes());
            j.extend_from_slice(data);
        }
        // Start of scan and some "image data".
        j.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x04, 1, 2, 0xAB, 0xCD, 0xFF, 0xD9]);
        j
    }

    fn exif_segment() -> (u8, Vec<u8>) {
        let mut data = b"Exif\0\0".to_vec();
        data.extend(exif_with_gps());
        (0xE1, data)
    }

    fn find(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn test_strip_jpeg_gps_zeroes_the_location_and_keeps_the_rest() {
        let photo = jpeg(&[(0xE0, b"JFIF\0\x01\x01".to_vec()), exif_segment()]);
        assert!(jpeg_has_gps(&photo));
        let clean = strip_jpeg_gps(&photo).unwrap();
        assert_eq!(clean.len(), photo.len(), "zeroed in place");
        assert!(!jpeg_has_gps(&clean));
        assert!(!find(&clean, &2_400u32.to_le_bytes()), "the latitude's values are gone");
        assert!(!find(&clean, b"N\0\0\0"));
        // Orientation and the image data are untouched.
        assert!(find(&clean, &[0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0]));
        assert!(clean.ends_with(&[0xFF, 0xDA, 0x00, 0x04, 1, 2, 0xAB, 0xCD, 0xFF, 0xD9]));
        assert_eq!(strip_jpeg_gps(&clean).unwrap(), clean, "stripping twice changes nothing");
        // Only the first bytes of a large photo are read for the warning: the metadata.
        assert!(jpeg_has_gps(&photo[..photo.len() - 8]));
    }

    #[test]
    fn test_strip_jpeg_gps_drops_xmp_positions() {
        let mut xmp = b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta>".to_vec();
        xmp.extend_from_slice(b"<exif:GPSLatitude>48,51.24N</exif:GPSLatitude></x:xmpmeta>");
        let photo = jpeg(&[(0xE1, xmp.clone())]);
        assert!(jpeg_has_gps(&photo));
        let clean = strip_jpeg_gps(&photo).unwrap();
        assert!(!jpeg_has_gps(&clean));
        assert_eq!(clean.len(), photo.len() - xmp.len() - 4);
        // XMP without a position stays.
        let plain = jpeg(&[(0xE1, b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta/>".to_vec())]);
        assert!(!jpeg_has_gps(&plain));
        assert_eq!(strip_jpeg_gps(&plain).unwrap(), plain);
    }

    #[test]
    fn test_jpeg_scan_never_panics_on_hostile_input() {
        assert_eq!(strip_jpeg_gps(b"\x89PNG\r\n"), None);
        assert_eq!(strip_jpeg_gps(&[]), None);
        assert!(!jpeg_has_gps(&[0xFF, 0xD8, 0xFF]));
        let photo = jpeg(&[exif_segment()]);
        // Every truncation, and every single corrupted byte, of a photo with a location.
        for cut in 0..photo.len() {
            let _ = strip_jpeg_gps(&photo[..cut]);
        }
        for at in 0..photo.len() {
            for value in [0x00, 0xFF, 0x7F] {
                let mut bad = photo.clone();
                bad[at] = value;
                let _ = strip_jpeg_gps(&bad);
            }
        }
        // Offsets pointing far outside the block.
        let mut tiff = exif_with_gps();
        tiff[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        let mut data = b"Exif\0\0".to_vec();
        data.extend(tiff);
        assert!(!jpeg_has_gps(&jpeg(&[(0xE1, data)])));
    }
}

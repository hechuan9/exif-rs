// RAW container framing. The embedded EXIF remains in its original namespace.
// Layout references: ExifTool FujiFilm::ProcessRAF, MinoltaRaw::ProcessMRW,
// and PanasonicRaw tag 0x2e (JpgFromRaw).
use crate::{jpeg, Error};
use std::io::{BufRead, Read, Seek, SeekFrom};

pub(crate) fn is_raw(b: &[u8]) -> bool {
    b.starts_with(b"FUJIFILMCCD-RAW ")
        || b.starts_with(b"\0MRM")
        || b.starts_with(b"\0MRI")
        || b.starts_with(b"II\x55\0")
}
fn number(b: &[u8], little: bool) -> u32 {
    let a = [b[0], b[1], b[2], b[3]];
    if little {
        u32::from_le_bytes(a)
    } else {
        u32::from_be_bytes(a)
    }
}
fn at<R: Read + Seek>(r: &mut R, offset: u64, size: u64, end: u64) -> Result<Vec<u8>, Error> {
    if offset > end || size > end - offset {
        return Err(Error::InvalidFormat("RAW metadata outside container"));
    }
    r.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::new();
    r.take(size).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != size {
        return Err(Error::InvalidFormat("Truncated RAW metadata"));
    }
    Ok(bytes)
}
fn jpeg_at<R: BufRead + Seek>(
    r: &mut R,
    offset: u64,
    size: u64,
    end: u64,
) -> Result<Vec<u8>, Error> {
    if offset > end || size > end - offset {
        return Err(Error::InvalidFormat("RAW JPEG outside container"));
    }
    r.seek(SeekFrom::Start(offset))?;
    jpeg::get_exif_attr(&mut r.take(size))
}
pub(crate) fn get_exif<R: BufRead + Seek>(r: &mut R) -> Result<Vec<u8>, Error> {
    let end = r.seek(SeekFrom::End(0))?;
    let h = at(r, 0, 8, end)?;
    if h.starts_with(b"FUJIFILM") {
        let h = at(r, 0, 112, end)?;
        return jpeg_at(
            r,
            number(&h[84..88], false) as u64,
            number(&h[88..92], false) as u64,
            end,
        );
    }
    if h.starts_with(b"\0MR") {
        let little = h[3] == b'I';
        let limit = 8 + number(&h[4..8], little) as u64;
        if limit > end {
            return Err(Error::InvalidFormat("MRW metadata outside container"));
        }
        let mut pos = 8;
        while pos < limit {
            let h = at(r, pos, 8, limit)?;
            let len = number(&h[4..8], little) as u64;
            if h[..4] == *b"\0TTW" {
                return at(r, pos + 8, len, limit);
            }
            if len > limit - pos - 8 {
                return Err(Error::InvalidFormat("MRW block outside container"));
            }
            pos += 8 + len;
        }
        return Err(Error::NotFound("MRW EXIF"));
    }
    if h.starts_with(b"II\x55\0") {
        let pos = number(&h[4..8], true) as u64;
        let n = at(r, pos, 2, end)?;
        let count = u16::from_le_bytes([n[0], n[1]]) as u64;
        let entries = at(r, pos + 2, count * 12, end)?;
        for e in entries.chunks_exact(12) {
            if e[..2] != [0x2e, 0] {
                continue;
            }
            if e[2..4] != [7, 0] {
                return Err(Error::InvalidFormat("Invalid RW2 JPEG type"));
            }
            return jpeg_at(
                r,
                number(&e[8..12], true) as u64,
                number(&e[4..8], true) as u64,
                end,
            );
        }
        return Err(Error::NotFound("RW2 embedded EXIF"));
    }
    Err(Error::InvalidFormat("Unknown RAW container"))
}

/// Metadata stored outside standard EXIF in legacy RAW containers.
/// Names describe the container values; they are not synthesized EXIF tags.
#[derive(Debug, Default)]
pub struct LegacyRawMetadata {
    /// Manufacturer string from the container.
    pub make: Option<String>,
    /// Camera model string from the container.
    pub model: Option<String>,
    /// Kyocera's local timestamp, with no assumed timezone.
    pub capture_time: Option<String>,
    /// Canon CIFF timestamp as stored. ExifTool formats this with `gmtime`
    /// without a timezone suffix; do not infer a UTC instant from it.
    pub capture_timestamp: Option<u32>,
    /// CIFF timezone seconds, present only when TimeZoneInfo marks it valid.
    pub timezone_offset_seconds: Option<i32>,
    /// Canon CIFF ImageInfo dimensions.
    pub dimensions: Option<(u32, u32)>,
}
fn text(b: &[u8]) -> Result<String, Error> {
    let end = b.iter().position(|x| *x == 0).unwrap_or(b.len());
    String::from_utf8(b[..end].to_vec()).map_err(|_| Error::InvalidFormat("Invalid RAW text"))
}
/// Read CIFF (CRW) or Kyocera's native header without inventing EXIF fields.
pub fn read_legacy_raw_metadata<R: BufRead + Seek>(r: &mut R) -> Result<LegacyRawMetadata, Error> {
    let end = r.seek(SeekFrom::End(0))?;
    let h = at(r, 0, 32, end)?;
    let mut result = LegacyRawMetadata::default();
    if &h[25..32] == b"ARECOYK" {
        let h = at(r, 0, 156, end)?;
        let reverse = |start: usize, len: usize| -> Result<String, Error> {
            {
                let b = &h[start..start + len];
                let n = b.iter().position(|x| *x == 0).unwrap_or(b.len());
                text(&b[..n].iter().rev().copied().collect::<Vec<_>>())
            }
        };
        result.make = Some(reverse(25, 7)?);
        result.model = Some(reverse(12, 12)?);
        result.capture_time = Some(reverse(33, 20)?);
    } else if (&h[..2] == b"II" || &h[..2] == b"MM")
        && (&h[6..14] == b"HEAPCCDR" || &h[6..14] == b"HEAPJPGM")
    {
        let little = &h[..2] == b"II";
        let start = number(&h[2..6], little) as u64;
        if start > end {
            return Err(Error::InvalidFormat("CIFF header outside container"));
        }
        ciff_directory(
            r,
            start,
            end - start,
            little,
            &mut result,
            &mut std::collections::HashSet::new(),
        )?;
    } else {
        return Err(Error::InvalidFormat("Unknown legacy RAW container"));
    }
    Ok(result)
}
fn ciff_directory<R: Read + Seek>(
    r: &mut R,
    start: u64,
    size: u64,
    little: bool,
    out: &mut LegacyRawMetadata,
    seen: &mut std::collections::HashSet<u64>,
) -> Result<(), Error> {
    if size < 4 {
        return Err(Error::InvalidFormat("Truncated CIFF directory"));
    }
    let end = start
        .checked_add(size)
        .ok_or(Error::InvalidFormat("CIFF offset overflow"))?;
    let tail = at(r, end - 4, 4, end)?;
    let pos = start + number(&tail, little) as u64;
    if !seen.insert(pos) {
        return Err(Error::InvalidFormat("Repeated CIFF directory"));
    }
    if seen.len() > 256 {
        return Err(Error::InvalidFormat("Too many CIFF directories"));
    }
    let h = at(r, pos, 2, end - 4)?;
    let count = if little {
        u16::from_le_bytes([h[0], h[1]])
    } else {
        u16::from_be_bytes([h[0], h[1]])
    };
    let entries = at(r, pos + 2, count as u64 * 10, end - 4)?;
    for e in entries.chunks_exact(10) {
        let tag = if little {
            u16::from_le_bytes([e[0], e[1]])
        } else {
            u16::from_be_bytes([e[0], e[1]])
        };
        if tag & 0x8000 != 0 {
            return Err(Error::InvalidFormat("Invalid CIFF tag"));
        }
        let len = number(&e[2..6], little) as u64;
        let offset = start + number(&e[6..10], little) as u64;
        if tag & 0x4000 == 0 && (tag & 0x3800 == 0x2800 || tag & 0x3800 == 0x3000) {
            if offset > end || len > end - offset {
                return Err(Error::InvalidFormat("CIFF child outside directory"));
            }
            ciff_directory(r, offset, len, little, out, seen)?;
            continue;
        }
        let id = tag & 0x3fff;
        if ![0x080a, 0x180e, 0x1810].contains(&id) {
            continue;
        }
        let b = if tag & 0x4000 != 0 {
            e[2..10].to_vec()
        } else {
            at(r, offset, len, end)?
        };
        match id {
            0x080a => {
                if b.len() < 6 {
                    return Err(Error::InvalidFormat("Truncated CIFF camera"));
                }
                out.make = Some(text(&b[..6])?);
                out.model = Some(text(&b[6..])?);
            }
            0x180e => {
                if b.len() < 4 {
                    return Err(Error::InvalidFormat("Truncated CIFF timestamp"));
                }
                out.capture_timestamp = Some(number(&b, little));
                if b.len() >= 12 && number(&b[8..12], little) & 0x80000000 != 0 {
                    out.timezone_offset_seconds = Some(number(&b[4..8], little) as i32);
                }
            }
            0x1810 => {
                if b.len() < 8 {
                    return Err(Error::InvalidFormat("Truncated CIFF dimensions"));
                }
                out.dimensions = Some((number(&b[..4], little), number(&b[4..8], little)));
            }
            _ => unreachable!(),
        }
    }
    Ok(())
}

/// Canon CR3's independently based TIFF metadata sections.
/// Each section retains its original bytes and tag namespace.
#[derive(Debug)]
pub struct Cr3Metadata {
    /// Standard TIFF/EXIF/GPS fields from CMT1, CMT2 and CMT4.
    pub fields: Vec<crate::Field>,
    /// CMT3 Canon MakerNotes TIFF bytes; not standard TIFF tags.
    pub maker_notes: Option<Vec<u8>>,
}
impl Cr3Metadata {
    /// Standard EXIF image identity, otherwise the Canon CMT3 identity.
    pub fn image_unique_id(&self) -> Result<Option<String>, Error> {
        for f in &self.fields {
            if f.tag != crate::Tag::ImageUniqueID {
                continue;
            }
            if let crate::Value::Ascii(values) = &f.value {
                if let Some(value) = values.first() {
                    let id = text(value)?;
                    if !id.trim().is_empty() {
                        return Ok(Some(id.trim().to_owned()));
                    }
                }
            }
        }
        match &self.maker_notes {
            Some(bytes) => crate::makernotes::canon_tiff_image_unique_id(bytes),
            None => Ok(None),
        }
    }
}

/// Read Canon CR3 metadata through its declared Canon UUID box.
/// This does not parse the raw pixel stream or treat CR3 as HEIF.
pub fn read_cr3_metadata<R: BufRead + Seek>(r: &mut R) -> Result<Cr3Metadata, Error> {
    let end = r.seek(SeekFrom::End(0))?;
    let first = at(r, 0, 16, end)?;
    if &first[4..8] != b"ftyp" || &first[8..12] != b"crx " {
        return Err(Error::InvalidFormat("Not a Canon CR3 container"));
    }
    let mut out = Cr3Metadata {
        fields: Vec::new(),
        maker_notes: None,
    };
    let mut seen = std::collections::HashSet::new();
    cr3_boxes(r, 0, end, 0, &mut out, &mut seen)?;
    if seen.is_empty() {
        return Err(Error::NotFound("CR3 metadata"));
    }
    Ok(out)
}
fn cr3_boxes<R: BufRead + Seek>(
    r: &mut R,
    mut pos: u64,
    end: u64,
    level: u8,
    out: &mut Cr3Metadata,
    seen: &mut std::collections::HashSet<[u8; 4]>,
) -> Result<(), Error> {
    const CANON_UUID: [u8; 16] = [
        0x85, 0xc0, 0xb6, 0x87, 0x82, 0x0f, 0x11, 0xe0, 0x81, 0x11, 0xf4, 0xce, 0x46, 0x2b, 0x6a,
        0x48,
    ];
    while pos < end {
        let h = at(r, pos, 8, end)?;
        let typ = [h[4], h[5], h[6], h[7]];
        let mut len = number(&h[..4], false) as u64;
        let mut header = 8;
        if len == 1 {
            let b = at(r, pos + 8, 8, end)?;
            len = u64::from_be_bytes(b.try_into().unwrap());
            header = 16;
        }
        if len == 0 {
            len = end - pos;
        }
        if len < header || len > end - pos {
            return Err(Error::InvalidFormat("Invalid CR3 box size"));
        }
        let begin = pos + header;
        let stop = pos + len;
        if level == 0 && &typ == b"moov" {
            cr3_boxes(r, begin, stop, 1, out, seen)?;
        }
        if level == 1 && &typ == b"uuid" {
            let id = at(r, begin, 16, stop)?;
            if id == CANON_UUID {
                cr3_boxes(r, begin + 16, stop, 2, out, seen)?;
            }
        }
        if level == 2
            && [&b"CMT1"[..], &b"CMT2"[..], &b"CMT3"[..], &b"CMT4"[..]].contains(&&typ[..])
        {
            if !seen.insert(typ) {
                return Err(Error::InvalidFormat("Duplicate CR3 metadata section"));
            }
            let bytes = at(r, begin, stop - begin, stop)?;
            if &typ == b"CMT3" {
                out.maker_notes = Some(bytes);
            } else {
                if &typ == b"CMT1" {
                    let exif = crate::Reader::new().read_raw(bytes)?;
                    out.fields.extend(exif.fields().cloned());
                } else {
                    if bytes.len() < 8 || !crate::tiff::is_tiff(&bytes) {
                        return Err(Error::InvalidFormat("Invalid CR3 TIFF header"));
                    }
                    let little = bytes[0] == b'I';
                    let offset = number(&bytes[4..8], little);
                    let context = if &typ == b"CMT2" {
                        crate::Context::Exif
                    } else {
                        crate::Context::Gps
                    };
                    out.fields.extend(crate::tiff::parse_ifd_context(
                        &bytes, little, offset, context,
                    )?);
                }
            }
        }
        pos = stop;
    }
    Ok(())
}

/// Sensor and active-image dimensions, separate from preview EXIF dimensions.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RawDimensions {
    /// Full sensor size, including borders.
    pub full: Option<(u32, u32)>,
    /// Active image extent after the container's recorded crop.
    pub cropped: Option<(u32, u32)>,
}
/// Read RAF proprietary directory or Panasonic RW2 sensor bounds.
/// Returns `NotFound` when the container has no recorded dimensions.
pub fn read_raw_dimensions<R: BufRead + Seek>(r: &mut R) -> Result<RawDimensions, Error> {
    let end = r.seek(SeekFrom::End(0))?;
    let h = at(r, 0, 8, end)?;
    let mut result = RawDimensions::default();
    if h.starts_with(b"FUJIFILM") {
        let h = at(r, 0, 112, end)?;
        let start = number(&h[92..96], false) as u64;
        let len = number(&h[96..100], false) as u64;
        if len == 0 {
            return Err(Error::NotFound("RAF dimensions"));
        }
        let b = at(r, start, len, end)?;
        if b.len() < 4 {
            return Err(Error::InvalidFormat("Truncated RAF directory"));
        }
        let count = number(&b, false);
        let mut pos = 4;
        for _ in 0..count {
            if b.len() - pos < 4 {
                return Err(Error::InvalidFormat("Truncated RAF entry"));
            }
            let tag = u16::from_be_bytes([b[pos], b[pos + 1]]);
            let len = u16::from_be_bytes([b[pos + 2], b[pos + 3]]) as usize;
            pos += 4;
            if len > b.len() - pos {
                return Err(Error::InvalidFormat("RAF value outside directory"));
            }
            if tag == 0x100 || tag == 0x111 {
                if len != 4 {
                    return Err(Error::InvalidFormat("Invalid RAF dimensions"));
                }
                let dims = (
                    u16::from_be_bytes([b[pos + 2], b[pos + 3]]) as u32,
                    u16::from_be_bytes([b[pos], b[pos + 1]]) as u32,
                );
                if tag == 0x100 {
                    result.full = Some(dims);
                } else {
                    result.cropped = Some(dims);
                }
            }
            pos += len;
        }
    } else if h.starts_with(b"II\x55\0") {
        let pos = number(&h[4..], true) as u64;
        let h = at(r, pos, 2, end)?;
        let count = u16::from_le_bytes([h[0], h[1]]) as u64;
        let entries = at(r, pos + 2, count * 12, end)?;
        let mut values = [None; 6];
        for e in entries.chunks_exact(12) {
            let tag = u16::from_le_bytes([e[0], e[1]]);
            if !(2..=7).contains(&tag) {
                continue;
            }
            if e[2..4] != [3, 0] || number(&e[4..8], true) != 1 {
                return Err(Error::InvalidFormat("Invalid RW2 sensor bounds"));
            }
            values[tag as usize - 2] = Some(u16::from_le_bytes([e[8], e[9]]) as u32);
        }
        result.full = values[0].zip(values[1]);
        if let (Some(t), Some(l), Some(b), Some(r)) = (values[2], values[3], values[4], values[5]) {
            if b < t || r < l {
                return Err(Error::InvalidFormat("Reversed RW2 sensor bounds"));
            }
            result.cropped = Some((r - l, b - t));
        }
    } else {
        return Err(Error::InvalidFormat("Unknown RAW dimensions container"));
    }
    if result.full.is_none() && result.cropped.is_none() {
        return Err(Error::NotFound("RAW dimensions"));
    }
    Ok(result)
}

/// Camera model and stored image extent in an ARRIRAW (ARI) header.
#[derive(Debug, PartialEq, Eq)]
pub struct AriMetadata {
    /// Camera model recorded in the header, without inferred normalization.
    pub model: String,
    /// Stored image width and height, not a preview extent.
    pub dimensions: (u32, u32),
}
/// Read the documented-by-implementation ARI header fields, not EXIF.
/// Format facts are corroborated by rawloader's ARI decoder; no pixel decode
/// or capture date is inferred from filenames or filesystem timestamps.
pub fn read_ari_metadata<R: BufRead + Seek>(r: &mut R) -> Result<AriMetadata, Error> {
    let end = r.seek(SeekFrom::End(0))?;
    let h = at(r, 0, 28, end)?;
    if &h[..8] != b"ARRI\x12\x34\x56\x78" {
        return Err(Error::InvalidFormat("Not an ARI container"));
    }
    let image_start = number(&h[8..12], true) as u64;
    if image_start <= 668 || image_start > end {
        return Err(Error::InvalidFormat("Invalid ARI header extent"));
    }
    let dimensions = (number(&h[20..24], true), number(&h[24..28], true));
    if dimensions.0 == 0 || dimensions.1 == 0 {
        return Err(Error::InvalidFormat("Invalid ARI image extent"));
    }
    let model_bytes = at(r, 668, image_start - 668, end)?;
    if !model_bytes.contains(&0) {
        return Err(Error::InvalidFormat("Unterminated ARI camera model"));
    }
    let model = text(&model_bytes)?;
    Ok(AriMetadata { model, dimensions })
}

//! Narrow image identity extraction; vendor fields remain separate from EXIF fields.
use crate::{Error, Exif, In};

fn invalid() -> Error {
    Error::InvalidFormat("Invalid image identity MakerNotes")
}

fn bytes(data: &[u8], offset: usize, count: usize) -> Result<&[u8], Error> {
    let end = offset.checked_add(count).ok_or_else(invalid)?;
    data.get(offset..end).ok_or_else(invalid)
}
fn u16_at(data: &[u8], offset: usize, le: bool) -> Result<u16, Error> {
    let v = bytes(data, offset, 2)?;
    Ok(if le {
        u16::from_le_bytes([v[0], v[1]])
    } else {
        u16::from_be_bytes([v[0], v[1]])
    })
}
fn u32_at(data: &[u8], offset: usize, le: bool) -> Result<u32, Error> {
    let v = bytes(data, offset, 4)?;
    Ok(if le {
        u32::from_le_bytes([v[0], v[1], v[2], v[3]])
    } else {
        u32::from_be_bytes([v[0], v[1], v[2], v[3]])
    })
}
fn text(data: &[u8]) -> Result<Option<String>, Error> {
    let end = data.iter().position(|v| *v == 0).unwrap_or(data.len());
    let value = std::str::from_utf8(&data[..end])
        .map_err(|_| invalid())?
        .trim();
    Ok(if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    })
}

pub(crate) fn image_unique_id(exif: &Exif, ifd: In) -> Result<Option<String>, Error> {
    use crate::{Tag, Value};
    if let Some(field) = exif.get_field(Tag::ImageUniqueID, ifd) {
        if let Value::Ascii(values) = &field.value {
            if let Some(value) = values.first() {
                if let Some(value) = text(value)? {
                    return Ok(Some(value));
                }
            }
        }
    }
    let (maker, offset) = match exif.get_field(Tag::MakerNote, ifd).map(|f| &f.value) {
        Some(Value::Undefined(data, offset)) => (data.as_slice(), *offset as usize),
        _ => return Ok(None),
    };
    let apple = maker.starts_with(b"Apple iOS\0");
    let canon = match exif.get_field(Tag::Make, ifd).map(|f| &f.value) {
        Some(Value::Ascii(values)) => values
            .first()
            .map(|v| v.starts_with(b"Canon"))
            .unwrap_or(false),
        _ => false,
    };
    if !apple && !canon {
        return Ok(None);
    }
    // Apple offsets are relative to its signature; Canon uses the TIFF origin.
    let (data, start, le, tag) = if apple {
        let le = match bytes(maker, 12, 2)? {
            b"II" => true,
            b"MM" => false,
            _ => return Err(invalid()),
        };
        (maker, 14, le, 0x15)
    } else {
        (exif.buf(), offset, exif.little_endian(), 0x28)
    };
    directory_id(data, start, le, apple, tag)
}

pub(crate) fn canon_tiff_image_unique_id(data: &[u8]) -> Result<Option<String>, Error> {
    let le = match bytes(data, 0, 4)? { b"II\x2a\0" => true, b"MM\0\x2a" => false, _ => return Err(invalid()) };
    let start = u32_at(data, 4, le)? as usize;
    directory_id(data, start, le, false, 0x28)
}

fn directory_id(data: &[u8], start: usize, le: bool, apple: bool, tag: u16) -> Result<Option<String>, Error> {
    let count = u16_at(data, start, le)? as usize;
    let begin = start.checked_add(2).ok_or_else(invalid)?;
    bytes(
        data,
        begin,
        count
            .checked_mul(12)
            .and_then(|n| n.checked_add(4))
            .ok_or_else(invalid)?,
    )?;
    for n in 0..count {
        let entry = begin + n * 12;
        if u16_at(data, entry, le)? != tag {
            continue;
        }
        let typ = u16_at(data, entry + 2, le)?;
        let count = u32_at(data, entry + 4, le)? as usize;
        if (apple && typ != 2) || (!apple && (typ != 7 && typ != 1)) {
            return Err(invalid());
        }
        if !apple && count != 16 {
            return Err(invalid());
        }
        let value_offset = if count <= 4 {
            entry + 8
        } else {
            u32_at(data, entry + 8, le)? as usize
        };
        let value = bytes(data, value_offset, count)?;
        if apple {
            return text(value);
        }
        if value.iter().all(|v| *v == 0) {
            return Ok(None);
        }
        use std::fmt::Write;
        let mut result = String::with_capacity(value.len() * 2);
        for byte in value {
            write!(&mut result, "{:02x}", byte).unwrap();
        }
        return Ok(Some(result));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Reader;

    fn push16(out: &mut Vec<u8>, n: u16, le: bool) {
        out.extend_from_slice(&if le { n.to_le_bytes() } else { n.to_be_bytes() });
    }
    fn push32(out: &mut Vec<u8>, n: u32, le: bool) {
        out.extend_from_slice(&if le { n.to_le_bytes() } else { n.to_be_bytes() });
    }
    fn entry(out: &mut Vec<u8>, tag: u16, typ: u16, count: u32, value: u32, le: bool) {
        push16(out, tag, le);
        push16(out, typ, le);
        push32(out, count, le);
        push32(out, value, le);
    }
    fn fixture(apple: bool, le: bool, standard: bool, zero: bool) -> Vec<u8> {
        let make: &[u8] = if apple { b"Apple\0" } else { b"Canon\0" };
        let maker_offset = 44 + if standard { 30 } else { 18 };
        let mut maker = Vec::new();
        if apple {
            maker.extend_from_slice(b"Apple iOS\0\0\x01");
            maker.extend_from_slice(if le { b"II" } else { b"MM" });
        }
        let payload: Vec<u8> = if apple {
            b"11223344-5566-7788-99aa-bbccddeeff00\0".to_vec()
        } else if zero {
            vec![0; 16]
        } else {
            (0..16).collect()
        };
        let value_offset = maker.len() + 18;
        push16(&mut maker, 1, le);
        entry(
            &mut maker,
            if apple { 0x15 } else { 0x28 },
            if apple { 2 } else { 7 },
            payload.len() as u32,
            if apple {
                value_offset as u32
            } else {
                maker_offset + value_offset as u32
            },
            le,
        );
        push32(&mut maker, 0, le);
        maker.extend_from_slice(&payload);
        let mut data = if le { b"II".to_vec() } else { b"MM".to_vec() };
        push16(&mut data, 42, le);
        push32(&mut data, 8, le);
        push16(&mut data, 2, le);
        entry(&mut data, 0x10f, 2, 6, 38, le);
        entry(&mut data, 0x8769, 4, 1, 44, le);
        push32(&mut data, 0, le);
        data.extend_from_slice(make);
        push16(&mut data, if standard { 2 } else { 1 }, le);
        entry(&mut data, 0x927c, 7, maker.len() as u32, maker_offset, le);
        if standard {
            entry(
                &mut data,
                0xa420,
                2,
                33,
                maker_offset + maker.len() as u32,
                le,
            );
        }
        push32(&mut data, 0, le);
        data.extend_from_slice(&maker);
        if standard {
            data.extend_from_slice(b"ffeeddccbbaa99887766554433221100\0");
        }
        data
    }
    #[test]
    fn canon_and_apple_id_both_byte_orders() {
        for apple in [false, true] {
            for le in [false, true] {
                let exif = Reader::new()
                    .read_raw(fixture(apple, le, false, false))
                    .unwrap();
                let expected = if apple {
                    "11223344-5566-7788-99aa-bbccddeeff00"
                } else {
                    "000102030405060708090a0b0c0d0e0f"
                };
                assert_eq!(
                    image_unique_id(&exif, In::PRIMARY).unwrap().as_deref(),
                    Some(expected)
                );
                assert!(exif
                    .get_field(crate::Tag::ImageUniqueID, In::PRIMARY)
                    .is_none());
            }
        }
    }
    #[test]
    fn standard_exif_wins_and_zero_canon_is_absent() {
        let e = Reader::new()
            .read_raw(fixture(false, true, true, false))
            .unwrap();
        assert_eq!(
            image_unique_id(&e, In::PRIMARY).unwrap().as_deref(),
            Some("ffeeddccbbaa99887766554433221100")
        );
        let e = Reader::new()
            .read_raw(fixture(false, true, false, true))
            .unwrap();
        assert!(image_unique_id(&e, In::PRIMARY).unwrap().is_none());
    }
    #[test]
    fn malformed_known_maker_notes_fail_without_following_offsets() {
        for apple in [false, true] {
            let original = fixture(apple, true, false, false);
            let start = 62 + if apple { 14 } else { 0 };
            for (relative, value) in [(4, 0xffff_u32), (6, 0xffff_ffff), (10, 0xffff_ffff)] {
                let mut data = original.clone();
                data[start + relative..start + relative + 4].copy_from_slice(&value.to_le_bytes());
                let e = Reader::new().read_raw(data).unwrap();
                assert!(e.image_unique_id(In::PRIMARY).is_err());
            }
            let mut data = original;
            data[start..start + 2].copy_from_slice(&u16::MAX.to_le_bytes());
            let e = Reader::new().read_raw(data).unwrap();
            assert!(e.image_unique_id(In::PRIMARY).is_err());
        }
    }
    #[test]
    fn unknown_manufacturer_and_unknown_tag_are_ignored() {
        let mut data = fixture(false, true, false, false);
        data[38..43].copy_from_slice(b"Other");
        let e = Reader::new().read_raw(data).unwrap();
        assert!(e.image_unique_id(In::PRIMARY).unwrap().is_none());
        let mut data = fixture(false, true, false, false);
        data[64..66].copy_from_slice(&0x29_u16.to_le_bytes());
        let e = Reader::new().read_raw(data).unwrap();
        assert!(e.image_unique_id(In::PRIMARY).unwrap().is_none());
        assert!(e.image_unique_id(In::THUMBNAIL).unwrap().is_none());
    }
    #[test]
    fn standalone_canon_tiff_uses_its_own_offset_base() {
        for le in [false, true] {
            let mut data=if le {b"II".to_vec()} else {b"MM".to_vec()};
            push16(&mut data,42,le);push32(&mut data,8,le);
            push16(&mut data,1,le);entry(&mut data,0x28,7,16,26,le);push32(&mut data,0,le);
            data.extend(0u8..16);
            assert_eq!(canon_tiff_image_unique_id(&data).unwrap().as_deref(),Some("000102030405060708090a0b0c0d0e0f"));
            for n in 0..data.len() {assert!(canon_tiff_image_unique_id(&data[..n]).is_err());}
        }
    }

}

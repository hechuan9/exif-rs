use exif::{In, Reader, Tag};
use std::io::Cursor;
fn tiff() -> Vec<u8> {
    b"II\x2a\0\x08\0\0\0\x01\0\x00\x01\x04\0\x01\0\0\0\x10\0\0\0\0\0\0\0".to_vec()
}
fn jpeg() -> Vec<u8> {
    let t = tiff();
    let mut b = vec![255, 216, 255, 225];
    b.extend_from_slice(&((t.len() + 8) as u16).to_be_bytes());
    b.extend_from_slice(b"Exif\0\0");
    b.extend(t);
    b.extend([255, 217]);
    b
}
fn check(b: Vec<u8>) {
    let e = Reader::new()
        .read_from_container(&mut Cursor::new(b))
        .unwrap();
    assert_eq!(
        e.get_field(Tag::ImageWidth, In::PRIMARY)
            .unwrap()
            .value
            .get_uint(0),
        Some(16)
    );
}
#[test]
fn raf_embedded_exif() {
    let j = jpeg();
    let mut b = vec![0; 112];
    b[..16].copy_from_slice(b"FUJIFILMCCD-RAW ");
    b[84..88].copy_from_slice(&112u32.to_be_bytes());
    b[88..92].copy_from_slice(&(j.len() as u32).to_be_bytes());
    b.extend(j);
    check(b);
}
#[test]
fn mrw_ttw_exif() {
    let t = tiff();
    let mut b = b"\0MRM".to_vec();
    b.extend_from_slice(&((t.len() + 8) as u32).to_be_bytes());
    b.extend_from_slice(b"\0TTW");
    b.extend_from_slice(&(t.len() as u32).to_be_bytes());
    b.extend(t);
    check(b);
}
#[test]
fn rw2_embedded_exif() {
    let j = jpeg();
    let mut b = b"II\x55\0\x08\0\0\0\x01\0\x2e\0\x07\0".to_vec();
    b.extend_from_slice(&(j.len() as u32).to_le_bytes());
    b.extend_from_slice(&26u32.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend(j);
    check(b);
}
fn bx(t: &[u8; 4], b: &[u8]) -> Vec<u8> {
    let mut r = ((b.len() + 8) as u32).to_be_bytes().to_vec();
    r.extend(t);
    r.extend(b);
    r
}
fn cr3() -> Vec<u8> {
    let mut b = bx(b"ftyp", b"crx \0\0\0\0crx ");
    let mut section = vec![
        0x85, 0xc0, 0xb6, 0x87, 0x82, 0x0f, 0x11, 0xe0, 0x81, 0x11, 0xf4, 0xce, 0x46, 0x2b, 0x6a,
        0x48,
    ];
    section.extend(bx(b"CMT1", &tiff()));
    let mut e = tiff();
    e[10..12].copy_from_slice(&0xa002u16.to_le_bytes());
    section.extend(bx(b"CMT2", &e));
    b.extend(bx(b"moov", &bx(b"uuid", &section)));
    b
}
#[test]
fn cr3_sections_preserve_context() {
    let m = exif::experimental::read_cr3_metadata(&mut Cursor::new(cr3())).unwrap();
    assert_eq!(m.fields[0].tag, Tag::ImageWidth);
    assert_eq!(m.fields[1].tag, Tag::PixelXDimension);
}
#[test]
fn cr3_truncations_fail() {
    let b = cr3();
    for n in 0..b.len() {
        assert!(
            exif::experimental::read_cr3_metadata(&mut Cursor::new(&b[..n])).is_err(),
            "{n}"
        );
    }
}
#[test]
fn kyocera_reversed_strings() {
    let mut b = vec![0; 156];
    b[25..32].copy_from_slice(b"ARECOYK");
    b[12..21].copy_from_slice(b"LATIGID N");
    b[33..52].copy_from_slice(b"03:41:81 61:70:5002");
    let m = exif::experimental::read_legacy_raw_metadata(&mut Cursor::new(b)).unwrap();
    assert_eq!(m.make.as_deref(), Some("KYOCERA"));
    assert_eq!(m.model.as_deref(), Some("N DIGITAL"));
    assert_eq!(m.capture_time.as_deref(), Some("2005:07:16 18:14:30"));
}
#[test]
fn malformed_raw_lengths_fail() {
    let mut b = b"\0MRM".to_vec();
    b.extend_from_slice(&1000u32.to_be_bytes());
    assert!(Reader::new()
        .read_from_container(&mut Cursor::new(b))
        .is_err());
    let mut b = vec![0; 112];
    b[..16].copy_from_slice(b"FUJIFILMCCD-RAW ");
    b[84..88].copy_from_slice(&112u32.to_be_bytes());
    b[88..92].copy_from_slice(&1000u32.to_be_bytes());
    assert!(Reader::new()
        .read_from_container(&mut Cursor::new(b))
        .is_err());
}
#[test]
fn orf_tiff_header_variants() {
    for magic in [b"IIRO", b"IIRS"] {
        let mut b = tiff();
        b[..4].copy_from_slice(magic);
        check(b);
    }
}
fn ciff() -> Vec<u8> {
    let mut b = b"II\x0e\0\0\0HEAPCCDR".to_vec();
    let mut block = b"Canon\0EOS Test\0".to_vec();
    let time_offset = block.len();
    block.extend_from_slice(&1068485966u32.to_le_bytes());
    block.extend_from_slice(&(-18000i32).to_le_bytes());
    block.extend_from_slice(&0x80000000u32.to_le_bytes());
    let dir = block.len();
    block.extend_from_slice(&2u16.to_le_bytes());
    for (tag, len, offset) in [(0x080au16, time_offset, 0), (0x180e, 12, time_offset)] {
        block.extend_from_slice(&tag.to_le_bytes());
        block.extend_from_slice(&(len as u32).to_le_bytes());
        block.extend_from_slice(&(offset as u32).to_le_bytes());
    }
    block.extend_from_slice(&(dir as u32).to_le_bytes());
    b.extend(block);
    b
}
#[test]
fn ciff_native_metadata_and_timezone() {
    let m = exif::experimental::read_legacy_raw_metadata(&mut Cursor::new(ciff())).unwrap();
    assert_eq!(m.make.as_deref(), Some("Canon"));
    assert_eq!(m.model.as_deref(), Some("EOS Test"));
    assert_eq!(m.capture_timestamp, Some(1068485966));
    assert_eq!(m.timezone_offset_seconds, Some(-18000));
}
#[test]
fn ciff_truncated_and_out_of_range() {
    let b = ciff();
    for n in 0..b.len() {
        assert!(
            exif::experimental::read_legacy_raw_metadata(&mut Cursor::new(&b[..n])).is_err(),
            "{n}"
        );
    }
}
#[test]
fn raf_sensor_dimensions_not_preview() {
    let mut b = vec![0; 112];
    b[..16].copy_from_slice(b"FUJIFILMCCD-RAW ");
    b[92..96].copy_from_slice(&112u32.to_be_bytes());
    b[96..100].copy_from_slice(&12u32.to_be_bytes());
    b.extend_from_slice(&1u32.to_be_bytes());
    b.extend_from_slice(&[1, 0, 0, 4, 0x0b, 0xb8, 0x0f, 0xa0]);
    let d = exif::experimental::read_raw_dimensions(&mut Cursor::new(b)).unwrap();
    assert_eq!(d.full, Some((4000, 3000)));
    assert_eq!(d.cropped, None);
}
#[test]
fn cr3_exif_interop_pointer_keeps_namespace() {
    let mut t = tiff();
    t[10..12].copy_from_slice(&0xa005u16.to_le_bytes());
    t[18..22].copy_from_slice(&26u32.to_le_bytes());
    t.extend_from_slice(&[
        1, 0, 1, 0, 2, 0, 4, 0, 0, 0, b'R', b'9', b'8', 0, 0, 0, 0, 0,
    ]);
    let mut section = vec![
        0x85, 0xc0, 0xb6, 0x87, 0x82, 0x0f, 0x11, 0xe0, 0x81, 0x11, 0xf4, 0xce, 0x46, 0x2b, 0x6a,
        0x48,
    ];
    section.extend(bx(b"CMT2", &t));
    let mut b = bx(b"ftyp", b"crx \0\0\0\0crx ");
    b.extend(bx(b"moov", &bx(b"uuid", &section)));
    let m = exif::experimental::read_cr3_metadata(&mut Cursor::new(b)).unwrap();
    assert!(m
        .fields
        .iter()
        .any(|f| f.tag == Tag(exif::Context::Interop, 1)));
    assert!(!m
        .fields
        .iter()
        .any(|f| f.tag == Tag(exif::Context::Exif, 1)));
}

#[test]
fn cr3_camera_identity_and_standard_precedence() {
    let mut t = tiff();
    t[10..12].copy_from_slice(&0x28u16.to_le_bytes());
    t[12..14].copy_from_slice(&7u16.to_le_bytes());
    t[14..18].copy_from_slice(&16u32.to_le_bytes());
    t[18..22].copy_from_slice(&26u32.to_le_bytes());
    t.extend_from_slice(&[0x12; 16]);
    let mut m = exif::experimental::Cr3Metadata {
        fields: vec![],
        maker_notes: Some(t),
    };
    assert_eq!(
        m.image_unique_id().unwrap().as_deref(),
        Some("12121212121212121212121212121212")
    );
    m.fields.push(exif::Field {
        tag: Tag::ImageUniqueID,
        ifd_num: In::PRIMARY,
        value: exif::Value::Ascii(vec![b"standard".to_vec()]),
    });
    assert_eq!(m.image_unique_id().unwrap().as_deref(), Some("standard"));
}

#[test]
fn ari_header_dimensions_and_invalid_offsets() {
    let mut b = vec![0; 1024];
    b[..8].copy_from_slice(b"ARRI\x12\x34\x56\x78");
    b[8..12].copy_from_slice(&1024u32.to_le_bytes());
    b[20..24].copy_from_slice(&3424u32.to_le_bytes());
    b[24..28].copy_from_slice(&2202u32.to_le_bytes());
    b[668..674].copy_from_slice(b"A-MINI");
    let m = exif::experimental::read_ari_metadata(&mut Cursor::new(&b)).unwrap();
    assert_eq!(m.model, "A-MINI");
    assert_eq!(m.dimensions, (3424, 2202));
    b[8..12].copy_from_slice(&2000u32.to_le_bytes());
    assert!(exif::experimental::read_ari_metadata(&mut Cursor::new(b)).is_err());
}

#[test]
fn rw2_sensor_full_crop_and_reversed_bounds() {
    let mut b = b"II\x55\0\x08\0\0\0\x06\0".to_vec();
    for (tag, value) in [
        (2u16, 3724u16),
        (3, 2754),
        (4, 10),
        (5, 20),
        (6, 2746),
        (7, 3668),
    ] {
        b.extend_from_slice(&tag.to_le_bytes());
        b.extend_from_slice(&3u16.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&(value as u32).to_le_bytes());
    }
    b.extend_from_slice(&0u32.to_le_bytes());
    let d = exif::experimental::read_raw_dimensions(&mut Cursor::new(&b)).unwrap();
    assert_eq!(d.full, Some((3724, 2754)));
    assert_eq!(d.cropped, Some((3648, 2736)));
    b[10 + 5 * 12 + 8..10 + 5 * 12 + 10].copy_from_slice(&1u16.to_le_bytes());
    assert!(exif::experimental::read_raw_dimensions(&mut Cursor::new(b)).is_err());
}

//! Binary/ZIP integrity without extracting untrusted archive members.
use crate::{readback::xml_signature, source::relative_path};
use cf_domain::{Error, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{Cursor, Read},
};

const MAX_ENTRIES: usize = 65_536;
const MAX_ENTRY: u64 = 256 * 1024 * 1024;
const MAX_TOTAL: u64 = 1024 * 1024 * 1024;
const MAX_MANIFEST: u64 = 8 * 1024 * 1024;

#[derive(Debug, PartialEq)]
enum Content {
    Bytes(u64, [u8; 32]),
    Manifest(Value),
}
fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}

fn archive(bytes: &[u8]) -> Result<BTreeMap<String, Content>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(crate::external)?;
    if archive.len() > MAX_ENTRIES {
        return Err(invalid("Превышен лимит числа файлов ZIP"));
    }
    if archive.has_duplicate_names() {
        return Err(invalid("Неоднозначные повторные имена ZIP"));
    }
    archive
        .validate_header_consistency()
        .map_err(crate::external)?;
    let mut output = BTreeMap::new();
    let mut total = 0_u64;
    let mut declared_total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(crate::external)?;
        let name = entry.name().map_err(crate::external)?.into_owned();
        relative_path(name.trim_end_matches('/'))?;
        if entry.encrypted() || entry.is_symlink() {
            return Err(invalid(format!(
                "Неподдерживаемый защищённый/ссылочный ZIP entry: {name}"
            )));
        }
        declared_total = declared_total
            .checked_add(entry.size())
            .ok_or_else(|| invalid("Переполнение размера ZIP"))?;
        let manifest = name.rsplit('/').next() == Some("manifest.xml");
        let limit = if manifest { MAX_MANIFEST } else { MAX_ENTRY };
        if entry.size() > limit || declared_total > MAX_TOTAL {
            return Err(invalid(format!(
                "Превышен лимит распакованных данных ZIP: {name}"
            )));
        }
        let mut length = 0_u64;
        let mut hash = Sha256::new();
        let mut xml = Vec::new();
        loop {
            let count = entry.read(&mut buffer).map_err(crate::external)?;
            if count == 0 {
                break;
            }
            length += count as u64;
            total += count as u64;
            if length > limit || total > MAX_TOTAL {
                return Err(invalid(format!(
                    "Превышен лимит распакованных данных ZIP: {name}"
                )));
            }
            hash.update(&buffer[..count]);
            if manifest {
                xml.extend_from_slice(&buffer[..count]);
            }
        }
        if length != entry.size() {
            return Err(invalid(format!("Неполное содержимое ZIP entry: {name}")));
        }
        let signature = if manifest {
            Content::Manifest(xml_signature(&xml)?)
        } else {
            Content::Bytes(length, hash.finalize().into())
        };
        if output.insert(name.clone(), signature).is_some() {
            return Err(invalid(format!("Неоднозначное имя ZIP entry: {name}")));
        }
    }
    Ok(output)
}

pub(crate) fn verify(relative: &str, before: &[u8], after: &[u8]) -> Result<()> {
    if relative
        .rsplit('.')
        .next()
        .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
    {
        let expected = archive(before)?;
        if before == after {
            return Ok(());
        }
        let actual = archive(after)?;
        if expected != actual {
            return Err(invalid("Изменился состав или содержимое ZIP"));
        }
    } else if before != after {
        return Err(invalid("Изменилось содержимое бинарного ресурса"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

    fn packed(files: &[(&str, &[u8])], compressed: bool) -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let method = if compressed {
            CompressionMethod::Deflated
        } else {
            CompressionMethod::Stored
        };
        for (name, bytes) in files {
            writer
                .start_file(
                    *name,
                    SimpleFileOptions::default().compression_method(method),
                )
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }
    #[test]
    fn repack_and_manifest_formatting_preserve_content() {
        let first = packed(
            &[
                ("100.png", b"pixels"),
                (
                    "manifest.xml",
                    b"<Picture><Variant name=\"100.png\"/></Picture>",
                ),
            ],
            false,
        );
        let second = packed(
            &[
                (
                    "manifest.xml",
                    b"<Picture>\n  <Variant name=\"100.png\"></Variant>\n</Picture>",
                ),
                ("100.png", b"pixels"),
            ],
            true,
        );
        assert_ne!(first, second);
        verify("Picture.zip", &first, &second).unwrap();
        assert!(
            verify(
                "Picture.zip",
                &first,
                &packed(&[("100.png", b"other")], false)
            )
            .is_err()
        );
        assert!(
            verify(
                "Picture.zip",
                &first,
                &packed(
                    &[("100.png", b"pixels"), ("manifest.xml", b"<Picture/>")],
                    false
                )
            )
            .is_err()
        );
    }
    #[test]
    fn corrupt_unsafe_and_duplicate_archives_are_rejected() {
        assert!(verify("x.zip", b"not a zip", b"not a zip").is_err());
        let unsafe_zip = packed(&[("../outside", b"x")], false);
        assert!(verify("x.zip", &unsafe_zip, &unsafe_zip).is_err());
        let mut duplicate = packed(&[("same", b"a"), ("diff", b"b")], false);
        for offset in 0..duplicate.len() - 4 {
            if &duplicate[offset..offset + 4] == b"diff" {
                duplicate[offset..offset + 4].copy_from_slice(b"same");
            }
        }
        assert!(verify("x.zip", &duplicate, &duplicate).is_err());
        let mut broken = packed(&[("data", b"pixels")], false);
        let offset = broken.windows(6).position(|w| w == b"pixels").unwrap();
        broken[offset] ^= 1;
        assert!(verify("x.zip", &broken, &broken).is_err());
    }
    #[test]
    fn ordinary_binary_is_exact() {
        verify("photo.png", b"abc", b"abc").unwrap();
        assert!(verify("photo.png", b"abc", b"abd").is_err());
    }

    #[test]
    fn declared_expansion_limits_are_checked_before_decompression() {
        for (name, limit) in [("data", MAX_ENTRY), ("manifest.xml", MAX_MANIFEST)] {
            let mut bytes = packed(&[(name, b"x")], false);
            let local = bytes.windows(4).position(|w| w == b"PK\x03\x04").unwrap();
            let central = bytes.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
            let size = ((limit + 1) as u32).to_le_bytes();
            bytes[local + 22..local + 26].copy_from_slice(&size);
            bytes[central + 24..central + 28].copy_from_slice(&size);
            let error = verify("resource.zip", &bytes, &bytes)
                .unwrap_err()
                .to_string();
            assert!(error.contains("лимит"), "{error}");
        }
    }
}

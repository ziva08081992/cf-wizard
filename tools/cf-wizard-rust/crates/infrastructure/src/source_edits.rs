//! Generic byte emitter and an independent streaming expectation for frozen edits.
use cf_domain::{Error, Result, package_plan::SourceEdit};
use sha2::{Digest, Sha256};

fn ordered<'a>(source: &[u8], edits: &'a [SourceEdit]) -> Result<Vec<&'a SourceEdit>> {
    let mut edits: Vec<_> = edits.iter().collect();
    edits.sort_by_key(|edit| (edit.start, edit.end));
    let mut end = 0;
    for edit in &edits {
        if edit.start < end || edit.end < edit.start || edit.end > source.len() {
            return Err(Error::Invalid(
                "Перекрывающиеся или недопустимые границы плана XML".into(),
            ));
        }
        end = edit.end;
    }
    Ok(edits)
}

pub(crate) fn digest(source: &[u8], edits: &[SourceEdit]) -> Result<String> {
    let mut hash = Sha256::new();
    let mut cursor = 0;
    for edit in ordered(source, edits)? {
        hash.update(&source[cursor..edit.start]);
        hash.update(edit.replacement.as_bytes());
        cursor = edit.end;
    }
    hash.update(&source[cursor..]);
    Ok(format!("{:x}", hash.finalize()))
}

pub(crate) fn emit(source: &[u8], edits: &[SourceEdit]) -> Result<Vec<u8>> {
    let edits = ordered(source, edits)?;
    let mut output = Vec::with_capacity(source.len());
    let mut cursor = 0;
    for edit in edits {
        output.extend_from_slice(&source[cursor..edit.start]);
        output.extend_from_slice(edit.replacement.as_bytes());
        cursor = edit.end;
    }
    output.extend_from_slice(&source[cursor..]);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untouched_bom_comments_cdata_and_tail_are_byte_exact() {
        let source = b"\xef\xbb\xbf<root><!--keep--><a>old</a><![CDATA[stay]]></root>\r\n";
        let edits = [SourceEdit {
            start: 23,
            end: 26,
            replacement: "new".into(),
        }];
        // Independent literal deliberately includes every untouched byte.
        let expected = b"\xef\xbb\xbf<root><!--keep--><a>new</a><![CDATA[stay]]></root>\r\n";
        assert_eq!(emit(source, &edits).unwrap(), expected);
        assert_eq!(
            digest(source, &edits).unwrap(),
            crate::canonical::sha256(expected)
        );
        let overlapping = [
            SourceEdit {
                start: 0,
                end: 8,
                replacement: String::new(),
            },
            SourceEdit {
                start: 7,
                end: 9,
                replacement: String::new(),
            },
        ];
        assert!(emit(source, &overlapping).is_err());
        assert!(digest(source, &overlapping).is_err());
    }
}

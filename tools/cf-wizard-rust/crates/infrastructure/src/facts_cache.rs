//! Versioned payload codec. V1 bytes are not relabelled or rewritten by this codec.
use cf_application::profile;
use cf_domain::{
    Error, Result,
    source_facts::{Coverage, Facet, FactVersions, InputObservation, SourceFacts},
};
use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
};

#[path = "facts_compact.rs"]
mod compact;

pub(crate) const CONTRACT: &str = "cf-source-facts-v2";
pub(crate) const CONTRACT_V3: &str = "cf-source-facts-v3";
pub(crate) const OBJECT_FACETS: &[Facet] = &[Facet::Metadata, Facet::References, Facet::Assets];
pub(crate) const REGISTRAR_FACETS: &[Facet] = &[Facet::Registrars];
const MAX_PAYLOAD: u64 = 64 * 1024 * 1024;
const V3_COMPACT: &[u8] = b"CFSF\x01";
const V3_V2: &[u8] = b"CFSF\x00";

fn oversized() -> Error {
    Error::Invalid("Слишком большой SourceFacts payload".into())
}

/// A narrow signature permits reuse of unaffected producers after a reader change.
/// Consuming rule versions intentionally do not occur in this signature.
pub(crate) fn signature(versions: &FactVersions, facets: &[Facet]) -> Result<String> {
    let readers: BTreeMap<_, _> = facets
        .iter()
        .map(|facet| {
            versions
                .readers
                .get(facet)
                .map(|reader| (*facet, reader.clone()))
                .ok_or_else(|| Error::Invalid(format!("Нет версии reader: {facet:?}")))
        })
        .collect::<Result<_>>()?;
    serde_json::to_string(&(versions.schema, readers)).map_err(crate::external)
}
#[cfg(test)]
pub(crate) fn encode(facts: &SourceFacts) -> Result<Vec<u8>> {
    profile::measure("facts.validate.codec", "facts", 0, None, || {
        facts.validate()
    })?;
    let payload = serde_json::to_vec(facts).map_err(crate::external)?;
    if payload.len() as u64 > MAX_PAYLOAD {
        return Err(oversized());
    }
    compress(&payload, &[])
}

struct BoundedPayload {
    data: Vec<u8>,
    exceeded: bool,
}

impl Write for BoundedPayload {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_PAYLOAD as usize - self.data.len() {
            self.exceeded = true;
            return Err(io::Error::other("SourceFacts payload limit"));
        }
        self.data.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn bounded_json(value: &impl serde::Serialize) -> Result<Vec<u8>> {
    let mut output = BoundedPayload {
        data: Vec::new(),
        exceeded: false,
    };
    serde_json::to_writer(&mut output, value).map_err(|error| {
        if output.exceeded {
            oversized()
        } else {
            crate::external(error)
        }
    })?;
    Ok(output.data)
}

fn compress(payload: &[u8], prefix: &[u8]) -> Result<Vec<u8>> {
    let mut writer = ZlibEncoder::new(prefix.to_vec(), Compression::fast());
    writer.write_all(payload).map_err(crate::external)?;
    writer.finish().map_err(crate::external)
}

pub(crate) fn encode_v3(facts: &SourceFacts) -> Result<Vec<u8>> {
    profile::measure("facts.validate.codec", "facts", 0, None, || {
        facts.validate()
    })?;
    if let Some(wire) = compact::DescriptorFacts::from_facts(facts)? {
        wire.check_budget()?;
        compress(&bounded_json(&wire)?, V3_COMPACT)
    } else {
        compress(&bounded_json(facts)?, V3_V2)
    }
}

pub(crate) fn decode(
    data: &[u8],
    object: &str,
    descriptor_hash: &[u8],
    versions: &FactVersions,
) -> Result<SourceFacts> {
    decode_row(data, object, descriptor_hash, versions, false)
}

pub(crate) fn decode_v3(
    data: &[u8],
    object: &str,
    descriptor_hash: &[u8],
    versions: &FactVersions,
) -> Result<SourceFacts> {
    decode_row(data, object, descriptor_hash, versions, true)
}

fn decode_row(
    data: &[u8],
    object: &str,
    descriptor_hash: &[u8],
    versions: &FactVersions,
    framed: bool,
) -> Result<SourceFacts> {
    profile::measure(
        "facts.decode",
        "facts.lookup",
        data.len(),
        Some(object),
        || {
            let (compressed, compact) = if !framed {
                (data, false)
            } else if let Some(compressed) = data.strip_prefix(V3_COMPACT) {
                (compressed, true)
            } else if let Some(compressed) = data.strip_prefix(V3_V2) {
                (compressed, false)
            } else {
                return Err(Error::Invalid(
                    "Неизвестный формат строки SourceFacts V3".into(),
                ));
            };
            let mut inflate = profile::span("facts.inflate", "facts.decode", data.len(), None);
            let mut payload = Vec::new();
            if framed {
                let mut decoder = flate2::bufread::ZlibDecoder::new(compressed);
                (&mut decoder)
                    .take(MAX_PAYLOAD + 1)
                    .read_to_end(&mut payload)
                    .map_err(crate::external)?;
                if payload.len() as u64 <= MAX_PAYLOAD && !decoder.get_ref().is_empty() {
                    return Err(Error::Invalid(
                        "Лишние данные в строке SourceFacts V3".into(),
                    ));
                }
            } else {
                ZlibDecoder::new(compressed)
                    .take(MAX_PAYLOAD + 1)
                    .read_to_end(&mut payload)
                    .map_err(crate::external)?;
            }
            inflate.success();
            drop(inflate);
            profile::note("facts.inflated_bytes", "facts.decode", payload.len());
            if payload.len() as u64 > MAX_PAYLOAD {
                return Err(oversized());
            }
            let facts: SourceFacts = if compact {
                let dto: compact::DescriptorFacts =
                    profile::measure("facts.json", "facts.decode", payload.len(), None, || {
                        serde_json::from_slice(&payload).map_err(crate::external)
                    })?;
                drop(payload);
                dto.expand()?
            } else {
                profile::measure("facts.json", "facts.decode", payload.len(), None, || {
                    serde_json::from_slice(&payload).map_err(crate::external)
                })?
            };
            profile::measure("facts.validate.codec", "facts", 0, None, || {
                facts.validate()
            })?;
            if facts.object != object
                || OBJECT_FACETS.iter().any(|facet| {
                    !facts.versions.compatible(versions, *facet)
                        || facts.coverage.get(facet) != Some(&Coverage::Supported)
                })
            {
                return Err(Error::Invalid(
                    "Семантика SourceFacts устарела: обновите индекс по кнопке".into(),
                ));
            }
            let object = facts
                .fragments
                .values()
                .find(|fragment| fragment.kind == cf_domain::source_facts::FragmentKind::Object)
                .ok_or_else(|| Error::Invalid("Нет объекта в SourceFacts payload".into()))?;
            let expected = hex::encode(descriptor_hash);
            if object.origin.sha256 != expected
                || facts.proof.inputs.get(&object.origin.path)
                    != Some(&InputObservation::Content { sha256: expected })
            {
                return Err(Error::Invalid(
                    "SourceFacts не соответствует hash descriptor".into(),
                ));
            }
            Ok(facts)
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use cf_domain::source_facts::SourceProof;
    use sha2::{Digest, Sha256};
    fn facts() -> (SourceFacts, Vec<u8>) {
        let bytes = b"<MetaDataObject><Catalog uuid='original'><Properties><Name>Probe</Name><Type>CatalogRef.Other</Type></Properties></Catalog></MetaDataObject>";
        let document = crate::source_reader::SourceDocument::descriptor(
            "Catalog.Probe",
            "Catalogs/Probe.xml",
            bytes,
            SourceProof {
                identity: "synthetic".into(),
                snapshot: "one".into(),
                inputs: BTreeMap::new(),
            },
        )
        .unwrap();
        (document.facts, Sha256::digest(bytes).to_vec())
    }
    #[test]
    fn source_facts_roundtrip_preserves_evidence_and_structural_subset() {
        let (facts, hash) = facts();
        let decoded = decode(
            &encode(&facts).unwrap(),
            "Catalog.Probe",
            &hash,
            &FactVersions::current(),
        )
        .unwrap();
        assert_eq!(facts, decoded);
        assert_eq!(
            decoded.structural_facts().unwrap().targets,
            ["Catalog.Other".into()].into()
        );
    }

    fn historical_payload(facts: &SourceFacts) -> Vec<u8> {
        let json = serde_json::to_vec(facts).unwrap();
        let mut compressed = ZlibEncoder::new(Vec::new(), Compression::best());
        compressed.write_all(&json).unwrap();
        compressed.finish().unwrap()
    }

    #[test]
    fn historical_v2_uses_the_same_validation_and_proof() {
        let (facts, hash) = facts();
        let read = |candidate: &SourceFacts| {
            decode(
                &historical_payload(candidate),
                &facts.object,
                &hash,
                &FactVersions::current(),
            )
        };
        assert_eq!(facts, read(&facts).unwrap());

        // The V2 validator also accepts acyclic addresses that do not encode the ancestry.
        let mut nonhierarchical = facts.clone();
        let name = nonhierarchical
            .fragments
            .values_mut()
            .find(|fragment| fragment.tag.local == "Name")
            .unwrap();
        name.parent = Some(name.id.replace("}Name[0]", "}Type[0]"));
        assert_eq!(nonhierarchical, read(&nonhierarchical).unwrap());

        let mut missing_parent = facts.clone();
        missing_parent
            .fragments
            .first_entry()
            .unwrap()
            .get_mut()
            .parent = Some("foreign".into());
        assert!(
            read(&missing_parent)
                .unwrap_err()
                .to_string()
                .contains("Неизвестный родитель")
        );

        let mut cycle = facts.clone();
        let (root, child) = (
            cycle.fragments.keys().next().unwrap().clone(),
            cycle.fragments.keys().nth(1).unwrap().clone(),
        );
        cycle.fragments.get_mut(&root).unwrap().parent = Some(child);
        assert!(
            read(&cycle)
                .unwrap_err()
                .to_string()
                .contains("Цикл родителей")
        );

        let mut missing_origin = facts.clone();
        missing_origin.proof.inputs.clear();
        assert!(
            read(&missing_origin)
                .unwrap_err()
                .to_string()
                .contains("Неподтверждённый источник")
        );

        let mut invalid_reference = facts.clone();
        invalid_reference.references[0].fragment = "missing".into();
        assert!(
            read(&invalid_reference)
                .unwrap_err()
                .to_string()
                .contains("Неизвестный origin ссылки")
        );

        let mut unsupported = facts.clone();
        unsupported.coverage.remove(&Facet::Metadata);
        assert!(
            read(&unsupported)
                .unwrap_err()
                .to_string()
                .contains("Семантика SourceFacts устарела")
        );
    }

    #[test]
    fn oversized_inflated_v2_and_truncated_zlib_are_rejected() {
        let (facts, hash) = facts();
        let read = |payload: &[u8]| decode(payload, &facts.object, &hash, &FactVersions::current());
        let mut payload = historical_payload(&facts);
        payload.pop();
        assert!(read(&payload).is_err());

        let chunk = [b' '; 64 * 1024];
        let mut compressed = ZlibEncoder::new(Vec::new(), Compression::fast());
        for _ in 0..MAX_PAYLOAD / chunk.len() as u64 {
            compressed.write_all(&chunk).unwrap();
        }
        compressed.write_all(b" ").unwrap();
        let payload = compressed.finish().unwrap();
        assert!(
            read(&payload)
                .unwrap_err()
                .to_string()
                .contains("Слишком большой")
        );
    }
    #[test]
    fn unchanged_bytes_do_not_upgrade_extraction_semantics() {
        let (mut facts, hash) = facts();
        facts
            .versions
            .readers
            .insert(Facet::Metadata, "historical-reader".into());
        let bytes = encode(&facts).unwrap();
        assert!(decode(&bytes, "Catalog.Probe", &hash, &FactVersions::current()).is_err());
        let original = FactVersions::current();
        let mut registrars_changed = original.clone();
        registrars_changed
            .readers
            .insert(Facet::Registrars, "registrars-next".into());
        assert_eq!(
            signature(&original, OBJECT_FACETS).unwrap(),
            signature(&registrars_changed, OBJECT_FACETS).unwrap()
        );
        assert_ne!(
            signature(&original, REGISTRAR_FACETS).unwrap(),
            signature(&registrars_changed, REGISTRAR_FACETS).unwrap()
        );
    }
    #[test]
    fn wrong_object_source_hash_and_legacy_shape_cannot_be_used_as_owned_facts() {
        let (facts, hash) = facts();
        let payload = encode(&facts).unwrap();
        assert!(
            decode(
                &payload,
                "Catalog.Different",
                &hash,
                &FactVersions::current()
            )
            .is_err()
        );
        assert!(
            decode(
                &payload,
                "Catalog.Probe",
                &[0; 32],
                &FactVersions::current()
            )
            .is_err()
        );
        let legacy = crate::index::encode_facts(&cf_domain::Facts::default()).unwrap();
        assert!(decode(&legacy, "Catalog.Probe", &hash, &FactVersions::current()).is_err());
    }
}

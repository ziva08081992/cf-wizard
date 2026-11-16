//! Versioned payload codec. V1 bytes are not relabelled or rewritten by this codec.
use cf_domain::{
    Error, Result,
    source_facts::{Coverage, Facet, FactVersions, InputObservation, SourceFacts},
};
use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
};

pub(crate) const CONTRACT: &str = "cf-source-facts-v2";
pub(crate) const OBJECT_FACETS: &[Facet] = &[Facet::Metadata, Facet::References, Facet::Assets];
pub(crate) const REGISTRAR_FACETS: &[Facet] = &[Facet::Registrars];
const MAX_PAYLOAD: u64 = 64 * 1024 * 1024;

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
pub(crate) fn encode(facts: &SourceFacts) -> Result<Vec<u8>> {
    facts.validate()?;
    let payload = serde_json::to_vec(facts).map_err(crate::external)?;
    if payload.len() as u64 > MAX_PAYLOAD {
        return Err(Error::Invalid("Слишком большой SourceFacts payload".into()));
    }
    let mut writer = ZlibEncoder::new(Vec::new(), Compression::fast());
    writer.write_all(&payload).map_err(crate::external)?;
    writer.finish().map_err(crate::external)
}
pub(crate) fn decode(
    data: &[u8],
    object: &str,
    descriptor_hash: &[u8],
    versions: &FactVersions,
) -> Result<SourceFacts> {
    let mut payload = Vec::new();
    ZlibDecoder::new(data)
        .take(MAX_PAYLOAD + 1)
        .read_to_end(&mut payload)
        .map_err(crate::external)?;
    if payload.len() as u64 > MAX_PAYLOAD {
        return Err(Error::Invalid("Слишком большой SourceFacts payload".into()));
    }
    let facts: SourceFacts = serde_json::from_slice(&payload).map_err(crate::external)?;
    facts.validate()?;
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

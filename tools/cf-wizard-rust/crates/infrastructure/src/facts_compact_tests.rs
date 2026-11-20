use super::*;
use crate::facts_cache::{V3_COMPACT, V3_V2, decode, decode_v3, encode, encode_v3};
use crate::source_reader::SourceDocument;
use cf_domain::source_facts::InputObservation;
use sha2::{Digest, Sha256};

const OBJECT: &str = "Catalog.Probe";
const PATH: &str = "Catalogs/Probe.xml";
const XML: &[u8] = br#"<MetaDataObject><Catalog uuid='ProbeUuid'><Properties><Name>Probe</Name><Type>CatalogRef.Other</Type><x:Custom xmlns:x='urn:fixture'><x:Entry/><x:Entry/></x:Custom></Properties><ChildObjects><Attribute><Properties><Name>Nested</Name></Properties></Attribute></ChildObjects></Catalog></MetaDataObject>"#;

fn proof(snapshot: &str) -> SourceProof {
    SourceProof {
        identity: "fixture".into(),
        snapshot: snapshot.into(),
        inputs: BTreeMap::new(),
    }
}

fn fixture() -> (SourceFacts, Vec<u8>) {
    let mut facts = SourceDocument::descriptor(OBJECT, PATH, XML, proof("before"))
        .unwrap()
        .facts;
    let address = facts
        .fragments
        .values()
        .find(|fragment| fragment.kind == FragmentKind::Object)
        .unwrap()
        .id
        .clone();
    facts.coverage_notes.push(CoverageNote {
        facet: Facet::References,
        fragment: address,
        coverage: Coverage::Unsupported("Ограничение \"reader\"".into()),
    });
    facts
        .proof
        .inputs
        .insert("Missing.xml".into(), InputObservation::Absent);
    (facts, Sha256::digest(XML).to_vec())
}

fn read(payload: &[u8], digest: &[u8]) -> Result<SourceFacts> {
    decode_v3(payload, OBJECT, digest, &FactVersions::current())
}

#[test]
fn v3_roundtrip_equals_v2_and_reuses_the_actual_xml_proof() {
    let (facts, hash) = fixture();
    let old = encode(&facts).unwrap();
    let compact = encode_v3(&facts).unwrap();
    assert!(compact.starts_with(V3_COMPACT));
    let v2 = decode(&old, OBJECT, &hash, &FactVersions::current()).unwrap();
    let v3 = read(&compact, &hash).unwrap();
    assert_eq!(facts, v2);
    assert_eq!(facts, v3);
    assert_eq!(
        serde_json::to_vec(&v2).unwrap(),
        serde_json::to_vec(&v3).unwrap()
    );
    assert_eq!(
        v2.structural_facts().unwrap(),
        v3.structural_facts().unwrap()
    );
    let v2 = SourceDocument::cached(OBJECT, PATH, XML, proof("current"), v2).unwrap();
    let v3 = SourceDocument::cached(OBJECT, PATH, XML, proof("current"), v3).unwrap();
    assert_eq!(v3.facts.proof.snapshot, "current");
    assert_eq!(v2.facts, v3.facts);
    assert_eq!(v2.node_ids, v3.node_ids);
    let mut changed = XML.to_vec();
    changed.push(b' ');
    assert!(SourceDocument::cached(OBJECT, PATH, &changed, proof("current"), v3.facts).is_err());
}

#[test]
fn v3_preserves_valid_noncanonical_v2_as_an_explicit_fallback() {
    let (mut facts, hash) = fixture();
    facts.fragments.values_mut().next().unwrap().origin.node[0].ordinal += 50;
    facts.validate().unwrap();
    let fallback = encode_v3(&facts).unwrap();
    assert!(fallback.starts_with(V3_V2));
    assert_eq!(read(&fallback, &hash).unwrap(), facts);
    assert_eq!(&fallback[V3_V2.len()..], encode(&facts).unwrap());
}

#[test]
fn v3_framing_and_existing_descriptor_guards_fail_closed() {
    let (mut facts, hash) = fixture();
    let compact = encode_v3(&facts).unwrap();
    assert!(read(&compact, &[0; 32]).is_err());
    assert!(read(&encode(&facts).unwrap(), &hash).is_err());
    assert!(decode(&compact, OBJECT, &hash, &FactVersions::current()).is_err());
    let mut unknown = compact.clone();
    unknown[V3_COMPACT.len() - 1] = 255;
    assert!(read(&unknown, &hash).is_err());
    assert!(read(&compact[..compact.len() - 1], &hash).is_err());
    let mut trailing = compact;
    trailing.push(0);
    assert!(read(&trailing, &hash).is_err());
    facts
        .versions
        .readers
        .insert(Facet::Metadata, "stale".into());
    assert!(read(&encode_v3(&facts).unwrap(), &hash).is_err());
}

#[test]
fn preflight_matches_exact_serde_bytes_and_charges_links_before_expansion() {
    let (facts, _) = fixture();
    let mut wire = DescriptorFacts::from_facts(&facts).unwrap().unwrap();
    wire.names.0[0].uri = "urn:\\unicode\t\"тег\"\n".into();
    wire.nodes.0[0].4 = "\u{1}Фрагмент".into();
    let (charged, _) = wire.preflight(LIMIT).unwrap();
    let expanded = wire.expand().unwrap();
    assert_eq!(charged, serde_json::to_vec(&expanded).unwrap().len());
    let wire = DescriptorFacts::from_facts(&expanded).unwrap().unwrap();
    assert_eq!(wire.preflight(charged).unwrap().0, charged);
    assert!(wire.preflight(charged - 1).is_err());
}

#[test]
fn small_wire_cannot_allocate_an_expanded_graph_over_the_limit() {
    let (facts, _) = fixture();
    let mut wire = DescriptorFacts::from_facts(&facts).unwrap().unwrap();
    wire.names.0 = vec![ExpandedName {
        uri: "x".repeat(300_000),
        local: "Tiny".into(),
    }];
    wire.nodes.0 = (0..400)
        .map(|ordinal| {
            Node(
                None,
                0,
                ordinal,
                FragmentKind::Value,
                String::new(),
                None,
                Some((0, 1)),
            )
        })
        .collect();
    wire.references.0.clear();
    wire.coverage_notes.0.clear();
    assert!(serde_json::to_vec(&wire).unwrap().len() < LIMIT);
    assert!(
        matches!(wire.expand(), Err(Error::Invalid(message)) if message.contains("Слишком большой"))
    );
}

#[test]
fn invalid_parent_link_name_depth_and_duplicates_are_rejected() {
    let (facts, _) = fixture();
    let wire = || DescriptorFacts::from_facts(&facts).unwrap().unwrap();
    let mut mutation = wire();
    mutation.nodes.0[0].0 = Some(0);
    assert!(mutation.expand().is_err());
    let mut mutation = wire();
    mutation.nodes.0[0].1 = u32::MAX;
    assert!(mutation.expand().is_err());
    let mut mutation = wire();
    mutation.coverage_notes.0[0].1 = u32::MAX;
    assert!(mutation.expand().is_err());
    let mut mutation = wire();
    let duplicate =
        serde_json::from_slice(&serde_json::to_vec(&mutation.nodes.0[0]).unwrap()).unwrap();
    mutation.nodes.0.push(duplicate);
    assert!(mutation.expand().is_err());
    let mut mutation = wire();
    mutation.nodes.0 = (0..=DEPTH)
        .map(|i| {
            Node(
                i.checked_sub(1).map(|parent| parent as u32),
                0,
                i,
                FragmentKind::Value,
                String::new(),
                None,
                None,
            )
        })
        .collect();
    assert!(mutation.expand().is_err());
    assert!(serde_json::from_str::<LimitedVec<u32, 2>>("[0,1,2]").is_err());
    assert_eq!(
        serde_json::from_str::<LimitedVec<u32, 2>>("[0,1]")
            .unwrap()
            .0,
        [0, 1]
    );
}

use super::*;
use crate::{
    emission_guard,
    source_xml::{CORE, CURRENT, MD},
    xml::Edit,
};
use cf_domain::source_facts::{ReferenceStrength, SourceProof};

fn source() -> SourceDocument {
    let xml = format!(
        r#"<m:MetaDataObject xmlns:m="{MD}" xmlns:v="{CORE}" xmlns:c="{CURRENT}" xmlns:r="{READABLE}">
 <m:Catalog uuid="object-uuid"><m:Properties>
  <m:Name>Host</m:Name><m:Type><v:Type>c:CatalogRef.A</v:Type></m:Type>
  <m:DefaultForm>Catalog.B.Form.Main</m:DefaultForm>
  <m:Owners><r:Item>Catalog.A</r:Item><r:Item>Catalog.B</r:Item></m:Owners>
 </m:Properties><m:ChildObjects>
  <m:Attribute uuid="keep-uuid"><m:Properties><m:Name>Keep</m:Name><m:Type><v:Type>c:CatalogRef.C</v:Type></m:Type></m:Properties></m:Attribute>
  <m:Attribute uuid="drop-uuid"><m:Properties><m:Name>Drop</m:Name><m:Type><v:Type>c:CatalogRef.D</v:Type></m:Type></m:Properties></m:Attribute>
 </m:ChildObjects></m:Catalog></m:MetaDataObject>"#
    );
    SourceDocument::descriptor(
        "Catalog.Host",
        "Catalogs/Host.xml",
        xml.as_bytes(),
        SourceProof {
            identity: "independent-model".into(),
            snapshot: "one".into(),
            inputs: BTreeMap::new(),
        },
    )
    .unwrap()
}
fn selection() -> StubSpec {
    StubSpec {
        finalized: true,
        members: BTreeSet::from([("Attribute".into(), "Keep".into())]),
        ..StubSpec::default()
    }
}
fn emit(source: &SourceDocument, plan: &DescriptorPlan) -> String {
    source
        .view
        .xml
        .apply(
            plan.edits
                .iter()
                .map(|edit| Edit {
                    range: edit.start..edit.end,
                    replacement: edit.replacement.clone(),
                })
                .collect(),
        )
        .unwrap()
}
#[test]
fn dependencies_come_from_retained_source_fragments_before_emission() {
    let source = source();
    let plan = compile(
        &source,
        "Catalog.Host",
        &selection(),
        &BTreeSet::from(["Catalog.A".into()]),
    )
    .unwrap();
    let targets: BTreeSet<_> = plan.references.iter().map(|r| r.target.key()).collect();
    assert_eq!(
        targets,
        BTreeSet::from(["Catalog.A".into(), "Catalog.C".into()])
    );
    assert!(
        plan.references
            .iter()
            .any(|r| r.target.key() == "Catalog.C" && r.strength == ReferenceStrength::Structured)
    );
    assert_eq!(
        plan.observations,
        BTreeMap::from([("Catalog.A".into(), true), ("Catalog.B".into(), false)])
    );
    // Expectations above are asserted before any XML writer is invoked.
    let output = emit(&source, &plan);
    emission_guard::descriptor(&plan.expected, output.as_bytes(), "fixture.xml").unwrap();
    assert!(output.contains("<m:DefaultForm/>") && !output.contains("drop-uuid"));
    assert!(output.contains("<m:Attribute uuid=\"keep-uuid\">"));
}
#[test]
fn explicit_property_item_is_not_replaced_by_presence_or_a_sibling() {
    let source = source();
    let mut selected = selection();
    selected
        .property_items
        .insert("Owners".into(), BTreeSet::from([1]));
    let plan = compile(
        &source,
        "Catalog.Host",
        &selected,
        &BTreeSet::from(["Catalog.A".into()]),
    )
    .unwrap();
    let owners = plan
        .references
        .iter()
        .filter(|r| {
            source.facts.fragments[&r.fragment].kind
                == cf_domain::source_facts::FragmentKind::PropertyItem
        })
        .map(|r| r.target.key())
        .collect::<BTreeSet<_>>();
    assert_eq!(owners, BTreeSet::from(["Catalog.B".into()]));
    let output = emit(&source, &plan);
    emission_guard::descriptor(&plan.expected, output.as_bytes(), "fixture.xml").unwrap();
    selected
        .property_items
        .insert("Owners".into(), BTreeSet::from([99]));
    assert!(compile(&source, "Catalog.Host", &selected, &BTreeSet::new()).is_err());
}
#[test]
fn independent_guard_detects_missing_extra_value_type_and_uuid_mutations() {
    let source = source();
    let plan = compile(
        &source,
        "Catalog.Host",
        &selection(),
        &BTreeSet::from(["Catalog.A".into()]),
    )
    .unwrap();
    let output = emit(&source, &plan);
    for mutant in [
        output.replace("keep-uuid", "wrong-uuid"),
        output.replace("<m:Name>Keep</m:Name>", "<m:Name>Wrong</m:Name>"),
        output.replace("c:CatalogRef.C", "c:DocumentRef.C"),
        output.replace(
            "</m:ChildObjects>",
            "<m:Form>Extra</m:Form></m:ChildObjects>",
        ),
        output
            .replace(
                "<m:Attribute uuid=\"keep-uuid\">",
                "<m:Missing uuid=\"keep-uuid\">",
            )
            .replace("</m:Attribute>", "</m:Missing>"),
    ] {
        assert!(
            emission_guard::descriptor(&plan.expected, mutant.as_bytes(), "mutant.xml").is_err()
        );
    }
}
#[test]
fn compiler_does_not_invent_minimum_fields_and_keeps_original_bytes() {
    let source = source();
    let mut selected = selection();
    selected.finalized = false;
    assert!(compile(&source, "Catalog.Host", &selected, &BTreeSet::new()).is_err());
    selected.finalized = true;
    let plan = compile(&source, "Catalog.Host", &selected, &BTreeSet::new()).unwrap();
    let output = emit(&source, &plan);
    let original = source.view.xml.source.as_str();
    let mut from = 0;
    let mut at = 0;
    for edit in &plan.edits {
        let untouched = &original[from..edit.start];
        assert_eq!(&output[at..at + untouched.len()], untouched);
        at += untouched.len() + edit.replacement.len();
        from = edit.end;
    }
    assert_eq!(&output[at..], &original[from..]);
}
#[test]
fn outer_attribute_does_not_change_the_compatible_scalar_reset_rule() {
    let xml = format!(
        r#"<MetaDataObject xmlns="{MD}" xmlns:i="http://www.w3.org/2001/XMLSchema-instance" xmlns:c="{CURRENT}"><Catalog uuid="original"><Properties><Name>Host</Name><Visual i:type="c:CatalogRef.Decoration"/></Properties></Catalog></MetaDataObject>"#
    );
    let source = SourceDocument::descriptor(
        "Catalog.Host",
        "Catalogs/Host.xml",
        xml.as_bytes(),
        SourceProof {
            identity: "synthetic".into(),
            snapshot: "one".into(),
            inputs: BTreeMap::new(),
        },
    )
    .unwrap();
    let plan = compile(&source, "Catalog.Host", &selection(), &BTreeSet::new()).unwrap();
    assert!(
        plan.edits.is_empty(),
        "outer attrs were not an input to the former scalar reset rule"
    );
    assert!(
        plan.references
            .iter()
            .any(|r| r.target.key() == "Catalog.Decoration")
    );
    emission_guard::descriptor(&plan.expected, xml.as_bytes(), "fixture.xml").unwrap();
}
#[test]
fn package_guard_rejects_missing_extra_and_changed_files() {
    use cf_domain::package_plan::*;
    use cf_domain::source_facts::{FactVersions, RULE_PROFILE};
    let source = source();
    let descriptor = compile(
        &source,
        "Catalog.Host",
        &selection(),
        &BTreeSet::from(["Catalog.A".into()]),
    )
    .unwrap();
    let output = emit(&source, &descriptor);
    let proof = source.facts.proof.clone();
    let stamp = PlanStamp {
        generation: "independent-selection-1".into(),
        proof_digest: proof.digest().unwrap(),
        readers: FactVersions::current(),
        rule_profile: RULE_PROFILE.into(),
    };
    let path = "Catalogs/Host.xml".to_owned();
    let plan = CompiledPackagePlan {
        source: proof,
        projection: ProjectionPlan {
            selected: BTreeSet::new(),
            dependencies: BTreeSet::from(["Catalog.Host".into()]),
            stamp: stamp.clone(),
            objects: BTreeMap::from([(
                "Catalog.Host".into(),
                ObjectProjection {
                    content: ContentMode::Projected,
                    role: SelectionRole::Dependency,
                    descriptor: path.clone(),
                    retained: descriptor.retained.clone(),
                    declarations: BTreeMap::new(),
                    identity: BTreeMap::new(),
                },
            )]),
            files: BTreeMap::from([(
                path.clone(),
                FileProjection {
                    owner: "Catalog.Host".into(),
                    content: ContentMode::Projected,
                    operation: FileOperation::Project {
                        source: path.clone(),
                        edits: descriptor.edits,
                    },
                    expectation: FileExpectation::Xml {
                        sha256: crate::canonical::sha256(output.as_bytes()),
                        root: descriptor.expected,
                    },
                },
            )]),
        },
        merge: MergePlan {
            stamp,
            children: BTreeMap::new(),
            order: vec!["Catalog.Host".into()],
            objects: BTreeMap::from([(
                "Catalog.Host".into(),
                MergeObjectPlan {
                    action: MergeAction::MergePrioritizingMainConfiguration,
                    protected: vec![],
                },
            )]),
        },
    };
    let inventory = BTreeSet::from([path]);
    emission_guard::package(&plan, &inventory, |_| Ok(output.as_bytes().to_vec())).unwrap();
    let mut read_count = 0;
    for wrong in [
        BTreeSet::new(),
        BTreeSet::from(["Catalogs/Host.xml".into(), "unexpected.bsl".into()]),
    ] {
        assert!(
            emission_guard::package(&plan, &wrong, |_| {
                read_count += 1;
                Ok(vec![])
            })
            .is_err()
        );
    }
    assert_eq!(
        read_count, 0,
        "inventory failure precedes payload reads/native"
    );
    assert!(
        emission_guard::package(&plan, &inventory, |_| Ok(output
            .replace("keep-uuid", "changed")
            .into_bytes()))
        .is_err()
    );
}

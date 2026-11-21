use super::*;
use crate::source_xml::MD;

fn document(key: &str, xml: &str) -> SourceDocument {
    SourceDocument::descriptor(
        key,
        "fixture.xml",
        xml.as_bytes(),
        SourceProof {
            identity: "synthetic".into(),
            snapshot: "immutable-one".into(),
            inputs: BTreeMap::new(),
        },
    )
    .unwrap()
}

#[test]
fn parent_first_addresses_preserve_namespace_paths_and_cached_evidence() {
    let key = "Catalog.Owner";
    let xml = format!(
        "<MetaDataObject xmlns='{MD}' xmlns:x='urn:outer'><Catalog><Properties><Name>Owner</Name></Properties><ChildObjects><TabularSection><Properties><Name>Rows</Name></Properties><ChildObjects>{}</ChildObjects></TabularSection></ChildObjects></Catalog></MetaDataObject>",
        "<Attribute><Properties><Name>Field</Name><x:Layer><x:Item/><x:Item/><x:Layer xmlns:x='urn:inner'><x:Item/><x:Item/></x:Layer></x:Layer></Properties></Attribute>".repeat(12)
    );
    let original = document(key, &xml);
    for (id, current) in original.node_ids.iter().enumerate() {
        assert!(Arc::ptr_eq(current, &original.facts.fragments[current].id));
        let legacy = original
            .view
            .path(id)
            .iter()
            .map(|step| format!("{{{}}}{}[{}]", step.name.uri, step.name.local, step.ordinal))
            .collect::<Vec<_>>()
            .join("/");
        assert_eq!(current.as_ref(), format!("{key}/{legacy}"));
    }
    assert!(
        original
            .node_ids
            .iter()
            .any(|id| id.ends_with("{urn:inner}Item[1]"))
    );
    assert!(
        original
            .node_ids
            .iter()
            .any(|id| id.ends_with("Attribute[11]"))
    );

    let proof = SourceProof {
        identity: "synthetic".into(),
        snapshot: "immutable-two".into(),
        inputs: BTreeMap::new(),
    };
    let cached =
        |facts| SourceDocument::cached(key, "fixture.xml", xml.as_bytes(), proof.clone(), facts);
    let restored = cached(original.facts.clone()).unwrap();
    let extracted =
        SourceDocument::descriptor(key, "fixture.xml", xml.as_bytes(), proof.clone()).unwrap();
    assert_eq!(restored.node_ids, extracted.node_ids);
    assert_eq!(restored.facts, extracted.facts);
    assert_eq!(
        restored.targets(restored.object),
        extracted.targets(extracted.object)
    );

    let object_id = &original.node_ids[original.object];
    for field in ["span", "tag", "parent"] {
        let mut broken = original.facts.clone();
        let object = broken.fragments.get_mut(object_id).unwrap();
        match field {
            "span" => object.origin.span = None,
            "tag" => std::sync::Arc::make_mut(&mut object.tag).local = "Wrong".into(),
            "parent" => object.parent = None,
            _ => unreachable!(),
        }
        assert!(
            matches!(cached(broken), Err(Error::Invalid(reason)) if reason.contains("source spans"))
        );
    }
}

#[test]
fn source_markup_hints_follow_retained_source_fragments() {
    let source = document(
        "Catalog.Owner",
        "<MetaDataObject><!--Catalog.Header--><Catalog><Properties><Name>Owner</Name><Comment><!--Catalog.Kept--></Comment><Custom><?hint Catalog.Pruned?></Custom></Properties><ChildObjects><Attribute><Properties><Name>Dropped</Name><Type><Type>CatalogRef.Child</Type></Type></Properties></Attribute></ChildObjects></Catalog></MetaDataObject>",
    );
    assert_eq!(
        source.targets(source.view.xml.root),
        &BTreeSet::from([
            "Catalog.Header".into(),
            "Catalog.Kept".into(),
            "Catalog.Pruned".into(),
            "Catalog.Child".into()
        ])
    );
    let plan = crate::projection::compile(
        &source,
        "Catalog.Owner",
        &crate::stubs::StubSpec {
            finalized: true,
            ..Default::default()
        },
        &BTreeSet::new(),
    )
    .unwrap();
    let targets: BTreeSet<_> = plan
        .references
        .iter()
        .map(|reference| reference.target.key())
        .collect();
    assert_eq!(
        targets,
        BTreeSet::from(["Catalog.Header".into(), "Catalog.Kept".into()])
    );
    let bytes = crate::source_edits::emit(source.view.xml.source.as_bytes(), &plan.edits).unwrap();
    assert!(
        String::from_utf8(bytes)
            .unwrap()
            .contains("<!--Catalog.Kept-->")
    );
}

#[test]
fn sidecar_facts_keep_their_file_origin_and_expanded_type() {
    let xml = format!(
        r#"<f:Form xmlns:f="urn:form" xmlns:t="{CORE}" xmlns:c="{CURRENT}"><f:Type><t:Type>c:CatalogRef.Target</t:Type></f:Type></f:Form>"#
    );
    let proof = SourceProof {
        identity: "synthetic".into(),
        snapshot: "fixed".into(),
        inputs: BTreeMap::new(),
    };
    let first = SourceDocument::sidecar(
        "Catalog.Owner",
        "Forms/First.xml",
        xml.as_bytes(),
        proof.clone(),
    )
    .unwrap();
    let second =
        SourceDocument::sidecar("Catalog.Owner", "Forms/Second.xml", xml.as_bytes(), proof)
            .unwrap();
    first.facts.validate().unwrap();
    second.facts.validate().unwrap();
    assert!(
        first
            .facts
            .references
            .iter()
            .any(|reference| reference.target.key() == "Catalog.Target"
                && reference.strength == ReferenceStrength::Structured)
    );
    assert!(
        first
            .facts
            .fragments
            .keys()
            .all(|id| !second.facts.fragments.contains_key(id))
    );
    assert!(
        first
            .facts
            .fragments
            .values()
            .all(|fragment| fragment.origin.path.as_ref() == "Forms/First.xml")
    );
}
#[test]
fn unqualified_legacy_form_type_is_a_lexical_hint_not_a_fake_namespace_binding() {
    let xml = b"<Form><Attributes><Attribute><Type><Type>cfg:CatalogRef.Target</Type></Type></Attribute></Attributes></Form>";
    let doc = SourceDocument::sidecar(
        "CommonForm.List",
        "CommonForms/List/Ext/Form.xml",
        xml,
        SourceProof {
            identity: "fixture".into(),
            snapshot: "fixed".into(),
            inputs: BTreeMap::new(),
        },
    )
    .unwrap();
    assert!(
        doc.facts
            .references
            .iter()
            .any(|reference| reference.target.key() == "Catalog.Target"
                && reference.strength == ReferenceStrength::Lexical
                && reference.missing == MissingPolicy::Hint)
    );
    assert!(
        !doc.facts
            .references
            .iter()
            .any(|reference| reference.target.key() == "Catalog.Target"
                && reference.strength == ReferenceStrength::Structured)
    );
}
#[test]
fn legacy_form_core_type_hint_does_not_relax_namespace_aware_xml() {
    let proof = SourceProof {
        identity: "synthetic".into(),
        snapshot: "fixed".into(),
        inputs: BTreeMap::new(),
    };
    let legacy = format!(
        r#"<Form xmlns:v="{CORE}"><Attributes><Attribute><Type><v:Type>cfg:CatalogRef.Target</v:Type></Type></Attribute></Attributes></Form>"#
    );
    let read = |xml: &str| {
        SourceDocument::sidecar(
            "Catalog.Owner",
            "Forms/Main/Ext/Form.xml",
            xml.as_bytes(),
            proof.clone(),
        )
    };
    let doc = read(&legacy).unwrap();
    assert!(
        doc.facts
            .references
            .iter()
            .any(|r| r.target.key() == "Catalog.Target"
                && r.strength == ReferenceStrength::Lexical
                && r.missing == MissingPolicy::Hint)
    );
    assert!(
        !doc.facts
            .references
            .iter()
            .any(|r| r.strength == ReferenceStrength::Structured)
    );
    assert!(doc.facts.coverage_notes.iter().any(|n| n.facet == Facet::References && matches!(n.coverage, Coverage::Unsupported(_))));
    // A real namespace-aware form, an arbitrary missing prefix, malformed QName
    // and undeclared element/attribute prefixes must still fail.
    for xml in [
        legacy.replace("<Form ", "<Form xmlns=\"urn:form\" "),
        legacy.replace("cfg:", "missing:"),
        legacy.replace("cfg:CatalogRef.Target", "cfg:CatalogRef.Target:Other"),
        legacy.replace("<Attribute>", "<Attribute missing:value=\"x\">"),
        legacy
            .replace("<v:Type>", "<missing:Type>")
            .replace("</v:Type>", "</missing:Type>"),
    ] {
        assert!(read(&xml).is_err(), "accepted {xml}");
    }
    // Explicit bindings retain their meaning even in the legacy form container.
    let bound =
        read(&legacy.replace("<Form ", &format!("<Form xmlns:cfg=\"{CURRENT}\" "))).unwrap();
    assert!(
        bound
            .facts
            .references
            .iter()
            .any(|r| r.target.key() == "Catalog.Target"
                && r.strength == ReferenceStrength::Structured)
    );
    let rebound = read(&legacy.replace("<Form ", "<Form xmlns:cfg=\"urn:other\" ")).unwrap();
    assert!(
        !rebound
            .facts
            .references
            .iter()
            .any(|r| r.strength == ReferenceStrength::Structured)
    );
}
#[test]
fn foreign_child_names_do_not_become_metadata_fields_or_retained_members() {
    let xml = format!(
        r#"<MetaDataObject xmlns="{MD}" xmlns:f="urn:foreign"><InformationRegister><Properties><Name>Register</Name></Properties><ChildObjects><f:Resource><Properties><Name>Ghost</Name></Properties></f:Resource><Resource><Properties><Name>Real</Name></Properties></Resource></ChildObjects></InformationRegister></MetaDataObject>"#
    );
    let doc = document("InformationRegister.Register", &xml);
    let facts = doc.structural_facts().unwrap();
    assert_eq!(
        facts
            .members
            .iter()
            .map(|m| m.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Real"]
    );
    let selection = crate::stubs::StubSpec {
        finalized: true,
        members: BTreeSet::from([
            ("Resource".into(), "Ghost".into()),
            ("Resource".into(), "Real".into()),
        ]),
        ..Default::default()
    };
    let plan = crate::projection::compile(
        &doc,
        "InformationRegister.Register",
        &selection,
        &BTreeSet::new(),
    )
    .unwrap();
    let output =
        String::from_utf8(crate::source_edits::emit(xml.as_bytes(), &plan.edits).unwrap()).unwrap();
    assert!(!output.contains("Ghost"));
    assert!(output.contains("Real"));
}
#[test]
fn malformed_known_qname_slots_cannot_degrade_to_lexical_hints() {
    for body in [
        format!(
            r#"<r:Item xmlns:r="{READABLE}" xmlns:i="{XSI}" i:type="missing:MDObjectRef">Catalog.Absent</r:Item>"#
        ),
        format!(
            r#"<Picture xmlns:i="{XSI}" xmlns:u="{UI}" i:type="u:Picture" ref="missing:Camera"/>"#
        ),
    ] {
        let xml = format!(
            "<MetaDataObject><Catalog><Properties><Name>Owner</Name><Value>{body}</Value></Properties></Catalog></MetaDataObject>"
        );
        assert!(
            SourceDocument::descriptor(
                "Catalog.Owner",
                "broken-qname.xml",
                xml.as_bytes(),
                SourceProof {
                    identity: "fixture".into(),
                    snapshot: "fixed".into(),
                    inputs: BTreeMap::new()
                }
            )
            .is_err()
        );
    }
}
#[test]
fn property_candidates_preserve_lexical_source_order() {
    let doc = document(
        "Catalog.Owner",
        "<MetaDataObject><Catalog><Properties><Name>Owner</Name><Owners>Catalog.Zed Catalog.Alpha Catalog.Zed</Owners><Other>Catalog.Decoy</Other></Properties></Catalog></MetaDataObject>",
    );
    let props = doc.view.child(doc.object, "Properties").unwrap();
    let owners = doc.view.child(props, "Owners").unwrap();
    let targets: Vec<_> = doc.references_in(owners).map(|r| r.target.key()).collect();
    assert_eq!(targets, vec!["Catalog.Zed", "Catalog.Alpha"]);
    let mixed = document(
        "Catalog.Owner",
        "<MetaDataObject><Catalog><Properties><Name>Owner</Name><Owners><Item>Catalog.Zed</Item><!--Catalog.Middle--> Catalog.Alpha<?hint Catalog.Last?></Owners></Properties></Catalog></MetaDataObject>",
    );
    let owners = mixed
        .view
        .child(
            mixed.view.child(mixed.object, "Properties").unwrap(),
            "Owners",
        )
        .unwrap();
    assert_eq!(
        mixed
            .ordered_candidates(owners)
            .iter()
            .map(|r| r.key())
            .collect::<Vec<_>>(),
        vec![
            "Catalog.Zed",
            "Catalog.Middle",
            "Catalog.Alpha",
            "Catalog.Last"
        ]
    );
}
#[test]
fn defined_type_expanded_names_default_qname_namespace_and_rebinding() {
    let xml = format!(
        r#"<m:MetaDataObject xmlns:m="{MD}" xmlns:v="{CORE}" xmlns:c="{CURRENT}">
      <m:DefinedType uuid="source-uuid"><m:Properties><m:Name>RefSet</m:Name><m:Type>
        <v:Type>c:CatalogRef.First</v:Type><v:Type xmlns="{CURRENT}">DocumentRef.Second</v:Type>
        <v:Type xmlns:c="urn:unrelated">c:CatalogRef.NotStructured</v:Type>
      </m:Type></m:Properties></m:DefinedType></m:MetaDataObject>"#
    );
    let first = document("DefinedType.RefSet", &xml);
    assert_eq!(
        first.structural_facts().unwrap().targets,
        BTreeSet::from(["Catalog.First".into(), "Document.Second".into()])
    );
    let aliases = xml
        .replace("xmlns:m=", "xmlns:metadata=")
        .replace("<m:", "<metadata:")
        .replace("</m:", "</metadata:")
        .replace("xmlns:c=", "xmlns:config=")
        .replace("c:Catalog", "config:Catalog");
    let second = document("DefinedType.RefSet", &aliases);
    assert_eq!(
        first.structural_facts().unwrap(),
        second.structural_facts().unwrap()
    );
    assert_eq!(
        first.facts.fragments.keys().collect::<Vec<_>>(),
        second.facts.fragments.keys().collect::<Vec<_>>()
    );
    assert!(
        first
            .facts
            .references
            .iter()
            .any(|r| r.target.key() == "Catalog.NotStructured"
                && r.strength == ReferenceStrength::Lexical
                && r.missing == MissingPolicy::Hint)
    );
    assert!(
        !first
            .facts
            .references
            .iter()
            .any(|r| r.target.key() == "Catalog.NotStructured"
                && r.strength == ReferenceStrength::Structured)
    );
}
#[test]
fn declarations_columns_and_property_items_have_separate_origins() {
    let xml = format!(
        r#"<MetaDataObject xmlns="{MD}" xmlns:r="{READABLE}" xmlns:i="{XSI}">
      <Document uuid="doc-uuid"><Properties><Name>Carrier</Name><RegisterRecords>
       <r:Item i:type="r:MDObjectRef">AccumulationRegister.A</r:Item>
       <r:Item i:type="r:MDObjectRef">AccumulationRegister.B</r:Item>
      </RegisterRecords></Properties><ChildObjects>
        <Attribute uuid="field-uuid"><Properties><Name>Same</Name></Properties></Attribute>
        <Form>Same</Form><Template>Same</Template>
        <TabularSection><Properties><Name>Left</Name></Properties><ChildObjects><Attribute uuid="left-uuid"><Properties><Name>Column</Name></Properties></Attribute></ChildObjects></TabularSection>
        <TabularSection><Properties><Name>Right</Name></Properties><ChildObjects><Attribute uuid="right-uuid"><Properties><Name>Column</Name></Properties></Attribute></ChildObjects></TabularSection>
      </ChildObjects></Document></MetaDataObject>"#
    );
    let doc = document("Document.Carrier", &xml);
    let same: Vec<_> = doc
        .facts
        .fragments
        .values()
        .filter(|f| {
            f.name == "Same" && matches!(f.kind, FragmentKind::Field | FragmentKind::Declaration)
        })
        .collect();
    assert_eq!(same.len(), 3);
    assert_eq!(
        same.iter()
            .filter(|f| f.kind == FragmentKind::Field)
            .count(),
        1
    );
    let columns: Vec<_> = doc
        .facts
        .fragments
        .values()
        .filter(|f| f.kind == FragmentKind::Column && f.name == "Column")
        .collect();
    assert_eq!(columns.len(), 2);
    assert_ne!(columns[0].id, columns[1].id);
    assert_ne!(columns[0].parent, columns[1].parent);
    let links: Vec<_> = doc
        .facts
        .references
        .iter()
        .filter(|r| r.strength == ReferenceStrength::Structured)
        .collect();
    assert_eq!(links.len(), 2);
    assert_eq!(
        links
            .iter()
            .map(|r| doc.facts.fragments[&r.fragment].ordinal)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([0, 1])
    );
    assert!(
        links
            .iter()
            .all(|r| doc.facts.fragments[&r.fragment].kind == FragmentKind::PropertyItem)
    );
    assert_eq!(
        doc.structural_facts().unwrap().members.len(),
        3,
        "declarations do not become fields"
    );
    let mut broken = doc.facts;
    broken.proof.inputs.clear();
    assert!(
        broken.validate().is_err(),
        "cached origin cannot outlive its source proof"
    );
}
#[test]
fn metadata_path_lexer_has_independent_expected_targets() {
    let text = "Catalog.A.Attribute.X CatalogRef.B CatalogTabularSectionRow.C.Lines Catalog.D.StandardCommand.OpenByValue.Preset Catalog.E.EmptyRef Catalog.F.Predef bad.Catalog.G Characteristic.H style:Accent";
    let actual = references(text);
    let rows: Vec<_> = actual
        .iter()
        .map(|r| {
            (
                r.key(),
                r.member.clone(),
                r.member_name.clone(),
                r.predefined.clone(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            (
                "Catalog.A".into(),
                "Attribute".into(),
                "X".into(),
                "".into()
            ),
            ("Catalog.B".into(), "".into(), "".into(), "".into()),
            (
                "Catalog.C".into(),
                "TabularSection".into(),
                "Lines".into(),
                "".into()
            ),
            ("Catalog.D".into(), "*".into(), "Preset".into(), "".into()),
            (
                "Catalog.D".into(),
                "StandardCommand".into(),
                "OpenByValue".into(),
                "".into()
            ),
            ("Catalog.E".into(), "".into(), "".into(), "".into()),
            ("Catalog.F".into(), "".into(), "".into(), "Predef".into()),
            (
                "ChartOfCharacteristicTypes.H".into(),
                "".into(),
                "".into(),
                "".into()
            ),
            ("StyleItem.Accent".into(), "".into(), "".into(), "".into()),
        ]
    );
}
#[test]
fn required_corrupt_namespace_and_malformed_xml_are_not_empty_facts() {
    for xml in [
        "<MetaDataObject>",
        "<p:MetaDataObject><p:DefinedType/></p:MetaDataObject>",
        "<MetaDataObject xmlns='urn:wrong'><DefinedType/></MetaDataObject>",
    ] {
        assert!(
            SourceDocument::descriptor(
                "DefinedType.RefSet",
                "broken.xml",
                xml.as_bytes(),
                SourceProof {
                    identity: "test".into(),
                    snapshot: "one".into(),
                    inputs: BTreeMap::new(),
                }
            )
            .is_err()
        );
    }
}
#[test]
fn plain_required_property_is_structured_but_attribute_hint_is_separate() {
    let xml = format!(
        r#"<MetaDataObject xmlns="{MD}" xmlns:i="{XSI}" xmlns:c="{CURRENT}">
      <FunctionalOption uuid="flag"><Properties><Name>UseFeature</Name>
      <Location>Constant.FeatureFlag</Location><Visual i:type="c:CatalogRef.Decoration"/>
      </Properties></FunctionalOption></MetaDataObject>"#
    );
    let doc = document("FunctionalOption.UseFeature", &xml);
    assert!(
        doc.facts
            .references
            .iter()
            .any(|r| r.target.key() == "Constant.FeatureFlag"
                && r.strength == ReferenceStrength::Structured
                && r.missing == MissingPolicy::Diagnose)
    );
    let attribute = doc
        .facts
        .references
        .iter()
        .find(|r| r.target.key() == "Catalog.Decoration")
        .unwrap();
    assert_eq!(
        attribute.surface,
        ReferenceSurface::Attribute(ExpandedName {
            uri: XSI.into(),
            local: "type".into()
        })
    );
    assert_eq!(attribute.missing, MissingPolicy::Hint);
}
#[test]
fn unknown_nonmetadata_datatype_has_coverage_but_corrupt_metadata_qname_fails() {
    let input = format!(
        r#"<MetaDataObject xmlns="{MD}" xmlns:v="{CORE}"><DefinedType uuid="original"><Properties><Name>Probe</Name><Type><v:Type>xs:string</v:Type></Type></Properties></DefinedType></MetaDataObject>"#
    );
    let doc = document("DefinedType.Probe", &input);
    assert_eq!(doc.facts.coverage_notes.len(), 1);
    assert!(doc.structural_facts().unwrap().targets.is_empty());
    assert!(
        !doc.facts.fragments.is_empty(),
        "unsupported datatype is recorded, not empty facts"
    );
    let corrupt_metadata = input.replace("xs:string", "missing:CatalogRef.Required");
    assert!(
        SourceDocument::descriptor(
            "DefinedType.Probe",
            "broken.xml",
            corrupt_metadata.as_bytes(),
            SourceProof {
                identity: "synthetic".into(),
                snapshot: "one".into(),
                inputs: BTreeMap::new()
            }
        )
        .is_err()
    );
}
#[test]
fn legacy_nested_metadata_type_nodes_keep_composite_field_targets() {
    let xml = format!(
        r#"<MetaDataObject xmlns="{MD}" xmlns:c="{CURRENT}"><Document uuid="original"><Properties><Name>Carrier</Name></Properties><ChildObjects><Attribute uuid="field"><Properties><Name>Reference</Name><Type><Type>c:DefinedType.Composite</Type><Type>c:CatalogRef.Entities</Type></Type></Properties></Attribute></ChildObjects></Document></MetaDataObject>"#
    );
    let source = document("Document.Carrier", &xml);
    let facts = source.structural_facts().unwrap();
    assert_eq!(
        facts.members[0].types,
        BTreeSet::from(["DefinedType.Composite".into(), "Catalog.Entities".into()])
    );
}

use crate::{package_plan::*, source_facts::*};
use std::collections::{BTreeMap, BTreeSet};

fn resource_dependency() -> CompiledPackagePlan {
    let path = "CommonPictures.Icon.xml";
    let object = "CommonPicture.Icon";
    let hash = "e4".repeat(32);
    let source = SourceProof {
        identity: "source-A".into(),
        snapshot: "snapshot-1".into(),
        inputs: BTreeMap::from([
            (
                path.into(),
                InputObservation::Content {
                    sha256: hash.clone(),
                },
            ),
            (
                "CommonPictures/Icon/Ext/Picture/manifest.xml".into(),
                InputObservation::Absent,
            ),
        ]),
    };
    let stamp = PlanStamp {
        generation: "selection-1".into(),
        proof_digest: source.digest().unwrap(),
        readers: FactVersions::current(),
        rule_profile: RULE_PROFILE.into(),
    };
    CompiledPackagePlan {
        projection: ProjectionPlan {
            selected: BTreeSet::new(),
            dependencies: BTreeSet::from([object.into()]),
            stamp: stamp.clone(),
            objects: BTreeMap::from([(
                object.into(),
                ObjectProjection {
                    content: ContentMode::Full,
                    role: SelectionRole::Dependency,
                    descriptor: path.into(),
                    retained: BTreeSet::new(),
                    declarations: BTreeMap::new(),
                    identity: BTreeMap::from([(
                        "uuid".into(),
                        "c56a4180-65aa-42ec-a945-5fd21dec0538".into(),
                    )]),
                },
            )]),
            files: BTreeMap::from([(
                path.into(),
                FileProjection {
                    owner: object.into(),
                    content: ContentMode::Full,
                    operation: FileOperation::Copy {
                        source: path.into(),
                    },
                    expectation: FileExpectation::Exact { sha256: hash },
                },
            )]),
        },
        merge: MergePlan {
            stamp,
            children: BTreeMap::new(),
            order: vec![object.into()],
            objects: BTreeMap::from([(
                object.into(),
                MergeObjectPlan {
                    action: MergeAction::MergePrioritizingMainConfiguration,
                    protected: vec![],
                },
            )]),
        },
        source,
    }
}

#[test]
fn full_resource_does_not_replace_receiver_and_selected_content_does() {
    let mut plan = resource_dependency();
    plan.validate().unwrap();
    plan.merge
        .objects
        .get_mut("CommonPicture.Icon")
        .unwrap()
        .action = MergeAction::GetFromSecondConfiguration;
    assert!(
        plan.validate().is_err(),
        "dependency payload must not enable replacement"
    );
    plan.projection
        .objects
        .get_mut("CommonPicture.Icon")
        .unwrap()
        .role = SelectionRole::Selected;
    plan.projection.selected.insert("CommonPicture.Icon".into());
    plan.projection.dependencies.remove("CommonPicture.Icon");
    plan.validate().unwrap();
    plan.projection
        .objects
        .get_mut("CommonPicture.Icon")
        .unwrap()
        .content = ContentMode::Projected;
    assert!(
        plan.validate().is_err(),
        "explicit full selection must keep its full bytes"
    );
}

#[test]
fn proof_and_generation_cover_unemitted_inputs() {
    let mut plan = resource_dependency();
    plan.source.inputs.insert(
        "Catalogs/Receiver/Ext/ObjectModule.bsl".into(),
        InputObservation::Content {
            sha256: "ab".repeat(32),
        },
    );
    assert!(
        plan.validate().is_err(),
        "omitted-module mutation invalidates both plans"
    );
    let digest = plan.source.digest().unwrap();
    plan.projection.stamp.proof_digest = digest.clone();
    plan.merge.stamp.proof_digest = digest;
    plan.validate().unwrap();
    plan.merge.stamp.generation = "different-selection".into();
    assert!(plan.validate().is_err());
}

#[test]
fn no_extra_merge_actions_files_or_source_substitution() {
    let good = resource_dependency();
    let mut plan = good.clone();
    plan.merge.objects.insert(
        "Catalog.Unrelated".into(),
        MergeObjectPlan {
            action: MergeAction::GetFromSecondConfiguration,
            protected: vec![],
        },
    );
    assert!(plan.validate().is_err());
    let mut plan = good.clone();
    plan.projection
        .files
        .values_mut()
        .next()
        .unwrap()
        .expectation = FileExpectation::Exact {
        sha256: "00".repeat(32),
    };
    assert!(plan.validate().is_err());
    let mut plan = good;
    let file = plan.projection.files.values_mut().next().unwrap();
    file.operation = FileOperation::Copy {
        source: "../outside.xml".into(),
    };
    assert!(plan.validate().is_err());
}

#[test]
fn schema_reader_and_rule_versions_are_separate() {
    let original = FactVersions::current();
    let mut metadata_changed = original.clone();
    metadata_changed
        .readers
        .insert(Facet::Metadata, "future-reader-test-only".into());
    assert!(!original.compatible(&metadata_changed, Facet::Metadata));
    assert!(original.compatible(&metadata_changed, Facet::Registrars));
    let mut schema_changed = original.clone();
    schema_changed.schema += 1;
    assert!(!original.compatible(&schema_changed, Facet::Registrars));
    let empty_readers = FactVersions {
        schema: FACT_SCHEMA,
        readers: BTreeMap::new(),
    };
    assert!(!empty_readers.compatible(&empty_readers, Facet::Metadata));
    let mut plan = resource_dependency();
    let before = plan.projection.stamp.readers.clone();
    plan.projection.stamp.rule_profile = "rule-only-change".into();
    assert_eq!(before, plan.projection.stamp.readers);
}

#[test]
fn conflicting_presence_content_and_listing_are_rejected() {
    let mut proof = SourceProof {
        identity: "local".into(),
        snapshot: "one".into(),
        inputs: BTreeMap::new(),
    };
    proof
        .observe("missing.xml".into(), InputObservation::Absent)
        .unwrap();
    assert!(
        proof
            .observe(
                "missing.xml".into(),
                InputObservation::Content {
                    sha256: "aa".repeat(32)
                }
            )
            .is_err()
    );
    proof
        .observe(
            "Catalogs".into(),
            InputObservation::Directory { entries: vec![] },
        )
        .unwrap();
    assert!(
        proof
            .observe(
                "Catalogs".into(),
                InputObservation::Directory {
                    entries: vec![DirectoryEntry {
                        name: "New.xml".into(),
                        directory: false
                    }]
                }
            )
            .is_err()
    );
}
#[test]
fn service_role_is_independent_of_full_copy_and_cannot_replace_a_dependency_role() {
    let mut plan = resource_dependency();
    plan.projection
        .objects
        .get_mut("CommonPicture.Icon")
        .unwrap()
        .role = SelectionRole::Service;
    plan.merge
        .objects
        .get_mut("CommonPicture.Icon")
        .unwrap()
        .action = MergeAction::DoNotMerge;
    assert!(
        plan.validate().is_err(),
        "a frozen dependency cannot silently become a service"
    );
    // Generic contract: the concrete CF profile chooses its service identifiers.
    plan.projection.dependencies.clear();
    plan.validate().unwrap();
    plan.projection.selected.insert("CommonPicture.Icon".into());
    assert!(
        plan.validate().is_err(),
        "an explicit selection cannot lose transfer priority"
    );
}

#[test]
fn child_rules_require_the_exact_retained_declaration_owner_key_and_order() {
    let mut plan = resource_dependency();
    let owner = "CommonPicture.Icon";
    let fragment = "source/decl/Main";
    let key = format!("{owner}.Form.Main");
    plan.projection
        .objects
        .get_mut(owner)
        .unwrap()
        .retained
        .insert(fragment.into());
    plan.projection
        .objects
        .get_mut(owner)
        .unwrap()
        .declarations
        .insert(fragment.into(), ("Form".into(), "Main".into()));
    plan.merge.children.insert(
        key.clone(),
        MergeChildPlan {
            owner: owner.into(),
            fragment: fragment.into(),
            tag: "Form".into(),
            name: "Main".into(),
            protected: vec!["Module".into()],
        },
    );
    plan.merge.order.push(key.clone());
    plan.validate().unwrap();
    let mut substituted = plan.clone();
    let mut child = substituted.merge.children.remove(&key).unwrap();
    child.name = "Other".into();
    let other = format!("{owner}.Form.Other");
    substituted.merge.children.insert(other.clone(), child);
    substituted
        .merge
        .order
        .iter_mut()
        .filter(|entry| **entry == key)
        .for_each(|entry| *entry = other.clone());
    assert!(
        substituted.validate().is_err(),
        "self-consistent name/key/order cannot change the source declaration identity"
    );
    let mut corrupt = plan.clone();
    corrupt.merge.children.get_mut(&key).unwrap().owner = "Catalog.Missing".into();
    assert!(corrupt.validate().is_err());
    let mut corrupt = plan.clone();
    corrupt.merge.children.get_mut(&key).unwrap().fragment = "source/unretained".into();
    assert!(corrupt.validate().is_err());
    let mut corrupt = plan.clone();
    corrupt.merge.children.get_mut(&key).unwrap().tag = "Attribute".into();
    assert!(corrupt.validate().is_err());
    let mut corrupt = plan.clone();
    corrupt.merge.children.get_mut(&key).unwrap().name = "Other".into();
    assert!(corrupt.validate().is_err());
    let mut corrupt = plan.clone();
    corrupt.merge.order.push(key.clone());
    assert!(corrupt.validate().is_err());
    let mut corrupt = plan.clone();
    corrupt.merge.order.pop();
    assert!(corrupt.validate().is_err());
    let mut corrupt = plan.clone();
    corrupt.projection.dependencies.clear();
    corrupt.projection.selected.insert(owner.into());
    corrupt.projection.objects.get_mut(owner).unwrap().role = SelectionRole::Selected;
    corrupt.merge.objects.get_mut(owner).unwrap().action = MergeAction::GetFromSecondConfiguration;
    assert!(corrupt.validate().is_err());
}

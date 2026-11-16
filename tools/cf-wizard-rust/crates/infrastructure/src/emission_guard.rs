//! Compare immutable expectations to actual output; never infer policy from output.
use crate::{canonical::sha256, projection::semantic_value, source_xml::SourceXml};
use cf_domain::{
    Error, Result,
    package_plan::{CompiledPackagePlan, FileExpectation, XmlValue},
};
use std::collections::BTreeSet;

pub(crate) fn descriptor(expected: &XmlValue, actual: &[u8], path: &str) -> Result<()> {
    let view = SourceXml::parse(actual)?;
    if semantic_value(&view, view.xml.root)? != *expected {
        return Err(Error::Invalid(format!(
            "Выданный XML не соответствует плану: {path}"
        )));
    }
    Ok(())
}

pub(crate) fn settings(plan: &CompiledPackagePlan, actual: &[u8]) -> Result<()> {
    plan.validate()?;
    merge_settings(&plan.merge, actual)
}
fn merge_settings(plan: &cf_domain::package_plan::MergePlan, actual: &[u8]) -> Result<()> {
    use cf_domain::{
        package_plan::{MergeAction, XmlPart},
        source_facts::ExpandedName,
    };
    const NS: &str = "http://v8.1c.ru/8.3/config/merge/settings";
    let node = |name: &str, attributes: Vec<(&str, String)>, parts: Vec<XmlPart>| XmlValue {
        name: ExpandedName {
            uri: NS.into(),
            local: name.into(),
        },
        attributes: attributes
            .into_iter()
            .map(|(name, value)| {
                (
                    ExpandedName {
                        uri: String::new(),
                        local: name.into(),
                    },
                    value,
                )
            })
            .collect(),
        parts,
    };
    let rule = |value: &str| {
        XmlPart::Element(node("MergeRule", vec![], vec![XmlPart::Text(value.into())]))
    };
    let mut objects = vec![];
    for key in &plan.order {
        let (action, properties) = if let Some(object) = plan.objects.get(key) {
            (object.action, &object.protected)
        } else {
            (
                MergeAction::MergePrioritizingMainConfiguration,
                &plan.children[key].protected,
            )
        };
        // Explicit contract literals, independent of writer Debug formatting.
        let action = match action {
            MergeAction::GetFromSecondConfiguration => "GetFromSecondConfiguration",
            MergeAction::MergePrioritizingMainConfiguration => "MergePrioritizingMainConfiguration",
            MergeAction::DoNotMerge => "DoNotMerge",
        };
        let mut parts = vec![rule(action)];
        if !properties.is_empty() {
            let properties = properties
                .iter()
                .map(|name| {
                    XmlPart::Element(node(
                        "Property",
                        vec![("name", name.clone())],
                        vec![rule("DoNotMerge")],
                    ))
                })
                .collect();
            parts.push(XmlPart::Element(node("Properties", vec![], properties)));
        }
        objects.push(XmlPart::Element(node(
            "Object",
            vec![("fullNameInSecondConfiguration", key.clone())],
            parts,
        )));
    }
    let expected = node(
        "Settings",
        vec![
            ("platformVersion", "8.3.11".into()),
            ("version", "1.2".into()),
        ],
        vec![
            XmlPart::Element(node(
                "Parameters",
                vec![],
                vec![
                    XmlPart::Element(node(
                        "ConfigurationsRelation",
                        vec![],
                        vec![XmlPart::Text("ConfigurationsNotRelated".into())],
                    )),
                    XmlPart::Element(node(
                        "AllowMainConfigurationObjectDeletion",
                        vec![],
                        vec![XmlPart::Text("false".into())],
                    )),
                ],
            )),
            XmlPart::Element(node("Objects", vec![], objects)),
        ],
    );
    descriptor(&expected, actual, "merge-settings.xml")
}

pub(crate) fn package(
    plan: &CompiledPackagePlan,
    inventory: &BTreeSet<String>,
    mut read: impl FnMut(&str) -> Result<Vec<u8>>,
) -> Result<()> {
    plan.validate()?;
    let expected: BTreeSet<_> = plan.projection.files.keys().cloned().collect();
    if inventory != &expected {
        let missing: Vec<_> = expected.difference(inventory).collect();
        let extra: Vec<_> = inventory.difference(&expected).collect();
        return Err(Error::Invalid(format!(
            "Состав выдачи не соответствует плану; отсутствуют: {missing:?}; лишние: {extra:?}"
        )));
    }
    for (path, file) in &plan.projection.files {
        let actual = read(path)?;
        match &file.expectation {
            FileExpectation::Exact { sha256: expected } if sha256(&actual) != *expected => {
                return Err(Error::Invalid(format!(
                    "Содержимое выданного файла отличается от плана: {path}"
                )));
            }
            FileExpectation::Xml {
                root,
                sha256: expected,
            } => {
                if sha256(&actual) != *expected {
                    return Err(Error::Invalid(format!(
                        "Байты выдачи отличаются от плана: {path}"
                    )));
                }
                descriptor(root, &actual, path)?;
            }
            FileExpectation::Exact { .. } => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod settings_tests {
    use super::*;
    use cf_domain::{
        package_plan::{MergeAction, MergeObjectPlan, MergePlan, PlanStamp},
        source_facts::FactVersions,
    };
    use std::collections::BTreeMap;
    #[test]
    fn independent_settings_expectation_rejects_policy_value_inventory_and_order_corruption() {
        let plan = MergePlan {
            stamp: PlanStamp {
                generation: "fixture".into(),
                proof_digest: "fixture".into(),
                readers: FactVersions::current(),
                rule_profile: "fixture".into(),
            },
            objects: BTreeMap::from([
                (
                    "Catalog.Selected".into(),
                    MergeObjectPlan {
                        action: MergeAction::GetFromSecondConfiguration,
                        protected: vec![],
                    },
                ),
                (
                    "Catalog.Dependency".into(),
                    MergeObjectPlan {
                        action: MergeAction::MergePrioritizingMainConfiguration,
                        protected: vec!["ObjectModule".into()],
                    },
                ),
            ]),
            children: BTreeMap::new(),
            order: vec!["Catalog.Selected".into(), "Catalog.Dependency".into()],
        };
        // Literal fixture is independent of the production settings serializer.
        let xml = r#"<Settings xmlns="http://v8.1c.ru/8.3/config/merge/settings" version="1.2" platformVersion="8.3.11"><Parameters><ConfigurationsRelation>ConfigurationsNotRelated</ConfigurationsRelation><AllowMainConfigurationObjectDeletion>false</AllowMainConfigurationObjectDeletion></Parameters><Objects><Object fullNameInSecondConfiguration="Catalog.Selected"><MergeRule>GetFromSecondConfiguration</MergeRule></Object><Object fullNameInSecondConfiguration="Catalog.Dependency"><MergeRule>MergePrioritizingMainConfiguration</MergeRule><Properties><Property name="ObjectModule"><MergeRule>DoNotMerge</MergeRule></Property></Properties></Object></Objects></Settings>"#;
        merge_settings(&plan, xml.as_bytes()).unwrap();
        for corrupt in [
            xml.replace("GetFromSecondConfiguration", "DoNotMerge"),
            xml.replace("ObjectModule", "ManagerModule"),
            xml.replace("Catalog.Dependency", "Catalog.Unrelated"),
            xml.replace("false", "true"),
            xml.replace("8.3.11", "8.3.27"),
            xml.replace("<Objects>", "<Objects><Extra/>"),
            xml.replace("config/merge/settings", "wrong"),
            xml.replace("Catalog.Selected", "Catalog.Dependency"),
        ] {
            assert!(merge_settings(&plan, corrupt.as_bytes()).is_err());
        }
        let mut reordered = plan.clone();
        reordered.order.reverse();
        assert!(merge_settings(&reordered, xml.as_bytes()).is_err());
    }
}

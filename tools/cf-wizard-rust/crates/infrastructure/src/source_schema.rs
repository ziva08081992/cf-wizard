//! Finite source value vocabulary, independent of projection/minimum-field rules.
//! These properties contain metadata references (or a reference-bearing type).
pub(crate) fn descriptor(key: &str) -> Option<String> {
    let (kind, name) = key.split_once('.')?;
    crate::kinds::TYPE_FOLDERS
        .iter()
        .find_map(|(label, folder)| (*label == kind).then(|| format!("{folder}/{name}.xml")))
}
pub(crate) const REFERENCE_PROPERTIES: &[(&str, &str)] = &[
    ("FunctionalOption", "Location"),
    ("BusinessProcess", "Task"),
    ("AccountingRegister", "ChartOfAccounts"),
    ("CalculationRegister", "ChartOfCalculationTypes"),
    ("CalculationRegister", "Schedule"),
    ("CalculationRegister", "ScheduleDate"),
    ("CalculationRegister", "ScheduleValue"),
    ("ChartOfAccounts", "ExtDimensionTypes"),
    ("CommonCommand", "Group"),
    ("CommonAttribute", "DataSeparationValue"),
    ("EventSubscription", "Handler"),
    ("EventSubscription", "Source"),
    ("ScheduledJob", "MethodName"),
];
pub(crate) fn structured_scalar(kind: &str, property: &str) -> bool {
    REFERENCE_PROPERTIES.contains(&(kind, property))
}
#[cfg(test)]
mod tests {
    use super::*;
    use cf_domain::source_facts::{MissingPolicy, ReferenceStrength, SourceProof};
    use std::collections::{BTreeMap, BTreeSet};
    #[test]
    fn each_schema_reference_property_has_an_independent_value_case() {
        let cases = [
            (
                "FunctionalOption",
                "Location",
                "Constant.Feature",
                "Constant.Feature",
            ),
            (
                "BusinessProcess",
                "Task",
                "Task.Assignment",
                "Task.Assignment",
            ),
            (
                "AccountingRegister",
                "ChartOfAccounts",
                "ChartOfAccounts.Main",
                "ChartOfAccounts.Main",
            ),
            (
                "CalculationRegister",
                "ChartOfCalculationTypes",
                "ChartOfCalculationTypes.Plan",
                "ChartOfCalculationTypes.Plan",
            ),
            (
                "CalculationRegister",
                "Schedule",
                "InformationRegister.Schedule",
                "InformationRegister.Schedule",
            ),
            (
                "CalculationRegister",
                "ScheduleDate",
                "InformationRegister.Schedule.Dimension.Day",
                "InformationRegister.Schedule",
            ),
            (
                "CalculationRegister",
                "ScheduleValue",
                "InformationRegister.Schedule.Resource.Hours",
                "InformationRegister.Schedule",
            ),
            (
                "ChartOfAccounts",
                "ExtDimensionTypes",
                "ChartOfCharacteristicTypes.Kinds",
                "ChartOfCharacteristicTypes.Kinds",
            ),
            (
                "CommonCommand",
                "Group",
                "CommandGroup.Tools",
                "CommandGroup.Tools",
            ),
            (
                "CommonAttribute",
                "DataSeparationValue",
                "SessionParameter.Tenant",
                "SessionParameter.Tenant",
            ),
            (
                "EventSubscription",
                "Handler",
                "CommonModule.Events.OnWrite",
                "CommonModule.Events",
            ),
            (
                "EventSubscription",
                "Source",
                "CatalogObject.Entities",
                "Catalog.Entities",
            ),
            (
                "ScheduledJob",
                "MethodName",
                "CommonModule.Timer.Run",
                "CommonModule.Timer",
            ),
        ];
        assert_eq!(
            cases
                .iter()
                .map(|(kind, property, _, _)| (*kind, *property))
                .collect::<BTreeSet<_>>(),
            REFERENCE_PROPERTIES.iter().copied().collect()
        );
        for (kind, property, value, expected) in cases {
            let xml = format!(
                "<MetaDataObject><{kind} uuid='original'><Properties><Name>Probe</Name><{property}>{value}</{property}></Properties></{kind}></MetaDataObject>"
            );
            let doc = crate::source_reader::SourceDocument::descriptor(
                &format!("{kind}.Probe"),
                "schema.xml",
                xml.as_bytes(),
                SourceProof {
                    identity: "model".into(),
                    snapshot: "one".into(),
                    inputs: BTreeMap::new(),
                },
            )
            .unwrap();
            assert!(
                doc.facts
                    .references
                    .iter()
                    .any(|r| r.target.key() == expected
                        && r.strength == ReferenceStrength::Structured
                        && r.missing == MissingPolicy::Diagnose),
                "{kind}.{property}"
            );
        }
        assert!(
            !structured_scalar("CommonAttribute", "DataSeparationUse"),
            "an enum switch is not a metadata-reference value"
        );
    }
}

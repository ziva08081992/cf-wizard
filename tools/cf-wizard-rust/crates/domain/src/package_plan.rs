//! Immutable commands for the emitter and settings serializer, never discovery.
use crate::{
    Error, Result,
    source_facts::{ExpandedName, FactVersions, SourceProof},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentMode {
    Full,
    Projected,
    Service,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionRole {
    Selected,
    Dependency,
    Service,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum MergeAction {
    GetFromSecondConfiguration,
    MergePrioritizingMainConfiguration,
    DoNotMerge,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlanStamp {
    pub generation: String,
    pub proof_digest: String,
    pub readers: FactVersions,
    pub rule_profile: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ObjectProjection {
    pub content: ContentMode,
    pub role: SelectionRole,
    pub descriptor: String,
    pub retained: BTreeSet<String>,
    /// Source-derived identities for retained declarations, separate from the
    /// child rules that must be checked against them.
    pub declarations: BTreeMap<String, (String, String)>,
    /// Logical node/attribute -> original or explicitly generated identity.
    pub identity: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceEdit {
    pub start: usize,
    pub end: usize,
    pub replacement: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum FileOperation {
    Copy {
        source: String,
    },
    Project {
        source: String,
        edits: Vec<SourceEdit>,
    },
    Generate {
        bytes: Vec<u8>,
    },
}
/// Independent logical expectation. Built from source + policy, not emitted bytes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct XmlValue {
    pub name: ExpandedName,
    pub attributes: Vec<(ExpandedName, String)>,
    pub parts: Vec<XmlPart>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum XmlPart {
    Text(String),
    Element(XmlValue),
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "expected", rename_all = "snake_case")]
pub enum FileExpectation {
    Exact { sha256: String },
    Xml { root: XmlValue, sha256: String },
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileProjection {
    pub owner: String,
    pub content: ContentMode,
    pub operation: FileOperation,
    pub expectation: FileExpectation,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProjectionPlan {
    pub stamp: PlanStamp,
    /// Frozen from the dependency plan, before assigning per-file content modes.
    pub selected: BTreeSet<String>,
    pub dependencies: BTreeSet<String>,
    pub objects: BTreeMap<String, ObjectProjection>,
    pub files: BTreeMap<String, FileProjection>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MergeObjectPlan {
    pub action: MergeAction,
    pub protected: Vec<String>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MergeChildPlan {
    pub owner: String,
    pub fragment: String,
    pub tag: String,
    pub name: String,
    pub protected: Vec<String>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MergePlan {
    pub stamp: PlanStamp,
    pub objects: BTreeMap<String, MergeObjectPlan>,
    pub children: BTreeMap<String, MergeChildPlan>,
    /// Serialization order is a frozen presentation decision, not writer policy.
    pub order: Vec<String>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompiledPackagePlan {
    pub source: SourceProof,
    pub projection: ProjectionPlan,
    pub merge: MergePlan,
}
fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains(['\\', ':'])
        && !path.chars().any(char::is_control)
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}
impl CompiledPackagePlan {
    pub fn validate(&self) -> Result<()> {
        let invalid =
            |message: &str| Error::Invalid(format!("Некорректный план выдачи: {message}"));
        if self.projection.stamp != self.merge.stamp
            || self.projection.stamp.proof_digest != self.source.digest()?
            || self.projection.stamp.generation.is_empty()
            || self.projection.stamp.rule_profile.is_empty()
        {
            return Err(invalid("разные поколения или свидетельства источника"));
        }
        for (key, object) in &self.projection.objects {
            let expected_role = if self.projection.selected.contains(key) {
                SelectionRole::Selected
            } else if self.projection.dependencies.contains(key) {
                SelectionRole::Dependency
            } else {
                SelectionRole::Service
            };
            if object.role != expected_role {
                return Err(invalid("роль объекта расходится с окончательным составом"));
            }
            if !valid_path(&object.descriptor)
                || !self.projection.files.contains_key(&object.descriptor)
            {
                return Err(invalid("descriptor не включён в файлы"));
            }
            let expected = match object.role {
                SelectionRole::Selected if object.content == ContentMode::Full => {
                    MergeAction::GetFromSecondConfiguration
                }
                SelectionRole::Dependency if object.content != ContentMode::Service => {
                    MergeAction::MergePrioritizingMainConfiguration
                }
                SelectionRole::Service => MergeAction::DoNotMerge,
                _ => return Err(invalid("несогласованные content/selection роли")),
            };
            if self.merge.objects.get(key).map(|value| value.action) != Some(expected) {
                return Err(invalid("действие объединения не соответствует роли выбора"));
            }
            let descriptor = &self.projection.files[&object.descriptor];
            if descriptor.owner != *key || descriptor.content != object.content {
                return Err(invalid("descriptor принадлежит другой роли или объекту"));
            }
        }
        if !self
            .projection
            .selected
            .is_disjoint(&self.projection.dependencies)
            || self
                .projection
                .selected
                .iter()
                .chain(&self.projection.dependencies)
                .any(|key| !self.projection.objects.contains_key(key))
        {
            return Err(invalid(
                "выбранный объект или зависимость потеряны в проекции",
            ));
        }
        if self
            .merge
            .objects
            .keys()
            .any(|key| !self.projection.objects.contains_key(key))
        {
            return Err(invalid("лишний объект в настройках объединения"));
        }
        for (key, child) in &self.merge.children {
            let owner = self
                .projection
                .objects
                .get(&child.owner)
                .ok_or_else(|| invalid("нет владельца дочернего правила объединения"))?;
            if owner.role != SelectionRole::Dependency
                || !owner.retained.contains(&child.fragment)
                || owner.declarations.get(&child.fragment)
                    != Some(&(child.tag.clone(), child.name.clone()))
                || !matches!(child.tag.as_str(), "Form" | "Command")
                || child.name.is_empty()
                || *key != format!("{}.{}.{}", child.owner, child.tag, child.name)
                || self.merge.objects.contains_key(key)
            {
                return Err(invalid(
                    "дочернее правило не относится к сохранённой декларации",
                ));
            }
        }
        let order: BTreeSet<_> = self.merge.order.iter().collect();
        let rules: BTreeSet<_> = self
            .merge
            .objects
            .keys()
            .chain(self.merge.children.keys())
            .collect();
        if order.len() != self.merge.order.len() || order != rules {
            return Err(invalid("порядок настроек не совпадает с набором правил"));
        }
        for (path, file) in &self.projection.files {
            if !valid_path(path) || !self.projection.objects.contains_key(&file.owner) {
                return Err(invalid("неизвестный владелец или опасный путь файла"));
            }
            match &file.operation {
                FileOperation::Copy { source } | FileOperation::Project { source, .. } => {
                    if !valid_path(source)
                        || !matches!(
                            self.source.inputs.get(source),
                            Some(crate::source_facts::InputObservation::Content { .. })
                        )
                    {
                        return Err(invalid("файл не покрыт свидетельством источника"));
                    }
                    if file.content == ContentMode::Full {
                        let crate::source_facts::InputObservation::Content { sha256 } =
                            &self.source.inputs[source]
                        else {
                            unreachable!()
                        };
                        if !matches!(&file.operation, FileOperation::Copy { .. })
                            || file.expectation
                                != (FileExpectation::Exact {
                                    sha256: sha256.clone(),
                                })
                        {
                            return Err(invalid(
                                "полный исходник нельзя проецировать или подменять",
                            ));
                        }
                    }
                }
                FileOperation::Generate { .. } if file.content != ContentMode::Service => {
                    return Err(invalid("генерируемый файл без служебной роли"));
                }
                FileOperation::Generate { .. } => {}
            }
            if file.content == ContentMode::Full
                && !matches!(file.expectation, FileExpectation::Exact { .. })
            {
                return Err(invalid("полный файл требует побайтное ожидание"));
            }
        }
        Ok(())
    }
}

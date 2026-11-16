//! Freeze source-derived file and merge commands; emitters cannot choose policy.
use crate::{
    canonical::sha256,
    engine::Recipe,
    projection, source_edits,
    source_reader::SourceDocument,
    source_xml::{READABLE, SourceXml, XSI},
    xml::escape,
};
use cf_application::MetadataSource;
use cf_domain::{Error, Result, package_plan::*, source_facts::*};
use std::collections::{BTreeMap, BTreeSet};

const SUBSYSTEM: &str = "Subsystem._Перенос";

fn value(uri: &str, local: &str, text: Option<&str>) -> XmlValue {
    XmlValue {
        name: ExpandedName {
            uri: uri.into(),
            local: local.into(),
        },
        attributes: vec![],
        parts: text
            .filter(|text| !text.is_empty())
            .map_or_else(Vec::new, |text| vec![XmlPart::Text(text.into())]),
    }
}
fn tag(view: &SourceXml, id: usize, local: &str) -> String {
    view.xml.elements[id]
        .name
        .rsplit_once(':')
        .map_or_else(|| local.into(), |(prefix, _)| format!("{prefix}:{local}"))
}
fn project(
    owner: &str,
    source: &str,
    bytes: &[u8],
    edits: Vec<SourceEdit>,
    expected: XmlValue,
) -> Result<FileProjection> {
    let digest = source_edits::digest(bytes, &edits)?;
    Ok(FileProjection {
        owner: owner.into(),
        content: ContentMode::Projected,
        operation: FileOperation::Project {
            source: source.into(),
            edits,
        },
        expectation: FileExpectation::Xml {
            root: expected,
            sha256: digest,
        },
    })
}
fn insert(
    files: &mut BTreeMap<String, FileProjection>,
    path: &str,
    file: FileProjection,
) -> Result<()> {
    if files.insert(path.into(), file).is_some() {
        return Err(Error::Invalid(format!(
            "Повтор запланированного файла {path}"
        )));
    }
    Ok(())
}
fn copy(recipe: &Recipe, owner: &str, path: &str) -> Result<FileProjection> {
    let hash =
        recipe.composition.source_hashes.get(path).ok_or_else(|| {
            Error::Invalid(format!("Файл не зафиксирован предпросмотром: {path}"))
        })?;
    Ok(FileProjection {
        owner: owner.into(),
        content: ContentMode::Full,
        operation: FileOperation::Copy {
            source: path.into(),
        },
        expectation: FileExpectation::Exact {
            sha256: hash.clone(),
        },
    })
}
fn declarations(
    source: &SourceDocument,
    retained: &BTreeSet<String>,
) -> BTreeMap<String, (String, String)> {
    retained
        .iter()
        .filter_map(|id| {
            let fragment = &source.facts.fragments[id];
            (fragment.kind == FragmentKind::Declaration).then(|| {
                (
                    id.clone(),
                    (fragment.tag.local.clone(), fragment.name.clone()),
                )
            })
        })
        .collect()
}
fn identities(source: &SourceDocument, retained: &BTreeSet<String>) -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();
    for id in retained {
        let fragment = &source.facts.fragments[id];
        if let Some(uuid) = &fragment.uuid {
            result.insert(format!("{id}/@uuid"), uuid.clone());
        }
        if matches!(fragment.tag.local.as_str(), "TypeId" | "ValueId") {
            result.insert(id.clone(), fragment.name.clone());
        }
    }
    result
}

struct ConfigurationFiles {
    descriptor: FileProjection,
    subsystem: FileProjection,
    languages: Vec<String>,
    retained: BTreeSet<String>,
    identity: BTreeMap<String, String>,
    subsystem_uuid: String,
}

fn configuration(
    source: &SourceDocument,
    included: &BTreeSet<String>,
    selected: &BTreeSet<String>,
) -> Result<ConfigurationFiles> {
    let view = &source.view;
    let xml = &view.xml;
    let object = source.object;
    let properties = view
        .child(object, "Properties")
        .ok_or_else(|| Error::Invalid("Нет Properties основной конфигурации".into()))?;
    let children = view
        .child(object, "ChildObjects")
        .ok_or_else(|| Error::Invalid("Нет ChildObjects основной конфигурации".into()))?;
    let mut dropped = BTreeSet::new();
    let mut emptied = BTreeSet::new();
    let mut edits = vec![];
    for &property in &xml.elements[properties].children {
        let local = &view.names[property].local;
        let protected = matches!(local.as_str(), "Name" | "Synonym" | "Comment" | "Type")
            || crate::kinds::REQUIRED_PROPERTIES
                .iter()
                .any(|&(kind, names)| kind == "Configuration" && names.contains(&local.as_str()));
        if protected
            || source
                .inner_targets(property)
                .iter()
                .all(|target| target.starts_with("Language."))
        {
            continue;
        }
        let items = &xml.elements[property].children;
        let list = !items.is_empty()
            && items.iter().all(|&id| {
                view.names[id].local == "Item"
                    && matches!(view.names[id].uri.as_str(), READABLE | "")
            });
        let retained: BTreeSet<_> = items
            .iter()
            .copied()
            .filter(|&id| source.targets(id).is_empty())
            .collect();
        if !list || retained.is_empty() {
            emptied.insert(property);
            edits.push(SourceEdit {
                start: xml.elements[property].span.start,
                end: xml.elements[property].span.end,
                replacement: projection::empty_tag(view, property),
            });
        } else {
            for (position, &id) in items.iter().enumerate() {
                if !retained.contains(&id) {
                    dropped.insert(id);
                    // Preserve the old removal of the item's leading formatting gap.
                    let start = position
                        .checked_sub(1)
                        .map_or(xml.elements[property].inner.start, |previous| {
                            xml.elements[items[previous]].span.end
                        });
                    edits.push(SourceEdit {
                        start,
                        end: xml.elements[id].span.end,
                        replacement: String::new(),
                    });
                }
            }
        }
    }
    let mut languages = vec![];
    let mut kept = BTreeSet::new();
    let mut insert_before = None;
    for &id in &xml.elements[children].children {
        let kind = &view.names[id].local;
        if !view.metadata(id, kind) {
            dropped.insert(id);
            continue;
        }
        let name = xml.text(id)?;
        let name = name.trim();
        if insert_before.is_none()
            && kind != "Language"
            && (kind != "Subsystem" || name > "_Перенос")
        {
            insert_before = Some(id);
        }
        let key = format!("{kind}.{name}");
        if key == SUBSYSTEM && included.contains(&key) {
            return Err(Error::Invalid(
                "Имя _Перенос зарезервировано для служебной подсистемы".into(),
            ));
        }
        if kind == "Language" || included.contains(&key) {
            kept.insert(id);
        } else {
            dropped.insert(id);
        }
        if kind == "Language" {
            languages.push(format!("Languages/{name}.xml"));
        }
    }
    let sub_tag = tag(view, children, "Subsystem");
    let insertion = format!("<{sub_tag}>_Перенос</{sub_tag}>");
    let mut pieces = Vec::new();
    let mut previous = xml.elements[children].inner.start;
    let mut last_gap = "";
    for &id in &xml.elements[children].children {
        let gap = &xml.source[previous..xml.elements[id].span.start];
        if insert_before == Some(id) {
            pieces.push(format!("{gap}{insertion}"));
        }
        if kept.contains(&id) {
            pieces.push(format!("{gap}{}", xml.raw(id)));
        }
        last_gap = gap;
        previous = xml.elements[id].span.end;
    }
    if insert_before.is_none() {
        pieces.push(format!("{last_gap}{insertion}"));
    }
    let child_tag = &xml.elements[children].name;
    let mut opening = xml.source
        [xml.elements[children].span.start..xml.elements[children].inner.start]
        .to_owned();
    if opening.ends_with("/>") {
        opening.truncate(opening.len() - 2);
        opening.push('>');
    }
    let replacement = format!(
        "{opening}{}{}</{child_tag}>",
        pieces.join(""),
        &xml.source[previous..xml.elements[children].inner.end]
    );
    edits.push(SourceEdit {
        start: xml.elements[children].span.start,
        end: xml.elements[children].span.end,
        replacement,
    });
    let mut expected = view.filtered_value(xml.root, &dropped, &emptied)?;
    projection::layout(&mut expected, false);
    let XmlPart::Element(root_object) = expected
        .parts
        .iter_mut()
        .find(|part| matches!(part, XmlPart::Element(_)))
        .expect("configuration object")
    else {
        unreachable!()
    };
    let child_list = root_object
        .parts
        .iter_mut()
        .find_map(|part| match part {
            XmlPart::Element(value) if value.name.local == "ChildObjects" => Some(value),
            _ => None,
        })
        .expect("child list");
    let insertion = XmlPart::Element(value(
        &view.names[children].uri,
        "Subsystem",
        Some("_Перенос"),
    ));
    child_list.parts.clear();
    for &id in &xml.elements[children].children {
        if insert_before == Some(id) {
            child_list.parts.push(insertion.clone());
        }
        if kept.contains(&id) {
            child_list.parts.push(XmlPart::Element(view.value(id)?));
        }
    }
    if insert_before.is_none() {
        child_list.parts.push(insertion);
    }
    let retained = source
        .node_ids
        .iter()
        .enumerate()
        .filter(|(id, _)| {
            let mut node = Some(*id);
            while let Some(current) = node {
                if dropped.contains(&current) || current != *id && emptied.contains(&current) {
                    return false;
                }
                node = xml.elements[current].parent;
            }
            true
        })
        .map(|(_, id)| id.clone())
        .collect();
    let identity = identities(source, &retained);
    let uuid = crate::identity::transfer_subsystem(xml.attr(object, "uuid").unwrap_or_default());
    let subsystem = subsystem_file(source, selected, &uuid)?;
    Ok(ConfigurationFiles {
        descriptor: project(
            &source.facts.object,
            "Configuration.xml",
            xml.source.as_bytes(),
            edits,
            expected,
        )?,
        subsystem,
        languages,
        retained,
        identity,
        subsystem_uuid: uuid,
    })
}

fn subsystem_file(
    source: &SourceDocument,
    selected: &BTreeSet<String>,
    uuid: &str,
) -> Result<FileProjection> {
    let view = &source.view;
    let metadata_uri = &view.names[source.object].uri;
    let object_tag = tag(view, source.object, "Subsystem");
    let properties_tag = tag(view, source.object, "Properties");
    let children_tag = tag(view, source.object, "ChildObjects");
    let content_tag = tag(view, source.object, "Content");
    let properties = [
        ("Name", "_Перенос"),
        ("Synonym", ""),
        ("Comment", ""),
        ("IncludeHelpInContents", "false"),
        ("IncludeInCommandInterface", "false"),
        ("UseOneCommand", "false"),
        ("Explanation", ""),
        ("Picture", ""),
    ];
    let mut text = view.xml.source[..view.xml.elements[source.object].span.start].to_owned();
    text.push_str(&format!("<{object_tag} uuid=\"{uuid}\""));
    let object_prefix = object_tag.split_once(':').map(|(prefix, _)| prefix);
    let item_prefix = if object_prefix == Some("xr") {
        "cfwItem"
    } else {
        "xr"
    };
    let type_prefix = if object_prefix == Some("xsi") {
        "cfwType"
    } else {
        "xsi"
    };
    // Metadata bindings may be declared on Configuration rather than the root.
    // Preserve them, but give generated Item/type bindings independent names
    // when either conventional prefix is the metadata element prefix itself.
    for attribute in &view.xml.elements[source.object].attributes {
        if attribute.name == "xmlns" || attribute.name.starts_with("xmlns:") {
            if attribute.name == format!("xmlns:{item_prefix}")
                || attribute.name == format!("xmlns:{type_prefix}")
            {
                continue;
            }
            text.push_str(&format!(
                " {}=\"{}\"",
                attribute.name,
                escape(&attribute.value)
            ));
        }
    }
    for (prefix, local, uri) in [(item_prefix, "Item", READABLE), (type_prefix, "type", XSI)] {
        if !view
            .resolve(view.xml.root, &format!("{prefix}:{local}"), false)
            .is_ok_and(|name| name.uri == uri)
        {
            text.push_str(&format!(" xmlns:{prefix}=\"{uri}\""));
        }
    }
    text.push_str(&format!(">\r\n\t\t<{properties_tag}>\r\n"));
    let mut expected_properties = value(metadata_uri, "Properties", None);
    for (local, body) in properties {
        let property_tag = tag(view, source.object, local);
        text.push_str(&if body.is_empty() {
            format!("\t\t\t<{property_tag}/>\r\n")
        } else {
            format!(
                "\t\t\t<{property_tag}>{}</{property_tag}>\r\n",
                escape(body)
            )
        });
        expected_properties
            .parts
            .push(XmlPart::Element(value(metadata_uri, local, Some(body))));
    }
    text.push_str(&format!("\t\t\t<{content_tag}>\r\n"));
    let mut content = value(metadata_uri, "Content", None);
    for key in selected {
        text.push_str(&format!("\t\t\t\t<{item_prefix}:Item {type_prefix}:type=\"{item_prefix}:MDObjectRef\">{}</{item_prefix}:Item>\r\n", escape(key)));
        let mut item = value(READABLE, "Item", Some(key));
        item.attributes.push((
            ExpandedName {
                uri: XSI.into(),
                local: "type".into(),
            },
            format!("{item_prefix}:MDObjectRef"),
        ));
        content.parts.push(XmlPart::Element(item));
    }
    expected_properties.parts.push(XmlPart::Element(content));
    let root_tag = &view.xml.elements[view.xml.root].name;
    text.push_str(&format!("\t\t\t</{content_tag}>\r\n\t\t</{properties_tag}>\r\n\t\t<{children_tag}/>\r\n\t</{object_tag}>\r\n</{root_tag}>"));
    let mut object = value(metadata_uri, "Subsystem", None);
    object.attributes.push((
        ExpandedName {
            uri: String::new(),
            local: "uuid".into(),
        },
        uuid.into(),
    ));
    object.parts = vec![
        XmlPart::Element(expected_properties),
        XmlPart::Element(value(metadata_uri, "ChildObjects", None)),
    ];
    let mut expected = view.value(view.xml.root)?;
    expected.parts = vec![XmlPart::Element(object)];
    Ok(FileProjection {
        owner: SUBSYSTEM.into(),
        content: ContentMode::Service,
        expectation: FileExpectation::Xml {
            root: expected,
            sha256: sha256(text.as_bytes()),
        },
        operation: FileOperation::Generate {
            bytes: text.into_bytes(),
        },
    })
}

pub(crate) fn compile(recipe: &Recipe, source: &dyn MetadataSource) -> Result<CompiledPackagePlan> {
    let composition = &recipe.composition;
    composition.require_complete()?;
    let mut proof = SourceProof {
        identity: source.identity().into(),
        snapshot: composition.source_fingerprint.clone(),
        inputs: BTreeMap::new(),
    };
    for (path, hash) in &composition.source_hashes {
        proof.observe(
            path.clone(),
            InputObservation::Content {
                sha256: hash.clone(),
            },
        )?;
    }
    for (path, present) in &recipe.presence {
        if !present {
            proof.observe(path.clone(), InputObservation::Absent)?;
        }
    }
    for (path, entries) in &recipe.listings {
        proof.observe(
            path.clone(),
            InputObservation::Directory {
                entries: entries
                    .iter()
                    .map(|entry| DirectoryEntry {
                        name: entry.name.clone(),
                        directory: entry.directory,
                    })
                    .collect(),
            },
        )?;
    }
    for (key, files) in &recipe.below {
        let base = crate::source_schema::descriptor(key)
            .expect("metadata descriptor")
            .trim_end_matches(".xml")
            .to_owned();
        proof.observe(
            base,
            InputObservation::Directory {
                entries: files
                    .iter()
                    .map(|path| DirectoryEntry {
                        name: path.clone(),
                        directory: false,
                    })
                    .collect(),
            },
        )?;
    }
    let stamp = PlanStamp {
        generation: "compiling".into(),
        proof_digest: proof.digest()?,
        readers: FactVersions::current(),
        rule_profile: RULE_PROFILE.into(),
    };
    let mut projection = ProjectionPlan {
        stamp: stamp.clone(),
        selected: composition.selected.clone(),
        dependencies: composition.stubs.keys().cloned().collect(),
        objects: BTreeMap::new(),
        files: BTreeMap::new(),
    };
    let mut merge = MergePlan {
        stamp,
        objects: BTreeMap::new(),
        children: BTreeMap::new(),
        order: vec![],
    };
    for key in &composition.selected {
        let path = crate::source_schema::descriptor(key).expect("metadata descriptor");
        let document = recipe.frozen_document(key).ok_or_else(|| {
            Error::Invalid(format!(
                "Не прочитаны исходные факты выбранного объекта {key}"
            ))
        })?;
        let retained = document.facts.fragments.keys().cloned().collect();
        projection.objects.insert(
            key.clone(),
            ObjectProjection {
                content: ContentMode::Full,
                role: SelectionRole::Selected,
                descriptor: path,
                declarations: declarations(document, &retained),
                identity: identities(document, &retained),
                retained,
            },
        );
        for path in &recipe.below[key] {
            insert(&mut projection.files, path, copy(recipe, key, path)?)?;
        }
        merge.objects.insert(
            key.clone(),
            MergeObjectPlan {
                action: MergeAction::GetFromSecondConfiguration,
                protected: vec![],
            },
        );
    }
    for (key, built) in &recipe.built {
        let path = crate::source_schema::descriptor(key).expect("metadata descriptor");
        let full = cf_domain::full_resource(key.split_once('.').expect("metadata key").0);
        let file = if full {
            copy(recipe, key, &path)?
        } else {
            project(
                key,
                &path,
                built.source.view.xml.source.as_bytes(),
                built.projection.edits.clone(),
                built.projection.expected.clone(),
            )?
        };
        let content = file.content;
        insert(&mut projection.files, &path, file)?;
        for path in &built.files {
            insert(&mut projection.files, path, copy(recipe, key, path)?)?;
        }
        projection.objects.insert(
            key.clone(),
            ObjectProjection {
                content,
                role: SelectionRole::Dependency,
                descriptor: path,
                declarations: declarations(&built.source, &built.projection.retained),
                retained: built.projection.retained.clone(),
                identity: identities(&built.source, &built.projection.retained),
            },
        );
        merge.objects.insert(
            key.clone(),
            MergeObjectPlan {
                action: MergeAction::MergePrioritizingMainConfiguration,
                protected: recipe.merge_rules.get(key).cloned().unwrap_or_default(),
            },
        );
    }
    for (key, protected) in &recipe.merge_rules {
        if merge.objects.contains_key(key) {
            continue;
        }
        let (owner, built) = recipe
            .built
            .iter()
            .find(|(owner, _)| key.starts_with(&format!("{owner}.")))
            .ok_or_else(|| Error::Invalid(format!("Нет владельца защиты {key}")))?;
        let (_, declaration) = built
            .source
            .facts
            .fragments
            .iter()
            .find(|(_, fragment)| {
                fragment.kind == FragmentKind::Declaration
                    && format!("{owner}.{}.{}", fragment.tag.local, fragment.name) == *key
                    && built.projection.retained.contains(&fragment.id)
            })
            .ok_or_else(|| Error::Invalid(format!("Нет сохранённой декларации для {key}")))?;
        merge.children.insert(
            key.clone(),
            MergeChildPlan {
                owner: owner.clone(),
                fragment: declaration.id.clone(),
                tag: declaration.tag.local.clone(),
                name: declaration.name.clone(),
                protected: protected.clone(),
            },
        );
    }
    let configuration_key = format!("Configuration.{}", source.catalog().name);
    let config_source = SourceDocument::descriptor(
        &configuration_key,
        "Configuration.xml",
        recipe.frozen_text("Configuration.xml")?.as_bytes(),
        proof.clone(),
    )?;
    let included = composition
        .selected
        .iter()
        .chain(composition.stubs.keys())
        .cloned()
        .collect();
    let config = configuration(&config_source, &included, &composition.selected)?;
    for path in &config.languages {
        insert(
            &mut projection.files,
            path,
            copy(recipe, &configuration_key, path)?,
        )?;
    }
    projection.objects.insert(
        configuration_key.clone(),
        ObjectProjection {
            content: ContentMode::Projected,
            role: SelectionRole::Service,
            descriptor: "Configuration.xml".into(),
            declarations: declarations(&config_source, &config.retained),
            retained: config.retained,
            identity: config.identity,
        },
    );
    projection.objects.insert(
        SUBSYSTEM.into(),
        ObjectProjection {
            content: ContentMode::Service,
            role: SelectionRole::Service,
            descriptor: "Subsystems/_Перенос.xml".into(),
            declarations: BTreeMap::new(),
            retained: BTreeSet::new(),
            identity: BTreeMap::from([("Subsystem/@uuid".into(), config.subsystem_uuid)]),
        },
    );
    insert(
        &mut projection.files,
        "Configuration.xml",
        config.descriptor,
    )?;
    insert(
        &mut projection.files,
        "Subsystems/_Перенос.xml",
        config.subsystem,
    )?;
    for key in [configuration_key.as_str(), SUBSYSTEM] {
        merge.objects.insert(
            key.into(),
            MergeObjectPlan {
                action: MergeAction::DoNotMerge,
                protected: vec![],
            },
        );
    }
    merge.order.push(configuration_key);
    merge.order.extend(composition.selected.iter().cloned());
    merge.order.extend(composition.stubs.keys().cloned());
    merge.order.extend(merge.children.keys().cloned());
    merge.order.push(SUBSYSTEM.into());
    let generation = sha256(
        &serde_json::to_vec(&(
            &proof,
            &projection.objects,
            &projection.files,
            &merge.objects,
            &merge.children,
            &merge.order,
        ))
        .map_err(crate::external)?,
    );
    projection.stamp.generation = generation.clone();
    merge.stamp.generation = generation;
    let plan = CompiledPackagePlan {
        source: proof,
        projection,
        merge,
    };
    plan.validate()?;
    Ok(plan)
}

/// Serialize only explicit actions. This function has no access to source files.
pub(crate) fn settings(plan: &CompiledPackagePlan) -> Result<String> {
    use std::fmt::Write;
    plan.validate()?;
    let mut text = "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n<Settings xmlns=\"http://v8.1c.ru/8.3/config/merge/settings\" xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" version=\"1.2\" platformVersion=\"8.3.11\">\r\n\t<Parameters>\r\n\t\t<ConfigurationsRelation>ConfigurationsNotRelated</ConfigurationsRelation>\r\n\t\t<AllowMainConfigurationObjectDeletion>false</AllowMainConfigurationObjectDeletion>\r\n\t</Parameters>\r\n\t<Objects>\r\n".to_owned();
    for key in &plan.merge.order {
        let (action, protected) = if let Some(object) = plan.merge.objects.get(key) {
            (object.action, &object.protected)
        } else {
            (
                MergeAction::MergePrioritizingMainConfiguration,
                &plan.merge.children[key].protected,
            )
        };
        write!(text, "\t\t<Object fullNameInSecondConfiguration=\"{}\">\r\n\t\t\t<MergeRule>{action:?}</MergeRule>\r\n", escape(key)).expect("String");
        if !protected.is_empty() {
            text.push_str("\t\t\t<Properties>\r\n");
            for property in protected {
                write!(text, "\t\t\t\t<Property name=\"{}\">\r\n\t\t\t\t\t<MergeRule>DoNotMerge</MergeRule>\r\n\t\t\t\t</Property>\r\n", escape(property)).expect("String");
            }
            text.push_str("\t\t\t</Properties>\r\n");
        }
        text.push_str("\t\t</Object>\r\n");
    }
    text.push_str("\t</Objects>\r\n</Settings>\r\n");
    Ok(text)
}

#[cfg(test)]
mod namespace_tests {
    use super::*;
    #[test]
    fn generated_subsystem_preserves_object_local_metadata_bindings() {
        let md = crate::source_xml::MD;
        for (root, object) in [
            (
                format!("<MetaDataObject xmlns=\"{md}\">"),
                format!("<m:Configuration xmlns:m=\"{md}\"/>"),
            ),
            (
                "<MetaDataObject>".into(),
                format!("<Configuration xmlns=\"{md}\"/>"),
            ),
            (
                format!("<MetaDataObject xmlns=\"{md}\">"),
                format!("<xr:Configuration xmlns:xr=\"{md}\"/>"),
            ),
            (
                format!("<MetaDataObject xmlns=\"{md}\">"),
                format!("<xsi:Configuration xmlns:xsi=\"{md}\"/>"),
            ),
        ] {
            let xml = format!("{root}{object}</MetaDataObject>");
            let doc = SourceDocument::descriptor(
                "Configuration.Test",
                "Configuration.xml",
                xml.as_bytes(),
                SourceProof {
                    identity: "fixture".into(),
                    snapshot: "fixed".into(),
                    inputs: BTreeMap::new(),
                },
            )
            .unwrap();
            let file = subsystem_file(
                &doc,
                &BTreeSet::from(["Catalog.Selected".into()]),
                "00000000-0000-0000-0000-000000000001",
            )
            .unwrap();
            let FileOperation::Generate { bytes } = file.operation else {
                panic!("expected generated file");
            };
            let FileExpectation::Xml { root, .. } = file.expectation else {
                panic!("expected independent logical tree");
            };
            crate::emission_guard::descriptor(&root, &bytes, "Subsystems/_Перенос.xml").unwrap();
        }
    }
}

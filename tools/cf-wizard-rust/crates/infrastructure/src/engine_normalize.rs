//! Structural policy adds explicit requirements before byte-preserving projection.
use super::*;
use crate::kinds::{REQUIRED_CHILDREN, REQUIRED_LISTS};

impl Session<'_> {
    fn choose(
        &mut self,
        rule: &str,
        key: &str,
        tag: &str,
        name: &str,
        ordinal: usize,
    ) -> Result<()> {
        let (kind, object_name) = key.split_once('.').expect("metadata key");
        self.choices.insert(StructuralChoice {
            rule: rule.into(),
            owner: key.into(),
            chosen: format!("{key}.{tag}.{name}"),
            source: descriptor(key).expect("key"),
            ordinal,
        });
        self.require(
            Reference {
                kind: kind.into(),
                name: object_name.into(),
                member: tag.into(),
                member_name: name.into(),
                ..Reference::default()
            },
            key,
            false,
        )
    }
    pub(super) fn normalise_projection(
        &mut self,
        key: &str,
        present: &BTreeSet<String>,
        allow_choice: bool,
    ) -> Result<bool> {
        #[cfg(test)]
        {
            self.projection_visits += 1;
        }
        self.source_address = descriptor(key).expect("key");
        let source = self.document(key)?;
        let view = &source.view;
        let xml = &view.xml;
        let root = source.object;
        let kind = key.split('.').next().unwrap_or_default();
        let spec = self.states[key].spec.clone();
        let children = view
            .child(root, "ChildObjects")
            .map_or(&[][..], |n| xml.elements[n].children.as_slice());
        let kept = |n: usize| {
            let e = &xml.elements[n];
            let name = source.name(n).to_owned();
            view.metadata(n, &e.local)
                && (spec.members.contains(&(e.local.clone(), name.clone()))
                    || spec.members.contains(&("*".into(), name)))
        };
        if spec.by_value {
            for (ordinal, &n) in children.iter().enumerate() {
                if view.metadata(n, "Dimension")
                    && view
                        .child(n, "Properties")
                        .and_then(|p| view.child(p, "Master"))
                        .is_some_and(|m| xml.text(m).is_ok_and(|v| v == "true"))
                    && !kept(n)
                {
                    self.choose(
                        "master_dimension",
                        key,
                        "Dimension",
                        source.name(n),
                        ordinal,
                    )?;
                    return Ok(true);
                }
            }
        }
        if allow_choice
            && let Some(&(_, tags)) = REQUIRED_CHILDREN.iter().find(|&&(k, _)| k == kind)
        {
            let satisfied = children
                .iter()
                .any(|&n| tags.contains(&xml.elements[n].local.as_str()) && kept(n));
            if !satisfied {
                let candidate = tags.iter().find_map(|tag| {
                    children
                        .iter()
                        .enumerate()
                        .find(|&(_, n)| view.metadata(*n, tag))
                });
                if let Some((ordinal, &n)) = candidate {
                    self.choose(
                        "minimum_field",
                        key,
                        &xml.elements[n].local,
                        source.name(n),
                        ordinal,
                    )?;
                    return Ok(true);
                }
                self.diagnostic(
                    DiagnosticKind::MissingCandidate,
                    key,
                    &format!("{key}/minimum_field"),
                );
            }
        }
        let Some(props) = view.child(root, "Properties") else {
            return Ok(false);
        };
        if self.states[key].required {
            if spec.predefined {
                let rel = format!(
                    "{}/Ext/Predefined.xml",
                    descriptor(key).expect("key").trim_end_matches(".xml")
                );
                if !self.source.exists(&rel, self.cancel)? {
                    self.diagnostic(DiagnosticKind::MissingFile, key, &rel);
                }
            }
            for &n in children {
                let tag = xml.elements[n].local.as_str();
                if matches!(tag, "Form" | "Template") && kept(n) {
                    let rel = format!(
                        "{}/{s}/{}.xml",
                        descriptor(key).expect("key").trim_end_matches(".xml"),
                        source.name(n),
                        s = if tag == "Form" { "Forms" } else { "Templates" }
                    );
                    if !self.source.exists(&rel, self.cancel)? {
                        self.diagnostic(DiagnosticKind::MissingFile, key, &rel);
                    }
                }
            }
        }
        for &p in &xml.elements[props].children {
            let node = &xml.elements[p];
            if !view.metadata(p, &node.local) {
                continue;
            }
            if kind == "Document" && node.local == "RegisterRecords" {
                self.states
                    .get_mut(key)
                    .expect("state")
                    .spec
                    .property_items
                    .entry(node.local.clone())
                    .or_default();
                continue;
            }
            let required = spec.based_on && node.local == "BasedOn"
                || REQUIRED_LISTS
                    .iter()
                    .any(|&(k, props)| k == kind && props.contains(&node.local.as_str()));
            if node.children.is_empty() {
                if required && allow_choice {
                    self.diagnostic(
                        DiagnosticKind::MissingCandidate,
                        key,
                        &format!("{key}/{}", node.local),
                    );
                }
                continue;
            }
            if !node.children.iter().all(|&n| {
                view.names[n].local == "Item"
                    && matches!(view.names[n].uri.as_str(), crate::source_xml::READABLE | "")
            }) {
                continue;
            }
            let foreign = source.targets(p).iter().any(|target| target != key);
            if !foreign && !required {
                continue;
            }
            let old = spec
                .property_items
                .get(&node.local)
                .cloned()
                .unwrap_or_default();
            for (ordinal, &n) in node.children.iter().enumerate() {
                self.requirements.conditional_item(
                    key,
                    &format!("$property:{}|{ordinal}", node.local),
                    source.targets(n),
                )?;
            }
            let eligible: BTreeSet<_> = node
                .children
                .iter()
                .enumerate()
                .filter(|&(_, n)| {
                    source
                        .targets(*n)
                        .iter()
                        .all(|target| present.contains(target) || target == key)
                })
                .map(|(i, _)| i)
                .collect();
            let mut wanted = old.clone();
            wanted.extend(eligible);
            if allow_choice && required && wanted.is_empty() {
                wanted.insert(0);
            }
            if !allow_choice {
                self.states
                    .get_mut(key)
                    .expect("state")
                    .spec
                    .property_items
                    .entry(node.local.clone())
                    .or_default();
                continue;
            }
            for ordinal in wanted.difference(&old) {
                self.choose(
                    if required {
                        "required_list"
                    } else {
                        "retained_list"
                    },
                    key,
                    &format!("$property:{}", node.local),
                    &ordinal.to_string(),
                    *ordinal,
                )?;
            }
            if wanted != old {
                return Ok(true);
            }
            self.states
                .get_mut(key)
                .expect("state")
                .spec
                .property_items
                .entry(node.local.clone())
                .or_default();
        }
        Ok(false)
    }
    pub(super) fn choose_structural(&mut self) -> Result<bool> {
        let present = self.take_present();
        let result = self.choose_structural_in(&present);
        self.present = present;
        result
    }
    fn choose_structural_in(&mut self, present: &BTreeSet<String>) -> Result<bool> {
        while let Some(key) = self.structural_pending.pop_first() {
            if self.normalise_projection(&key, present, true)? {
                self.structural_pending.insert(key);
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub(super) fn choose_registrar(&mut self) -> Result<bool> {
        let included = self.take_present();
        let result = self.choose_registrar_in(&included);
        self.present = included;
        result
    }
    fn choose_registrar_in(&mut self, included: &BTreeSet<String>) -> Result<bool> {
        while let Some(key) = self.registrar_pending.pop_first() {
            let key = &key;
            let kind = key.split('.').next().unwrap_or_default();
            let subordinate = if kind == "InformationRegister" {
                let source = self.document(key)?;
                source
                    .view
                    .child(source.object, "Properties")
                    .and_then(|p| source.view.child(p, "WriteMode"))
                    .is_some_and(|n| {
                        source
                            .view
                            .xml
                            .text(n)
                            .is_ok_and(|v| v == "RecorderSubordinate")
                    })
            } else {
                false
            };
            let needs = matches!(
                kind,
                "AccumulationRegister" | "AccountingRegister" | "CalculationRegister"
            ) || subordinate;
            if !needs {
                continue;
            }
            let mut docs = self.registrars(key)?;
            if docs.iter().any(|d| self.selected.contains(d)) {
                continue;
            }
            let mut satisfied = false;
            for doc in docs
                .iter()
                .filter(|d| self.states.contains_key(*d))
                .cloned()
                .collect::<Vec<_>>()
            {
                let source = self.document(&doc)?;
                let records = source
                    .view
                    .child(source.object, "Properties")
                    .and_then(|p| source.view.child(p, "RegisterRecords"));
                if let Some(records) = records
                    && self.states[&doc]
                        .spec
                        .property_items
                        .get("RegisterRecords")
                        .is_some_and(|items| {
                            items.iter().any(|&i| {
                                source.view.xml.elements[records]
                                    .children
                                    .get(i)
                                    .is_some_and(|&n| source.targets(n).contains(key))
                            })
                        })
                {
                    satisfied = true;
                    break;
                }
            }
            if satisfied {
                continue;
            }
            docs.sort_by_key(|d| (!included.contains(d), descriptor(d).unwrap_or_default()));
            let mut candidates = vec![];
            for doc in docs {
                let source = self.document(&doc)?;
                let records = source
                    .view
                    .child(source.object, "Properties")
                    .and_then(|p| source.view.child(p, "RegisterRecords"));
                if let Some(records) = records {
                    for (ordinal, &n) in source.view.xml.elements[records]
                        .children
                        .iter()
                        .enumerate()
                    {
                        if source.targets(n).contains(key) {
                            if self.selected.contains(&doc)
                                || self.states.get(&doc).is_some_and(|s| {
                                    s.spec
                                        .property_items
                                        .get("RegisterRecords")
                                        .is_some_and(|items| items.contains(&ordinal))
                                })
                            {
                                candidates.clear();
                                candidates.push((false, String::new(), 0, String::new(), true));
                                break;
                            }
                            candidates.push((
                                !included.contains(&doc),
                                descriptor(&doc).expect("doc"),
                                ordinal,
                                doc.clone(),
                                false,
                            ));
                        }
                    }
                }
                if !candidates.is_empty() {
                    break;
                }
            }
            if candidates.iter().any(|c| c.4) {
                continue;
            }
            candidates.sort();
            if let Some((_, path, ordinal, doc, _)) = candidates.into_iter().next() {
                let (_, name) = doc.split_once('.').expect("doc");
                self.require(
                    Reference {
                        kind: "Document".into(),
                        name: name.into(),
                        member: "$property:RegisterRecords".into(),
                        member_name: ordinal.to_string(),
                        ..Reference::default()
                    },
                    key,
                    false,
                )?;
                self.choices.insert(StructuralChoice {
                    rule: "registrar_link".into(),
                    owner: key.clone(),
                    chosen: format!("{doc}→{key}"),
                    source: path,
                    ordinal,
                });
                return Ok(true);
            }
            self.source_address = descriptor(key).expect("key");
            self.diagnostic(
                DiagnosticKind::MissingCandidate,
                key,
                &format!("{key}/registrar"),
            );
        }
        Ok(false)
    }
}

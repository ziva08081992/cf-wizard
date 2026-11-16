//! Owned typed fact normalisation. The Datafrog context exists only during a query.
use crate::kernel::{FragmentId, Kernel, ObjectId, Relations, Seeds};
use cf_application::Cancellation;
use cf_domain::{ImmediateReason, Result};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Eq, PartialEq, Ord, PartialOrd)]
enum Fact {
    Owner(FragmentId, ObjectId),
    Mandatory(ObjectId, FragmentId),
    Contains(ObjectId, FragmentId),
    Reference(FragmentId, ObjectId),
    Member(FragmentId, FragmentId),
    Conditional(FragmentId, FragmentId, FragmentId),
    Resource(ObjectId),
}

pub(crate) struct Requirements {
    pub core: Kernel,
    objects: BTreeMap<String, ObjectId>,
    keys: Vec<String>,
    fragments: BTreeMap<(String, String), FragmentId>,
    labels: Vec<(String, String)>,
    pending: Relations,
    all: Relations,
    seeds: Seeds,
    fed: BTreeSet<Fact>,
    pub rounds: usize,
}
pub(crate) struct ActivatedRequirements {
    pub objects: Vec<String>,
    pub fragments: Vec<(String, String)>,
}
impl Requirements {
    pub fn new(selected: &BTreeSet<String>) -> Result<Self> {
        let mut query = Self {
            core: Kernel::new(0, 0)?,
            objects: BTreeMap::new(),
            keys: vec![],
            fragments: BTreeMap::new(),
            labels: vec![],
            pending: Relations::default(),
            all: Relations::default(),
            seeds: Seeds::default(),
            fed: BTreeSet::new(),
            rounds: 0,
        };
        for key in selected {
            let id = query.object(key)?;
            query.seeds.full.push(id);
        }
        query.core.seed(&query.seeds)?;
        Ok(query)
    }
    fn object(&mut self, key: &str) -> Result<ObjectId> {
        if let Some(&id) = self.objects.get(key) {
            return Ok(id);
        }
        let id = ObjectId(u32::try_from(self.keys.len()).map_err(crate::external)?);
        self.core.grow(self.keys.len() + 1, self.labels.len())?;
        self.keys.push(key.into());
        self.objects.insert(key.into(), id);
        if cf_domain::full_resource(key.split('.').next().unwrap_or_default()) {
            self.pending.full_resource.push(id);
        }
        Ok(id)
    }
    pub fn active(&self, key: &str) -> bool {
        self.objects
            .get(key)
            .is_some_and(|&o| self.core.state().object(o))
    }
    pub fn fragment(&mut self, key: &str, label: &str) -> Result<FragmentId> {
        if let Some(&id) = self.fragments.get(&(key.into(), label.into())) {
            return Ok(id);
        }
        let object = self.object(key)?;
        let id = FragmentId(u32::try_from(self.labels.len()).map_err(crate::external)?);
        self.core.grow(self.keys.len(), self.labels.len() + 1)?;
        self.labels.push((key.into(), label.into()));
        self.fragments.insert((key.into(), label.into()), id);
        self.pending.owner.push((id, object));
        self.pending.contains.push((object, id));
        if label == "$identity" {
            self.pending.mandatory.push((object, id));
        }
        Ok(id)
    }
    pub fn reference(&mut self, owner: &str, key: &str, label: Option<&str>) -> Result<()> {
        self.reference_from(owner, "$identity", key, label)
    }
    pub fn reference_from(
        &mut self,
        owner: &str,
        origin: &str,
        key: &str,
        label: Option<&str>,
    ) -> Result<()> {
        let from = self.fragment(owner, origin)?;
        self.fragment(key, "$identity")?;
        self.pending.reference.push((from, self.objects[key]));
        if let Some(label) = label {
            let target = self.fragment(key, label)?;
            self.pending.member_reference.push((from, target));
        }
        Ok(())
    }
    pub fn conditional_item(
        &mut self,
        key: &str,
        label: &str,
        targets: &BTreeSet<String>,
    ) -> Result<()> {
        let mut guard = self.fragment(key, "$identity")?;
        let result = self.fragment(key, label)?;
        if targets.is_empty() {
            self.pending.member_reference.push((guard, result));
            return Ok(());
        }
        for (i, target) in targets.iter().enumerate() {
            let premise = self.fragment(target, "$identity")?;
            let next = if i + 1 == targets.len() {
                result
            } else {
                self.fragment(key, &format!("$guard:{label}:{i}"))?
            };
            self.pending.conditional.push((guard, premise, next));
            guard = next;
        }
        Ok(())
    }
    pub fn close(&mut self, cancel: &Cancellation) -> Result<ActivatedRequirements> {
        let mut pending = std::mem::take(&mut self.pending);
        pending
            .owner
            .retain(|&(a, b)| self.fed.insert(Fact::Owner(a, b)));
        pending
            .mandatory
            .retain(|&(a, b)| self.fed.insert(Fact::Mandatory(a, b)));
        pending
            .contains
            .retain(|&(a, b)| self.fed.insert(Fact::Contains(a, b)));
        pending
            .reference
            .retain(|&(a, b)| self.fed.insert(Fact::Reference(a, b)));
        pending
            .member_reference
            .retain(|&(a, b)| self.fed.insert(Fact::Member(a, b)));
        pending
            .conditional
            .retain(|&(a, b, c)| self.fed.insert(Fact::Conditional(a, b, c)));
        pending
            .full_resource
            .retain(|&a| self.fed.insert(Fact::Resource(a)));
        self.core.feed(&pending)?;
        macro_rules! extend {($($f:ident),*)=>{$(self.all.$f.extend(pending.$f);)*}}
        extend!(
            owner,
            mandatory,
            contains,
            reference,
            member_reference,
            conditional,
            full_resource
        );
        let delta = self.core.close(cancel)?;
        self.rounds += delta.rounds;
        Ok(ActivatedRequirements {
            objects: delta
                .objects
                .into_iter()
                .map(|id| self.keys[id.0 as usize].clone())
                .collect(),
            fragments: delta
                .keeps
                .into_iter()
                .map(|id| self.labels[id.0 as usize].clone())
                .collect(),
        })
    }
    pub fn reasons(&self) -> Vec<ImmediateReason> {
        let mut out = BTreeSet::new();
        let s = self.core.state();
        let o = |id: ObjectId| format!("object:{}", self.keys[id.0 as usize]);
        let k = |id: FragmentId| {
            let (key, label) = &self.labels[id.0 as usize];
            format!("keep:{key}/{label}")
        };
        let f = |id: ObjectId| format!("full:{}", self.keys[id.0 as usize]);
        let mut add = |rule: &str, head: String, premises: Vec<String>| {
            out.insert(ImmediateReason {
                rule: rule.into(),
                head,
                premises,
            });
        };
        for &id in &self.seeds.full {
            add("seed_full", f(id), vec![]);
        }
        for &(a, b) in &self.all.owner {
            if s.keep(a) {
                add("owner", o(b), vec![k(a)]);
            }
        }
        for id in s.full_objects() {
            add("full_object", o(id), vec![f(id)]);
        }
        for &(a, b) in &self.all.mandatory {
            if s.object(a) {
                add("mandatory", k(b), vec![o(a)]);
            }
        }
        for &id in &self.all.full_resource {
            if s.object(id) {
                add("full_resource", f(id), vec![o(id)]);
            }
        }
        for &(a, b) in &self.all.contains {
            if s.full(a) {
                add("contains", k(b), vec![f(a)]);
            }
        }
        for &(a, b) in &self.all.reference {
            if s.keep(a) {
                add("reference", o(b), vec![k(a)]);
            }
        }
        for &(a, b) in &self.all.member_reference {
            if s.keep(a) {
                add("member_reference", k(b), vec![k(a)]);
            }
        }
        for &(a, b, c) in &self.all.conditional {
            if s.keep(a) && s.keep(b) {
                add("conditional", k(c), vec![k(a), k(b)]);
            }
        }
        out.into_iter().collect()
    }
    pub fn counts(&self) -> (usize, usize, usize) {
        (self.objects.len(), self.fragments.len(), self.fed.len())
    }
}

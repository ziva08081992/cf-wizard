//! Query-local positive requirement closure. Readers, policy and choices live outside it.
//! The Datafrog context is deliberately not Send; only owned State/Delta leave a job.
use cf_application::Cancellation;
use cf_domain::{Error, Result};
use datafrog::{Iteration, Variable};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ObjectId(pub u32);
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FragmentId(pub u32);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum Requirement {
    Object(ObjectId),
    Keep(FragmentId),
    Full(ObjectId),
}

/// Immutable rows may be supplied in successive demand batches. Facts are not query output.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Relations {
    pub owner: Vec<(FragmentId, ObjectId)>,
    pub mandatory: Vec<(ObjectId, FragmentId)>,
    pub contains: Vec<(ObjectId, FragmentId)>,
    pub reference: Vec<(FragmentId, ObjectId)>,
    pub member_reference: Vec<(FragmentId, FragmentId)>,
    pub conditional: Vec<(FragmentId, FragmentId, FragmentId)>,
    pub full_resource: Vec<ObjectId>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Seeds {
    pub object: Vec<ObjectId>,
    pub keep: Vec<FragmentId>,
    pub full: Vec<ObjectId>,
}
#[derive(Clone, Debug, Default)]
pub struct State {
    objects: Vec<bool>,
    keeps: Vec<bool>,
    full: Vec<bool>,
}
impl State {
    pub fn object(&self, id: ObjectId) -> bool {
        self.objects.get(id.0 as usize).copied().unwrap_or(false)
    }
    pub fn keep(&self, id: FragmentId) -> bool {
        self.keeps.get(id.0 as usize).copied().unwrap_or(false)
    }
    pub fn full(&self, id: ObjectId) -> bool {
        self.full.get(id.0 as usize).copied().unwrap_or(false)
    }
    pub fn objects(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.objects
            .iter()
            .enumerate()
            .filter_map(|(i, &yes)| yes.then_some(ObjectId(i as u32)))
    }
    pub fn keeps(&self) -> impl Iterator<Item = FragmentId> + '_ {
        self.keeps
            .iter()
            .enumerate()
            .filter_map(|(i, &yes)| yes.then_some(FragmentId(i as u32)))
    }
    pub fn full_objects(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.full
            .iter()
            .enumerate()
            .filter_map(|(i, &yes)| yes.then_some(ObjectId(i as u32)))
    }
}
#[derive(Clone, Debug, Default)]
pub struct Delta {
    pub objects: Vec<ObjectId>,
    pub keeps: Vec<FragmentId>,
    pub full: Vec<ObjectId>,
    pub rounds: usize,
}
impl Delta {
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty() && self.keeps.is_empty() && self.full.is_empty()
    }
}

pub struct Kernel {
    iteration: Iteration,
    object: Variable<(ObjectId, ())>,
    keep: Variable<(FragmentId, ())>,
    full: Variable<(ObjectId, ())>,
    owner: Variable<(FragmentId, ObjectId)>,
    mandatory: Variable<(ObjectId, FragmentId)>,
    contains: Variable<(ObjectId, FragmentId)>,
    reference: Variable<(FragmentId, ObjectId)>,
    member_reference: Variable<(FragmentId, FragmentId)>,
    conditional: Variable<(FragmentId, (FragmentId, FragmentId))>,
    full_resource: Variable<(ObjectId, ())>,
    pending: Variable<(FragmentId, FragmentId)>,
    state: State,
}
impl Kernel {
    pub fn new(objects: usize, fragments: usize) -> Result<Self> {
        let mut iteration = Iteration::new();
        let mut result = Self {
            object: iteration.variable("object"),
            keep: iteration.variable("keep"),
            full: iteration.variable("full"),
            owner: iteration.variable("owner"),
            mandatory: iteration.variable("mandatory"),
            contains: iteration.variable("contains"),
            reference: iteration.variable("reference"),
            member_reference: iteration.variable("member_reference"),
            conditional: iteration.variable("conditional"),
            full_resource: iteration.variable("full_resource"),
            pending: iteration.variable("pending"),
            iteration,
            state: State::default(),
        };
        result.grow(objects, fragments)?;
        Ok(result)
    }
    pub fn grow(&mut self, objects: usize, fragments: usize) -> Result<()> {
        if objects > u32::MAX as usize || fragments > u32::MAX as usize {
            return Err(Error::Invalid("Превышен диапазон ID требований".into()));
        }
        fn grow(bits: &mut Vec<bool>, length: usize) -> Result<()> {
            if length > bits.len() {
                bits.try_reserve(length - bits.len()).map_err(|e| {
                    Error::Invalid(format!("Недостаточно памяти для требований: {e}"))
                })?;
                bits.resize(length, false)
            }
            Ok(())
        }
        grow(&mut self.state.objects, objects)?;
        grow(&mut self.state.full, objects)?;
        grow(&mut self.state.keeps, fragments)
    }
    pub fn state(&self) -> &State {
        &self.state
    }
    pub fn feed(&self, facts: &Relations) -> Result<()> {
        let object = |o: ObjectId| (o.0 as usize) < self.state.objects.len();
        let fragment = |f: FragmentId| (f.0 as usize) < self.state.keeps.len();
        if facts.owner.iter().any(|&(f, o)| !fragment(f) || !object(o))
            || facts
                .mandatory
                .iter()
                .chain(&facts.contains)
                .any(|&(o, f)| !object(o) || !fragment(f))
            || facts
                .reference
                .iter()
                .any(|&(f, o)| !fragment(f) || !object(o))
            || facts
                .member_reference
                .iter()
                .any(|&(a, b)| !fragment(a) || !fragment(b))
            || facts
                .conditional
                .iter()
                .any(|&(a, b, c)| !fragment(a) || !fragment(b) || !fragment(c))
            || facts.full_resource.iter().any(|&o| !object(o))
        {
            return Err(Error::Invalid(
                "ID факта выходит за объявленный диапазон".into(),
            ));
        }
        self.owner.insert(facts.owner.clone().into());
        self.mandatory.insert(facts.mandatory.clone().into());
        self.contains.insert(facts.contains.clone().into());
        self.reference.insert(facts.reference.clone().into());
        self.member_reference
            .insert(facts.member_reference.clone().into());
        self.conditional.insert(
            facts
                .conditional
                .iter()
                .map(|&(a, b, c)| (a, (b, c)))
                .collect(),
        );
        self.full_resource
            .insert(facts.full_resource.iter().map(|&o| (o, ())).collect());
        Ok(())
    }
    pub fn seed(&self, seeds: &Seeds) -> Result<()> {
        if seeds
            .object
            .iter()
            .chain(&seeds.full)
            .any(|o| o.0 as usize >= self.state.objects.len())
            || seeds
                .keep
                .iter()
                .any(|f| f.0 as usize >= self.state.keeps.len())
        {
            return Err(Error::Invalid(
                "ID seed выходит за объявленный диапазон".into(),
            ));
        }
        self.object
            .insert(seeds.object.iter().map(|&o| (o, ())).collect());
        self.keep
            .insert(seeds.keep.iter().map(|&f| (f, ())).collect());
        self.full
            .insert(seeds.full.iter().map(|&o| (o, ())).collect());
        Ok(())
    }
    pub fn close(&mut self, cancel: &Cancellation) -> Result<Delta> {
        let mut delta = Delta::default();
        while self.iteration.changed() {
            cancel.check()?;
            delta.rounds += 1;
            for &(o, ()) in self.object.recent.borrow().iter() {
                if !self.state.objects[o.0 as usize] {
                    self.state.objects[o.0 as usize] = true;
                    delta.objects.push(o)
                }
            }
            for &(f, ()) in self.keep.recent.borrow().iter() {
                if !self.state.keeps[f.0 as usize] {
                    self.state.keeps[f.0 as usize] = true;
                    delta.keeps.push(f)
                }
            }
            for &(o, ()) in self.full.recent.borrow().iter() {
                if !self.state.full[o.0 as usize] {
                    self.state.full[o.0 as usize] = true;
                    delta.full.push(o)
                }
            }
            self.object
                .from_join(&self.keep, &self.owner, |&_f, &(), &o| (o, ()));
            self.object.from_map(&self.full, |&(o, ())| (o, ()));
            self.keep
                .from_join(&self.object, &self.mandatory, |&_o, &(), &f| (f, ()));
            self.full
                .from_join(&self.object, &self.full_resource, |&o, &(), &()| (o, ()));
            self.keep
                .from_join(&self.full, &self.contains, |&_o, &(), &f| (f, ()));
            self.object
                .from_join(&self.keep, &self.reference, |&_f, &(), &o| (o, ()));
            self.keep
                .from_join(&self.keep, &self.member_reference, |&_f, &(), &g| (g, ()));
            self.pending
                .from_join(&self.keep, &self.conditional, |&_a, &(), &(b, c)| (b, c));
            self.keep
                .from_join(&self.pending, &self.keep, |&_b, &c, &()| (c, ()));
        }
        Ok(delta)
    }
}

#[cfg(test)]
#[path = "kernel_tests.rs"]
mod tests;

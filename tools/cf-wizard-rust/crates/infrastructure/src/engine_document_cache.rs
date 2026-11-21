//! Bounds expanded XML between demands; frozen bytes and facts remain authoritative.
use super::SourceDocument;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

pub(super) type Headroom = Arc<dyn Fn() -> Option<u64> + Send + Sync>;

#[derive(Default)]
pub(super) struct CacheBudget {
    cap: Option<u64>,
    limit: u64,
    used: u64,
    fifo: VecDeque<(String, u64)>,
    headroom: Option<Headroom>,
    sampled: Option<Instant>,
    // Estimate the next parse from previously expanded XML in this session.
    expanded_per_source: u64,
    pub evictions: usize,
    pub replays: usize,
    pub high_water: u64,
}

impl CacheBudget {
    pub fn new(cap: Option<u64>, headroom: Option<Headroom>) -> Self {
        Self {
            cap,
            limit: cap.unwrap_or(u64::MAX),
            headroom,
            expanded_per_source: 32,
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub fn resample_on_next_demand(&mut self) {
        self.sampled = None;
    }

    pub fn inventory_window(&self) -> usize {
        // Directory inventories have just four simultaneous readers in the fast path.
        ((self.limit / (128 << 20)).clamp(1, cf_application::INVENTORY_WINDOW_LIMIT as u64))
            as usize
    }

    pub fn preflight(
        &mut self,
        source_bytes: usize,
        documents: &mut BTreeMap<String, Arc<SourceDocument>>,
    ) {
        let Some(cap) = self.cap else { return };
        if self.headroom.is_some()
            && self
                .sampled
                .is_none_or(|last| last.elapsed() >= Duration::from_millis(200))
        {
            self.sampled = Some(Instant::now());
            if let Some(free) = self.headroom.as_ref().and_then(|sample| sample()) {
                let reserve = (cap / 4).min(128 << 20);
                self.limit = self
                    .used
                    .saturating_add(free)
                    .saturating_sub(reserve)
                    .min(cap);
            }
        }
        let expected = (source_bytes as u64).saturating_mul(self.expanded_per_source);
        self.make_room(expected, documents);
    }

    fn make_room(
        &mut self,
        additional: u64,
        documents: &mut BTreeMap<String, Arc<SourceDocument>>,
    ) {
        while self.used.saturating_add(additional) > self.limit {
            let Some((key, weight)) = self.fifo.pop_front() else {
                break;
            };
            documents.remove(&key);
            self.used -= weight;
            self.evictions += 1;
        }
    }

    pub fn insert(
        &mut self,
        key: &str,
        document: &Arc<SourceDocument>,
        documents: &mut BTreeMap<String, Arc<SourceDocument>>,
    ) {
        if self.cap.is_some() {
            let xml_bytes = document.view.xml.source.len() as u64;
            // Preserve the conservative admission weight across address storage changes.
            // This is a cache policy estimate, independent of physical address sharing.
            let addresses: u64 = document.node_ids.iter().map(|id| id.len() as u64).sum();
            let weight = xml_bytes * 2
                + addresses * 4
                + document.node_ids.len() as u64 * 640
                + document.facts.references.len() as u64 * 384
                + document.target_members() as u64 * 128;
            let observed_ratio = weight.div_ceil(xml_bytes.max(1));
            self.expanded_per_source = (self.expanded_per_source * 3 + observed_ratio).div_ceil(4);
            self.make_room(weight, documents);
            if weight > self.limit {
                return;
            }
            self.used += weight;
            self.high_water = self.high_water.max(self.used);
            self.fifo.push_back((key.into(), weight));
        }
        documents.insert(key.into(), Arc::clone(document));
    }
}

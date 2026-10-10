//! Bounded retention of validated visual fragments; hosts supply immutable scope keys.
use std::collections::VecDeque;

const MAX_ENTRIES: usize = 16;
const MAX_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct PaintCache<K> {
    entries: VecDeque<(K, String)>,
    bytes: usize,
}
impl<K> Default for PaintCache<K> {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
        }
    }
}
impl<K: PartialEq> PaintCache<K> {
    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }
    /// Invalidate affected windows without disturbing unaffected recency or the
    /// retained allocation budget.
    pub fn retain(&mut self, mut keep: impl FnMut(&K) -> bool) {
        let bytes = &mut self.bytes;
        self.entries.retain(|(key, value)| {
            if keep(key) {
                true
            } else {
                *bytes -= value.capacity();
                false
            }
        });
    }
    pub fn get(&mut self, key: &K) -> Option<String> {
        let index = self
            .entries
            .iter()
            .position(|(candidate, _)| candidate == key)?;
        let entry = self.entries.remove(index)?;
        let value = entry.1.clone();
        self.entries.push_back(entry);
        Some(value)
    }
    pub fn insert(&mut self, key: K, value: String) {
        if let Some(index) = self
            .entries
            .iter()
            .position(|(candidate, _)| candidate == &key)
            && let Some((_, old)) = self.entries.remove(index)
        {
            self.bytes -= old.capacity();
        }
        if value.capacity() > MAX_BYTES {
            return;
        }
        while self.entries.len() >= MAX_ENTRIES || self.bytes + value.capacity() > MAX_BYTES {
            let Some((_, old)) = self.entries.pop_front() else {
                break;
            };
            self.bytes -= old.capacity();
        }
        self.bytes += value.capacity();
        self.entries.push_back((key, value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalidating_windows_preserves_unaffected_recency_and_allocation_limits() {
        let mut cache = PaintCache::default();
        for key in 0..MAX_ENTRIES {
            cache.insert(key, format!("fragment {key}"));
        }
        cache.get(&0).unwrap();
        cache.retain(|key| key % 2 == 0);
        assert_eq!(cache.entries.front().unwrap().0, 2);
        assert_eq!(cache.entries.back().unwrap().0, 0);
        assert!(cache.get(&1).is_none());
        assert!(cache.get(&0).is_some());
        assert_eq!(
            cache.bytes,
            cache
                .entries
                .iter()
                .map(|(_, value)| value.capacity())
                .sum::<usize>()
        );
        cache.insert(100, "x".repeat(MAX_BYTES));
        assert_eq!(cache.entries.len(), 1);
        assert!(cache.get(&100).is_some());
        cache.retain(|_| false);
        assert!(cache.entries.is_empty());
        assert_eq!(cache.bytes, 0);
    }
    #[test]
    fn bounded_fragments_evict_by_recency_and_allocation_without_retaining_rejected_paint() {
        let mut cache = PaintCache::default();
        for key in 0..MAX_ENTRIES {
            cache.insert(key, format!("fragment {key}"));
        }
        assert!(cache.get(&0).is_some());
        cache.insert(MAX_ENTRIES, "next".into());
        assert!(cache.get(&1).is_none());
        assert!(cache.get(&0).is_some());
        let mut oversized = String::with_capacity(MAX_BYTES + 1);
        oversized.push('x');
        cache.insert(0, oversized);
        assert!(cache.get(&0).is_none());
        cache.insert(100, "x".repeat(MAX_BYTES));
        assert_eq!(cache.entries.len(), 1);
        cache.insert(101, "next".into());
        assert!(cache.get(&100).is_none());
        assert!(cache.bytes <= MAX_BYTES);
        cache.clear();
        assert_eq!(cache.bytes, 0);
        assert!(cache.entries.is_empty());
    }
}

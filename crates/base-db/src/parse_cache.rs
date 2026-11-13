//! The syntax trees of the files parsed most recently.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use syntax::{Language, Parse};

/// How many trees are kept.
const CAPACITY: usize = 128;

/// Recently parsed trees, found by their text's identity: a tree is a pure
/// function of its text, so whichever snapshot asks, the same text gives
/// the same tree, and a changed file's new text simply misses.
#[derive(Default)]
pub struct ParseCache {
    /// Most recently used first.
    entries: Mutex<VecDeque<Arc<Parse>>>,
}

impl ParseCache {
    pub fn get_or_parse(&self, text: &Arc<str>, language: Language) -> Arc<Parse> {
        if let Some(found) = self.take(text, language) {
            self.put(found.clone());
            return found;
        }
        let parsed = Arc::new(syntax::parse(text.clone(), language));
        self.put(parsed.clone());
        parsed
    }

    fn take(&self, text: &Arc<str>, language: Language) -> Option<Arc<Parse>> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let position = entries.iter().position(|entry| {
            Arc::ptr_eq(entry.shared_text(), text) && entry.language() == language
        })?;
        entries.remove(position)
    }

    fn put(&self, parse: Arc<Parse>) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        entries.push_front(parse);
        entries.truncate(CAPACITY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_text_reuses_its_tree_and_a_new_text_gets_its_own() {
        let cache = ParseCache::default();
        let text: Arc<str> = Arc::from("class A");
        let first = cache.get_or_parse(&text, Language::Kotlin);
        let again = cache.get_or_parse(&text, Language::Kotlin);
        assert!(Arc::ptr_eq(&first, &again));
        let equal_but_new: Arc<str> = Arc::from("class A");
        let other = cache.get_or_parse(&equal_but_new, Language::Kotlin);
        assert!(!Arc::ptr_eq(&first, &other));
    }

    #[test]
    fn only_the_most_recent_trees_are_kept() {
        let cache = ParseCache::default();
        let texts: Vec<Arc<str>> = (0..CAPACITY + 1)
            .map(|index| Arc::from(format!("class A{index}").as_str()))
            .collect();
        let first = cache.get_or_parse(&texts[0], Language::Kotlin);
        for text in &texts[1..] {
            cache.get_or_parse(text, Language::Kotlin);
        }
        let reparsed = cache.get_or_parse(&texts[0], Language::Kotlin);
        assert!(!Arc::ptr_eq(&first, &reparsed));
    }
}

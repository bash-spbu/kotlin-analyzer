//! Identifiers interned once per process: a [`Name`] is a `u32`, compared
//! and hashed as one, and spelled out on demand. Source code repeats the same
//! few thousand identifiers millions of times; storing each once keeps
//! declarations compact.

use std::sync::{OnceLock, RwLock};

use rustc_hash::FxHashMap;

/// An interned identifier (`String`, `map`, `kotlin`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Name(u32);

#[derive(Default)]
struct Interner {
    spellings: Vec<&'static str>,
    ids: FxHashMap<&'static str, u32>,
}

fn interner() -> &'static RwLock<Interner> {
    static INTERNER: OnceLock<RwLock<Interner>> = OnceLock::new();
    INTERNER.get_or_init(Default::default)
}

impl Name {
    pub fn new(text: &str) -> Name {
        if let Some(id) = interner()
            .read()
            .expect("interner lock is never poisoned: no code panics while holding it")
            .ids
            .get(text)
        {
            return Name(*id);
        }
        let mut interner = interner()
            .write()
            .expect("interner lock is never poisoned: no code panics while holding it");
        if let Some(id) = interner.ids.get(text) {
            return Name(*id);
        }
        // Names live as long as the process: a bounded set of identifiers.
        let spelling: &'static str = Box::leak(text.to_owned().into_boxed_str());
        let id = interner.spellings.len() as u32;
        interner.spellings.push(spelling);
        interner.ids.insert(spelling, id);
        Name(id)
    }

    pub fn as_str(self) -> &'static str {
        interner()
            .read()
            .expect("interner lock is never poisoned: no code panics while holding it")
            .spellings[self.0 as usize]
    }
}

impl std::fmt::Debug for Name {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:?}", self.as_str())
    }
}

impl std::fmt::Display for Name {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A dotted name: a package (`kotlin.collections`) or a qualified
/// declaration (`java.util.Map.Entry`).
#[derive(Clone, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub struct Path(Box<[Name]>);

impl Path {
    pub fn new(segments: impl IntoIterator<Item = Name>) -> Path {
        Path(segments.into_iter().collect())
    }

    pub fn parse(dotted: &str) -> Path {
        Path::new(
            dotted
                .split('.')
                .filter(|segment| !segment.is_empty())
                .map(Name::new),
        )
    }

    pub fn segments(&self) -> &[Name] {
        &self.0
    }

    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    pub fn last(&self) -> Option<Name> {
        self.0.last().copied()
    }

    /// All segments but the last one.
    pub fn parent(&self) -> Path {
        match self.0.split_last() {
            Some((_, parent)) => Path::new(parent.iter().copied()),
            None => Path::default(),
        }
    }

    pub fn join(&self, name: Name) -> Path {
        Path::new(self.0.iter().copied().chain(std::iter::once(name)))
    }
}

impl std::fmt::Display for Path {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, segment) in self.0.iter().enumerate() {
            if index > 0 {
                formatter.write_str(".")?;
            }
            formatter.write_str(segment.as_str())?;
        }
        Ok(())
    }
}

impl std::fmt::Debug for Path {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_spelling_is_one_name() {
        let first = Name::new("String");
        let second = Name::new(&String::from("String"));
        assert_eq!(first, second);
        assert_ne!(first, Name::new("string"));
        assert_eq!(first.as_str(), "String");
    }

    #[test]
    fn paths_parse_join_and_print() {
        let path = Path::parse("java.util.Map");
        assert_eq!(path.to_string(), "java.util.Map");
        assert_eq!(path.parent().to_string(), "java.util");
        assert_eq!(
            path.parent().join(Name::new("List")),
            Path::parse("java.util.List")
        );
        assert!(Path::parse("").is_root());
    }
}

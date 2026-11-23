//! Identifiers interned once per process: a [`Name`] is a `u32`, compared
//! and hashed as one, and spelled out on demand. Source code repeats the same
//! few thousand identifiers millions of times; storing each once keeps
//! declarations compact.

use std::hash::{BuildHasher, BuildHasherDefault};
use std::sync::{OnceLock, RwLock};

use rustc_hash::{FxHashMap, FxHasher};

/// An interned identifier (`String`, `map`, `kotlin`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Name(u32);

/// How many independently locked parts the spelling → id map has: every
/// analysis thread interns names, and one lock would be their meeting point.
const SHARDS: usize = 64;

struct Interner {
    /// Spellings by id. Append-only and read without a lock.
    spellings: boxcar::Vec<&'static str>,
    /// Ids by spelling, split by the spelling's hash.
    shards: [RwLock<NameShard>; SHARDS],
}

#[derive(Default)]
struct NameShard {
    ids: FxHashMap<&'static str, u32>,
    spellings: SpellingArena,
}

/// Where a shard's spellings are kept: back to back in chunks that live as
/// long as the process — an allocation per chunk, not per name.
#[derive(Default)]
struct SpellingArena {
    /// What is left of the current chunk.
    free: &'static mut [u8],
}

impl SpellingArena {
    const CHUNK: usize = 8 * 1024;
    /// Longer spellings get an allocation of their own, so a chunk's unused
    /// end stays small.
    const LONGEST_IN_CHUNK: usize = 1024;

    fn store(&mut self, text: &str) -> &'static str {
        let length = text.len();
        if length > Self::LONGEST_IN_CHUNK {
            return Box::leak(text.into());
        }
        if self.free.len() < length {
            self.free = Box::leak(vec![0; Self::CHUNK].into_boxed_slice());
        }
        let (stored, rest) = std::mem::take(&mut self.free).split_at_mut(length);
        self.free = rest;
        stored.copy_from_slice(text.as_bytes());
        let stored: &'static [u8] = stored;
        std::str::from_utf8(stored).expect("the bytes of a `str`")
    }
}

fn interner() -> &'static Interner {
    static INTERNER: OnceLock<Interner> = OnceLock::new();
    INTERNER.get_or_init(|| Interner {
        spellings: boxcar::Vec::new(),
        shards: std::array::from_fn(|_| RwLock::default()),
    })
}

impl Name {
    pub fn new(text: &str) -> Name {
        let interner = interner();
        let shard = &interner.shards
            [BuildHasherDefault::<FxHasher>::default().hash_one(text) as usize % SHARDS];
        let lock_held = "interner lock is never poisoned: no code panics while holding it";
        if let Some(id) = shard.read().expect(lock_held).ids.get(text) {
            return Name(*id);
        }
        let mut shard = shard.write().expect(lock_held);
        if let Some(id) = shard.ids.get(text) {
            return Name(*id);
        }
        // Names live as long as the process: a bounded set of identifiers.
        let spelling = shard.spellings.store(text);
        let id = interner.spellings.push(spelling) as u32;
        shard.ids.insert(spelling, id);
        Name(id)
    }

    pub fn as_str(self) -> &'static str {
        interner().spellings[self.0 as usize]
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

/// A value interned once per process: equal values are one value, so an
/// `Interned` is a pointer — compared and hashed as one — and the value is
/// stored once however many declarations name it (`String`, `List<T>`).
/// Like names, interned values live as long as the process.
pub struct Interned<T: 'static>(&'static T);

/// A type whose values can be interned: it says where they're kept.
pub trait Internable: Eq + std::hash::Hash + Send + Sync + Sized + 'static {
    fn storage() -> &'static InternStorage<Self>;
}

/// Where a type's interned values are: side by side in an append-only
/// vector whose elements never move (an allocation per bucket, not per
/// value), found through sets split like names' ids so threads interning at
/// once rarely meet at one lock.
pub struct InternStorage<T: 'static> {
    values: boxcar::Vec<T>,
    shards: [RwLock<rustc_hash::FxHashSet<&'static T>>; SHARDS],
}

impl<T> Default for InternStorage<T> {
    fn default() -> InternStorage<T> {
        InternStorage {
            values: boxcar::Vec::new(),
            shards: std::array::from_fn(|_| RwLock::default()),
        }
    }
}

impl<T: Internable> Interned<T> {
    pub fn new(value: T) -> Interned<T> {
        let storage = T::storage();
        let shard = &storage.shards
            [BuildHasherDefault::<FxHasher>::default().hash_one(&value) as usize % SHARDS];
        let lock_held = "interner lock is never poisoned: no code panics while holding it";
        if let Some(found) = shard.read().expect(lock_held).get(&value) {
            return Interned(found);
        }
        let mut values = shard.write().expect(lock_held);
        if let Some(found) = values.get(&value) {
            return Interned(found);
        }
        let index = storage.values.push(value);
        let stored: &'static T = &storage.values[index];
        values.insert(stored);
        Interned(stored)
    }
}

impl<T> std::ops::Deref for Interned<T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.0
    }
}

impl<T> AsRef<T> for Interned<T> {
    fn as_ref(&self) -> &T {
        self.0
    }
}

impl<T> Clone for Interned<T> {
    fn clone(&self) -> Interned<T> {
        *self
    }
}

impl<T> Copy for Interned<T> {}

/// One value per content: the same address is the same value.
impl<T> PartialEq for Interned<T> {
    fn eq(&self, other: &Interned<T>) -> bool {
        std::ptr::eq(self.0, other.0)
    }
}

impl<T> Eq for Interned<T> {}

impl<T> std::hash::Hash for Interned<T> {
    fn hash<State: std::hash::Hasher>(&self, state: &mut State) {
        std::ptr::hash(self.0, state);
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for Interned<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
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

/// Written as its text: an interned name's number is this process's.
impl serde::Serialize for Name {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for Name {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Name, D::Error> {
        let text = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        Ok(Name::new(&text))
    }
}

/// Written as its segments' texts.
impl serde::Serialize for Path {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.segments().serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for Path {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Path, D::Error> {
        let segments = Vec::<Name>::deserialize(deserializer)?;
        Ok(Path::new(segments))
    }
}

/// Written as the value; read back, interned again.
impl<T: serde::Serialize> serde::Serialize for Interned<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de, T: Internable + serde::Deserialize<'de>> serde::Deserialize<'de> for Interned<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Interned<T>, D::Error> {
        T::deserialize(deserializer).map(Interned::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(PartialEq, Eq, Hash, Debug)]
    struct Shape(Vec<u32>);

    impl Internable for Shape {
        fn storage() -> &'static InternStorage<Shape> {
            static STORAGE: OnceLock<InternStorage<Shape>> = OnceLock::new();
            STORAGE.get_or_init(InternStorage::default)
        }
    }

    #[test]
    fn spellings_are_kept_whole_across_chunks() {
        let mut arena = SpellingArena::default();
        let texts: Vec<String> = (0..5000).map(|index| format!("name_{index}_ä")).collect();
        let stored: Vec<&'static str> = texts.iter().map(|text| arena.store(text)).collect();
        assert_eq!(stored, texts);
        let long = "x".repeat(SpellingArena::LONGEST_IN_CHUNK + 1);
        assert_eq!(arena.store(&long), long);
        assert_eq!(arena.store(""), "");
        let spelled: Vec<&str> = texts.iter().map(|text| Name::new(text).as_str()).collect();
        assert_eq!(spelled, texts);
    }

    #[test]
    fn names_paths_and_interned_values_are_written_as_text_and_read_back_as_the_same() {
        #[derive(PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
        struct Shape(String);
        impl Internable for Shape {
            fn storage() -> &'static InternStorage<Shape> {
                static STORAGE: std::sync::OnceLock<InternStorage<Shape>> =
                    std::sync::OnceLock::new();
                STORAGE.get_or_init(InternStorage::default)
            }
        }
        let name = Name::new("written-name");
        let bytes = postcard::to_stdvec(&name).unwrap();
        assert!(bytes.ends_with(b"written-name"), "the text, not a number");
        assert_eq!(postcard::from_bytes::<Name>(&bytes).unwrap(), name);
        let path = Path::parse("com.example.Client");
        let bytes = postcard::to_stdvec(&path).unwrap();
        assert_eq!(postcard::from_bytes::<Path>(&bytes).unwrap(), path);
        let shape = Interned::new(Shape("circle".to_owned()));
        let bytes = postcard::to_stdvec(&shape).unwrap();
        let read: Interned<Shape> = postcard::from_bytes(&bytes).unwrap();
        assert!(
            std::ptr::eq(&*read, &*shape),
            "interned again: the same value"
        );
    }

    #[test]
    fn equal_values_intern_to_one_and_unequal_ones_to_two() {
        let first = Interned::new(Shape(vec![1, 2]));
        let again = Interned::new(Shape(vec![1, 2]));
        let other = Interned::new(Shape(vec![2, 1]));
        assert!(std::ptr::eq(&*first, &*again), "stored once");
        assert_eq!(first, again);
        assert_ne!(first, other);
        assert_eq!(first.0.0, vec![1, 2], "read through the pointer");
        let across_threads: Vec<Interned<Shape>> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| Interned::new(Shape(vec![7]))))
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect()
        });
        assert!(across_threads.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[test]
    fn threads_interning_the_same_spellings_agree() {
        let spellings: Vec<String> = (0..500).map(|index| format!("shared{index}")).collect();
        let per_thread: Vec<Vec<Name>> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| spellings.iter().map(|text| Name::new(text)).collect()))
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect()
        });
        for names in &per_thread[1..] {
            assert_eq!(names, &per_thread[0]);
        }
        for (name, text) in per_thread[0].iter().zip(&spellings) {
            assert_eq!(name.as_str(), text);
        }
    }

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

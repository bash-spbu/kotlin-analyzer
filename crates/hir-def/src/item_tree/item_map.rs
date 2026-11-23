//! The lookups an [`ItemTree`](super::ItemTree) keeps, as sorted slices:
//! a tree is built once and read many times, so it holds no hash table's
//! empty slots and no allocation per key.

use intern::Name;

use super::ItemId;

/// Values by item: sorted by id, found by binary search.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct ItemMap<V>(Box<[(ItemId, V)]>);

impl<V> Default for ItemMap<V> {
    fn default() -> ItemMap<V> {
        ItemMap(Box::new([]))
    }
}

impl<V> ItemMap<V> {
    /// Every entry, by id.
    pub(crate) fn entries(&self) -> &[(ItemId, V)] {
        &self.0
    }

    pub(crate) fn get(&self, id: ItemId) -> Option<&V> {
        let found = self.0.binary_search_by_key(&id, |(key, _)| *key).ok()?;
        Some(&self.0[found].1)
    }
}

impl<V> FromIterator<(ItemId, V)> for ItemMap<V> {
    /// Each id once: ids are unique in their tree.
    fn from_iter<I: IntoIterator<Item = (ItemId, V)>>(entries: I) -> ItemMap<V> {
        let mut entries: Vec<(ItemId, V)> = entries.into_iter().collect();
        entries.sort_unstable_by_key(|(id, _)| *id);
        ItemMap(entries.into_boxed_slice())
    }
}

/// Which declarations a parent has under a name.
type NameKey = (Option<ItemId>, Name);

/// Items by parent (`None`: top level) and name: the keys sorted, each
/// key's items one run of `items` in document order.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(crate) struct NamedItems {
    keys: Box<[NameKey]>,
    items: Box<[ItemId]>,
}

impl NamedItems {
    /// `items` in document order, each with its parent and name.
    pub(crate) fn new(items: impl Iterator<Item = (NameKey, ItemId)>) -> NamedItems {
        let mut entries: Vec<(NameKey, ItemId)> = items.collect();
        // Stable: a key's items keep their document order.
        entries.sort_by_key(|(key, _)| *key);
        let (keys, items): (Vec<NameKey>, Vec<ItemId>) = entries.into_iter().unzip();
        NamedItems {
            keys: keys.into_boxed_slice(),
            items: items.into_boxed_slice(),
        }
    }

    pub(crate) fn get(&self, parent: Option<ItemId>, name: Name) -> &[ItemId] {
        let key = (parent, name);
        let start = self.keys.partition_point(|candidate| *candidate < key);
        let end = self.keys.partition_point(|candidate| *candidate <= key);
        &self.items[start..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_is_found_by_its_id_whatever_order_it_came_in() {
        let map: ItemMap<&str> = [(ItemId(30), "c"), (ItemId(10), "a"), (ItemId(20), "b")]
            .into_iter()
            .collect();
        assert_eq!(map.get(ItemId(10)), Some(&"a"));
        assert_eq!(map.get(ItemId(30)), Some(&"c"));
        assert_eq!(map.get(ItemId(15)), None);
        assert_eq!(ItemMap::<u32>::default().get(ItemId(10)), None);
    }

    #[test]
    fn a_names_items_are_its_parents_own_in_document_order() {
        let pay = Name::new("pay");
        let charge = Name::new("charge");
        let class = Some(ItemId(7));
        let named = NamedItems::new(
            [
                ((None, pay), ItemId(50)),
                ((class, pay), ItemId(40)),
                ((None, charge), ItemId(30)),
                ((None, pay), ItemId(20)),
                ((class, pay), ItemId(10)),
            ]
            .into_iter(),
        );
        assert_eq!(named.get(None, pay), [ItemId(50), ItemId(20)]);
        assert_eq!(named.get(class, pay), [ItemId(40), ItemId(10)]);
        assert_eq!(named.get(None, charge), [ItemId(30)]);
        assert_eq!(named.get(class, charge), []);
    }
}

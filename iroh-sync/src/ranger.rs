//! Implementation of Set Reconcilliation based on
//! "Range-Based Set Reconciliation" by Aljoscha Meyer.
//!

use std::cmp::Ordering;
use std::fmt::Debug;
use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

/// Store entries that can be fingerprinted and put into ranges.
pub trait RangeEntry: Debug + Clone + PartialOrd {
    /// The key for this entry, to be used in ranges.
    type Key: RangeKey + PartialEq + Clone + Default + Debug;
    /// Get the key for this entry.
    fn key(&self) -> &Self::Key;
    /// Get the fingerprint for this entry.
    fn as_fingerprint(&self) -> Fingerprint;
}

/// Stores a range.
///
/// There are three possibilities
/// - x, x: All elements in a set, denoted with
/// - [x, y): x < y: Includes x, but not y
/// - S \ [y, x) y < x: Includes x, but not y.
/// This means that ranges are "wrap around" conceptually.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct Range<K> {
    x: K,
    y: K,
}

impl<K> Range<K> {
    pub fn x(&self) -> &K {
        &self.x
    }

    pub fn y(&self) -> &K {
        &self.y
    }

    pub fn new(x: K, y: K) -> Self {
        Range { x, y }
    }

    pub fn map<X>(self, f: impl FnOnce(K, K) -> (X, X)) -> Range<X> {
        let (x, y) = f(self.x, self.y);
        Range { x, y }
    }
}

impl<K: Ord> Range<K> {
    pub fn is_all(&self) -> bool {
        self.x() == self.y()
    }

    pub fn contains(&self, t: &K) -> bool {
        match self.x().cmp(self.y()) {
            Ordering::Equal => true,
            Ordering::Less => self.x() <= t && t < self.y(),
            Ordering::Greater => self.x() <= t || t < self.y(),
        }
    }
}

impl<K> From<(K, K)> for Range<K> {
    fn from((x, y): (K, K)) -> Self {
        Range { x, y }
    }
}

pub trait RangeKey: Sized + Ord + Debug {}

impl RangeKey for &str {}
impl RangeKey for &[u8] {}
impl RangeKey for Vec<u8> {}
impl RangeKey for String {}

#[derive(Copy, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fingerprint(pub [u8; 32]);

impl Debug for Fingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Fp({})", blake3::Hash::from(self.0).to_hex())
    }
}

impl Fingerprint {
    /// The fingerprint of the empty set
    pub fn empty() -> Self {
        Fingerprint(*blake3::hash(&[]).as_bytes())
    }

    pub fn new<T: RangeEntry>(val: T) -> Self {
        val.as_fingerprint()
    }
}

impl std::ops::BitXorAssign for Fingerprint {
    fn bitxor_assign(&mut self, rhs: Self) {
        for (a, b) in self.0.iter_mut().zip(rhs.0.iter()) {
            *a ^= b;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RangeFingerprint<K> {
    #[serde(bound(
        serialize = "Range<K>: Serialize",
        deserialize = "Range<K>: Deserialize<'de>"
    ))]
    pub range: Range<K>,
    /// The fingerprint of `range`.
    pub fingerprint: Fingerprint,
}

/// Transfers items inside a range to the other participant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RangeItem<E: RangeEntry> {
    /// The range out of which the elements are.
    #[serde(bound(
        serialize = "Range<E::Key>: Serialize",
        deserialize = "Range<E::Key>: Deserialize<'de>"
    ))]
    pub range: Range<E::Key>,
    #[serde(bound(serialize = "E: Serialize", deserialize = "E: Deserialize<'de>"))]
    pub values: Vec<E>,
    /// If false, requests to send local items in the range.
    /// Otherwise not.
    pub have_local: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MessagePart<E: RangeEntry> {
    #[serde(bound(
        serialize = "RangeFingerprint<E::Key>: Serialize",
        deserialize = "RangeFingerprint<E::Key>: Deserialize<'de>"
    ))]
    RangeFingerprint(RangeFingerprint<E::Key>),
    #[serde(bound(
        serialize = "RangeItem<E>: Serialize",
        deserialize = "RangeItem<E>: Deserialize<'de>"
    ))]
    RangeItem(RangeItem<E>),
}

impl<E: RangeEntry> MessagePart<E> {
    pub fn is_range_fingerprint(&self) -> bool {
        matches!(self, MessagePart::RangeFingerprint(_))
    }

    pub fn is_range_item(&self) -> bool {
        matches!(self, MessagePart::RangeItem(_))
    }

    pub fn values(&self) -> Option<&[E]> {
        match self {
            MessagePart::RangeFingerprint(_) => None,
            MessagePart::RangeItem(RangeItem { values, .. }) => Some(values),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message<E: RangeEntry> {
    #[serde(bound(
        serialize = "MessagePart<E>: Serialize",
        deserialize = "MessagePart<E>: Deserialize<'de>"
    ))]
    parts: Vec<MessagePart<E>>,
}

impl<E: RangeEntry> Message<E> {
    /// Construct the initial message.
    fn init<S: Store<E>>(store: &S) -> Result<Self, S::Error> {
        let x = store.get_first()?;
        let range = Range::new(x.clone(), x);
        let fingerprint = store.get_fingerprint(&range)?;
        let part = MessagePart::RangeFingerprint(RangeFingerprint { range, fingerprint });
        Ok(Message { parts: vec![part] })
    }

    pub fn parts(&self) -> &[MessagePart<E>] {
        &self.parts
    }
}

pub trait Store<E: RangeEntry>: Sized {
    type Error: Debug + Send + Sync + Into<anyhow::Error>;

    /// Get a the first key (or the default if none is available).
    fn get_first(&self) -> Result<E::Key, Self::Error>;
    fn get(&self, key: &E::Key) -> Result<Option<E>, Self::Error>;
    fn len(&self) -> Result<usize, Self::Error>;
    fn is_empty(&self) -> Result<bool, Self::Error>;
    /// Calculate the fingerprint of the given range.
    fn get_fingerprint(&self, range: &Range<E::Key>) -> Result<Fingerprint, Self::Error>;

    /// Insert the given key value pair.
    fn put(&mut self, entry: E) -> Result<(), Self::Error>;

    type RangeIterator<'a>: Iterator<Item = Result<E, Self::Error>>
    where
        Self: 'a,
        E: 'a;

    /// Returns all items in the given range
    fn get_range(&self, range: Range<E::Key>) -> Result<Self::RangeIterator<'_>, Self::Error>;

    /// Get all entries in the store
    fn all(&self) -> Result<Self::RangeIterator<'_>, Self::Error>;

    /// Remove an entry from the store.
    fn remove(&mut self, key: &E::Key) -> Result<Option<E>, Self::Error>;
}

#[derive(Debug)]
pub struct Peer<E: RangeEntry, S: Store<E>> {
    store: S,
    /// Up to how many values to send immediately, before sending only a fingerprint.
    max_set_size: usize,
    /// `k` in the protocol, how many splits to generate. at least 2
    split_factor: usize,

    /// This is needed because the `E: RangeEntry` would be unused otherwise.
    /// Only having it referenced in the `S: Store` generic doesn't satisfy rustc.
    _phantom: PhantomData<E>,
}

impl<E, S> Default for Peer<E, S>
where
    E: RangeEntry,
    S: Store<E> + Default,
{
    fn default() -> Self {
        Peer {
            store: S::default(),
            max_set_size: 1,
            split_factor: 2,
            _phantom: Default::default(),
        }
    }
}

impl<E, S> Peer<E, S>
where
    E: RangeEntry,
    S: Store<E>,
{
    pub fn from_store(store: S) -> Self {
        Peer {
            store,
            max_set_size: 1,
            split_factor: 2,
            _phantom: Default::default(),
        }
    }

    /// Generates the initial message.
    pub fn initial_message(&self) -> Result<Message<E>, S::Error> {
        Message::init(&self.store)
    }

    /// Processes an incoming message and produces a response.
    /// If terminated, returns `None`
    ///
    /// `validate_cb` is called before an entry received from the remote is inserted into the store.
    /// It must return true if the entry is valid and should be stored, and false otherwise
    /// (which means the entry will be dropped and not stored).
    pub fn process_message<F>(
        &mut self,
        message: Message<E>,
        validate_cb: F,
    ) -> Result<Option<Message<E>>, S::Error>
    where
        F: Fn(&S, &E) -> bool,
    {
        let mut out = Vec::new();

        // TODO: can these allocs be avoided?
        let mut items = Vec::new();
        let mut fingerprints = Vec::new();
        for part in message.parts {
            match part {
                MessagePart::RangeItem(item) => {
                    items.push(item);
                }
                MessagePart::RangeFingerprint(fp) => {
                    fingerprints.push(fp);
                }
            }
        }

        // Process item messages
        for RangeItem {
            range,
            values,
            have_local,
        } in items
        {
            let diff: Option<Vec<_>> = if have_local {
                None
            } else {
                Some(
                    self.store
                        .get_range(range.clone())?
                        .filter_map(|existing| match existing {
                            Ok(existing) => {
                                if !values.iter().any(|entry| existing.key() == entry.key()) {
                                    Some(Ok(existing))
                                } else {
                                    None
                                }
                            }
                            Err(err) => Some(Err(err)),
                        })
                        .collect::<Result<_, _>>()?,
                )
            };

            // Store incoming values
            for entry in values {
                if validate_cb(&self.store, &entry) {
                    self.store.put(entry)?;
                }
            }

            if let Some(diff) = diff {
                if !diff.is_empty() {
                    out.push(MessagePart::RangeItem(RangeItem {
                        range,
                        values: diff,
                        have_local: true,
                    }));
                }
            }
        }

        // Process fingerprint messages
        for RangeFingerprint { range, fingerprint } in fingerprints {
            let local_fingerprint = self.store.get_fingerprint(&range)?;
            // Case1 Match, nothing to do
            if local_fingerprint == fingerprint {
                continue;
            }

            // Case2 Recursion Anchor
            // TODO: This is hugely inefficient and needs to be optimized
            // For an identity range that includes everything we allocate a vec with all entries of
            // the replica here.
            let local_values: Vec<_> = self
                .store
                .get_range(range.clone())?
                .collect::<Result<_, _>>()?;
            if local_values.len() <= 1 || fingerprint == Fingerprint::empty() {
                out.push(MessagePart::RangeItem(RangeItem {
                    range,
                    values: local_values,
                    have_local: false,
                }));
            } else {
                // Case3 Recurse
                // Create partition
                // m0 = x < m1 < .. < mk = y, with k>= 2
                // such that [ml, ml+1) is nonempty
                let mut ranges = Vec::with_capacity(self.split_factor);

                // Select the first index, for which the key is larger or equal than the x of the range.
                let start_index = local_values
                    .iter()
                    .position(|el| el.key() >= range.x())
                    .unwrap_or(0);
                // select a pivot value. pivots repeat every split_factor, so pivot(i) == pivot(i + self.split_factor * x)
                // it is guaranteed that pivot(0) != x if local_values.len() >= 2
                let pivot = |i: usize| {
                    // ensure that pivots wrap around
                    let i = i % self.split_factor;
                    // choose an offset. this will be
                    // 1/2, 1 in case of split_factor == 2
                    // 1/3, 2/3, 1 in case of split_factor == 3
                    // etc.
                    let offset = (local_values.len() * (i + 1)) / self.split_factor;
                    let offset = (start_index + offset) % local_values.len();
                    local_values[offset].key()
                };
                if range.is_all() {
                    // the range is the whole set, so range.x and range.y should not matter
                    // just add all ranges as normal ranges. Exactly one of the ranges will
                    // wrap around, so we cover the entire set.
                    for i in 0..self.split_factor {
                        let (x, y) = (pivot(i), pivot(i + 1));
                        // don't push empty ranges
                        if x != y {
                            ranges.push(Range {
                                x: x.clone(),
                                y: y.clone(),
                            })
                        }
                    }
                } else {
                    // guaranteed to be non-empty because
                    // - pivot(0) is guaranteed to be != x for local_values.len() >= 2
                    // - local_values.len() < 2 gets handled by the recursion anchor
                    // - x != y (regular range)
                    ranges.push(Range {
                        x: range.x().clone(),
                        y: pivot(0).clone(),
                    });
                    // this will only be executed for split_factor > 2
                    for i in 0..self.split_factor - 2 {
                        // don't push empty ranges
                        let (x, y) = (pivot(i), pivot(i + 1));
                        if x != y {
                            ranges.push(Range {
                                x: x.clone(),
                                y: y.clone(),
                            })
                        }
                    }
                    // guaranteed to be non-empty because
                    // - pivot is a value in the range
                    // - y is the exclusive end of the range
                    // - x != y (regular range)
                    ranges.push(Range {
                        x: pivot(self.split_factor - 2).clone(),
                        y: range.y().clone(),
                    });
                }

                let mut non_empty = 0;
                for range in ranges {
                    let chunk: Vec<_> = self.store.get_range(range.clone())?.collect();
                    if !chunk.is_empty() {
                        non_empty += 1;
                    }
                    // Add either the fingerprint or the item set
                    let fingerprint = self.store.get_fingerprint(&range)?;
                    if chunk.len() > self.max_set_size {
                        out.push(MessagePart::RangeFingerprint(RangeFingerprint {
                            range: range.clone(),
                            fingerprint,
                        }));
                    } else {
                        let values = chunk.into_iter().collect::<Result<_, _>>()?;
                        out.push(MessagePart::RangeItem(RangeItem {
                            range,
                            values,
                            have_local: false,
                        }));
                    }
                }
                debug_assert!(non_empty > 1);
            }
        }

        // If we have any parts, return a message
        if !out.is_empty() {
            Ok(Some(Message { parts: out }))
        } else {
            Ok(None)
        }
    }

    /// Insert a key value pair.
    pub fn put(&mut self, entry: E) -> Result<(), S::Error> {
        self.store.put(entry)
    }

    /// List all existing key value pairs.
    // currently unused outside of tests
    #[cfg(test)]
    pub fn all(&self) -> Result<impl Iterator<Item = Result<E, S::Error>> + '_, S::Error> {
        self.store.all()
    }

    // /// Get the entry for the given key.
    // pub fn get(&self, k: &K) -> Result<Option<V>, S::Error> {
    //     self.store.get(k)
    // }
    // /// Remove the given key.
    // pub fn remove(&mut self, k: &K) -> Result<Vec<V>, S::Error> {
    //     self.store.remove(k)
    // }

    /// Returns a refernce to the underlying store.
    pub(crate) fn store(&self) -> &S {
        &self.store
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use std::{cell::RefCell, collections::BTreeMap, convert::Infallible, fmt::Debug, rc::Rc};
    use test_strategy::proptest;

    use super::*;

    #[derive(Debug)]
    struct SimpleStore<K, V> {
        data: BTreeMap<K, V>,
    }

    impl<K, V> Default for SimpleStore<K, V> {
        fn default() -> Self {
            SimpleStore {
                data: BTreeMap::default(),
            }
        }
    }

    impl<K, V> RangeEntry for (K, V)
    where
        K: RangeKey + PartialEq + Clone + Default + Debug,
        V: Debug + Clone + PartialOrd,
    {
        type Key = K;

        fn key(&self) -> &Self::Key {
            &self.0
        }

        fn as_fingerprint(&self) -> Fingerprint {
            let mut hasher = blake3::Hasher::new();
            hasher.update(format!("{:?}", self.0).as_bytes());
            hasher.update(format!("{:?}", self.1).as_bytes());
            Fingerprint(hasher.finalize().into())
        }
    }

    impl<K, V> Store<(K, V)> for SimpleStore<K, V>
    where
        K: RangeKey + PartialEq + Clone + Default + Debug,
        V: Debug + Clone + PartialOrd,
    {
        type Error = Infallible;

        fn get_first(&self) -> Result<K, Self::Error> {
            if let Some((k, _)) = self.data.first_key_value() {
                Ok(k.clone())
            } else {
                Ok(Default::default())
            }
        }

        fn get(&self, key: &K) -> Result<Option<(K, V)>, Self::Error> {
            Ok(self.data.get(key).cloned().map(|v| (key.clone(), v)))
        }

        fn len(&self) -> Result<usize, Self::Error> {
            Ok(self.data.len())
        }

        fn is_empty(&self) -> Result<bool, Self::Error> {
            Ok(self.data.is_empty())
        }

        /// Calculate the fingerprint of the given range.
        fn get_fingerprint(&self, range: &Range<K>) -> Result<Fingerprint, Self::Error> {
            let elements = self.get_range(range.clone())?;
            let mut fp = Fingerprint::empty();
            for el in elements {
                let el = el?;
                fp ^= el.as_fingerprint();
            }

            Ok(fp)
        }

        /// Insert the given key value pair.
        fn put(&mut self, e: (K, V)) -> Result<(), Self::Error> {
            self.data.insert(e.0, e.1);
            Ok(())
        }

        type RangeIterator<'a> = SimpleRangeIterator<'a, K, V>
        where K: 'a, V: 'a;
        /// Returns all items in the given range
        fn get_range(&self, range: Range<K>) -> Result<Self::RangeIterator<'_>, Self::Error> {
            // TODO: this is not very efficient, optimize depending on data structure
            let iter = self.data.iter();

            Ok(SimpleRangeIterator {
                iter,
                range: Some(range),
            })
        }

        fn remove(&mut self, key: &K) -> Result<Option<(K, V)>, Self::Error> {
            let res = self.data.remove(key).map(|v| (key.clone(), v));
            Ok(res)
        }

        fn all(&self) -> Result<Self::RangeIterator<'_>, Self::Error> {
            let iter = self.data.iter();

            Ok(SimpleRangeIterator { iter, range: None })
        }
    }

    #[derive(Debug)]
    pub struct SimpleRangeIterator<'a, K, V> {
        iter: std::collections::btree_map::Iter<'a, K, V>,
        range: Option<Range<K>>,
    }

    impl<'a, K, V> Iterator for SimpleRangeIterator<'a, K, V>
    where
        K: Clone + Ord,
        V: Clone,
    {
        type Item = Result<(K, V), Infallible>;

        fn next(&mut self) -> Option<Self::Item> {
            let mut next = self.iter.next()?;

            let filter = |x: &K| match &self.range {
                None => true,
                Some(ref range) => range.contains(x),
            };

            loop {
                if filter(next.0) {
                    return Some(Ok((next.0.clone(), next.1.clone())));
                }

                next = self.iter.next()?;
            }
        }
    }

    #[test]
    fn test_paper_1() {
        let alice_set = [("ape", 1), ("eel", 1), ("fox", 1), ("gnu", 1)];
        let bob_set = [
            ("bee", 1),
            ("cat", 1),
            ("doe", 1),
            ("eel", 1),
            ("fox", 1),
            ("hog", 1),
        ];

        let res = sync(&alice_set, &bob_set);
        res.print_messages();
        assert_eq!(res.alice_to_bob.len(), 3, "A -> B message count");
        assert_eq!(res.bob_to_alice.len(), 2, "B -> A message count");

        // Initial message
        assert_eq!(res.alice_to_bob[0].parts.len(), 1);
        assert!(res.alice_to_bob[0].parts[0].is_range_fingerprint());

        // Response from Bob - recurse once
        assert_eq!(res.bob_to_alice[0].parts.len(), 2);
        assert!(res.bob_to_alice[0].parts[0].is_range_fingerprint());
        assert!(res.bob_to_alice[0].parts[1].is_range_fingerprint());

        // Last response from Alice
        assert_eq!(res.alice_to_bob[1].parts.len(), 3);
        assert!(res.alice_to_bob[1].parts[0].is_range_fingerprint());
        assert!(res.alice_to_bob[1].parts[1].is_range_fingerprint());
        assert!(res.alice_to_bob[1].parts[2].is_range_item());

        // Last response from Bob
        assert_eq!(res.bob_to_alice[1].parts.len(), 2);
        assert!(res.bob_to_alice[1].parts[0].is_range_item());
        assert!(res.bob_to_alice[1].parts[1].is_range_item());
    }

    #[test]
    fn test_paper_2() {
        let alice_set = [
            ("ape", 1),
            ("bee", 1),
            ("cat", 1),
            ("doe", 1),
            ("eel", 1),
            ("fox", 1), // the only value being sent
            ("gnu", 1),
            ("hog", 1),
        ];
        let bob_set = [
            ("ape", 1),
            ("bee", 1),
            ("cat", 1),
            ("doe", 1),
            ("eel", 1),
            ("gnu", 1),
            ("hog", 1),
        ];

        let res = sync(&alice_set, &bob_set);
        assert_eq!(res.alice_to_bob.len(), 3, "A -> B message count");
        assert_eq!(res.bob_to_alice.len(), 2, "B -> A message count");
    }

    #[test]
    fn test_paper_3() {
        let alice_set = [
            ("ape", 1),
            ("bee", 1),
            ("cat", 1),
            ("doe", 1),
            ("eel", 1),
            ("fox", 1),
            ("gnu", 1),
            ("hog", 1),
        ];
        let bob_set = [("ape", 1), ("cat", 1), ("eel", 1), ("gnu", 1)];

        let res = sync(&alice_set, &bob_set);
        assert_eq!(res.alice_to_bob.len(), 3, "A -> B message count");
        assert_eq!(res.bob_to_alice.len(), 2, "B -> A message count");
    }

    #[test]
    fn test_limits() {
        let alice_set = [("ape", 1), ("bee", 1), ("cat", 1)];
        let bob_set = [("ape", 1), ("cat", 1), ("doe", 1)];

        let res = sync(&alice_set, &bob_set);
        assert_eq!(res.alice_to_bob.len(), 2, "A -> B message count");
        assert_eq!(res.bob_to_alice.len(), 2, "B -> A message count");
    }

    #[test]
    fn test_prefixes_simple() {
        let alice_set = [("/foo/bar", 1), ("/foo/baz", 1), ("/foo/cat", 1)];
        let bob_set = [("/foo/bar", 1), ("/alice/bar", 1), ("/alice/baz", 1)];

        let res = sync(&alice_set, &bob_set);
        assert_eq!(res.alice_to_bob.len(), 2, "A -> B message count");
        assert_eq!(res.bob_to_alice.len(), 2, "B -> A message count");
    }

    #[test]
    fn test_prefixes_empty_alice() {
        let alice_set = [];
        let bob_set = [("/foo/bar", 1), ("/alice/bar", 1), ("/alice/baz", 1)];

        let res = sync(&alice_set, &bob_set);
        assert_eq!(res.alice_to_bob.len(), 1, "A -> B message count");
        assert_eq!(res.bob_to_alice.len(), 1, "B -> A message count");
    }

    #[test]
    fn test_prefixes_empty_bob() {
        let alice_set = [("/foo/bar", 1), ("/foo/baz", 1), ("/foo/cat", 1)];
        let bob_set = [];

        let res = sync(&alice_set, &bob_set);
        assert_eq!(res.alice_to_bob.len(), 2, "A -> B message count");
        assert_eq!(res.bob_to_alice.len(), 1, "B -> A message count");
    }

    #[test]
    fn test_multikey() {
        #[derive(Default, Clone, PartialEq, Eq, PartialOrd, Ord)]
        struct Multikey {
            author: [u8; 4],
            key: Vec<u8>,
        }

        impl RangeKey for Multikey {}

        impl Debug for Multikey {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                let key = if let Ok(key) = std::str::from_utf8(&self.key) {
                    key.to_string()
                } else {
                    hex::encode(&self.key)
                };
                f.debug_struct("Multikey")
                    .field("author", &hex::encode(self.author))
                    .field("key", &key)
                    .finish()
            }
        }

        impl Multikey {
            fn new(author: [u8; 4], key: impl AsRef<[u8]>) -> Self {
                Multikey {
                    author,
                    key: key.as_ref().to_vec(),
                }
            }
        }
        let author_a = [1u8; 4];
        let author_b = [2u8; 4];
        let alice_set = [
            (Multikey::new(author_a, "ape"), 1),
            (Multikey::new(author_a, "bee"), 1),
            (Multikey::new(author_b, "bee"), 1),
            (Multikey::new(author_a, "doe"), 1),
        ];
        let bob_set = [
            (Multikey::new(author_a, "ape"), 1),
            (Multikey::new(author_a, "bee"), 1),
            (Multikey::new(author_a, "cat"), 1),
            (Multikey::new(author_b, "cat"), 1),
        ];

        // No limit
        let res = sync(&alice_set, &bob_set);
        assert_eq!(res.alice_to_bob.len(), 2, "A -> B message count");
        assert_eq!(res.bob_to_alice.len(), 2, "B -> A message count");
        res.assert_alice_set(
            "no limit",
            &[
                (Multikey::new(author_a, "ape"), 1),
                (Multikey::new(author_a, "bee"), 1),
                (Multikey::new(author_b, "bee"), 1),
                (Multikey::new(author_a, "doe"), 1),
                (Multikey::new(author_a, "cat"), 1),
                (Multikey::new(author_b, "cat"), 1),
            ],
        );

        res.assert_bob_set(
            "no limit",
            &[
                (Multikey::new(author_a, "ape"), 1),
                (Multikey::new(author_a, "bee"), 1),
                (Multikey::new(author_b, "bee"), 1),
                (Multikey::new(author_a, "doe"), 1),
                (Multikey::new(author_a, "cat"), 1),
                (Multikey::new(author_b, "cat"), 1),
            ],
        );
    }

    // This tests two things:
    // 1) validate cb returning false leads to no changes on both sides after sync
    // 2) validate cb receives expected entries
    #[test]
    fn test_validate_cb() {
        let alice_set = [("alice1", 1), ("alice2", 2)];
        let bob_set = [("bob1", 3), ("bob2", 4), ("bob3", 5)];
        let alice_validate_set = Rc::new(RefCell::new(vec![]));
        let bob_validate_set = Rc::new(RefCell::new(vec![]));

        let validate_alice: ValidateCb<&str, i32> = Box::new({
            let alice_validate_set = alice_validate_set.clone();
            move |_, e| {
                alice_validate_set.borrow_mut().push(*e);
                false
            }
        });
        let validate_bob: ValidateCb<&str, i32> = Box::new({
            let bob_validate_set = bob_validate_set.clone();
            move |_, e| {
                bob_validate_set.borrow_mut().push(*e);
                false
            }
        });

        let mut alice = Peer::default();
        for (k, v) in alice_set {
            alice.put((k, v)).unwrap();
        }

        let mut bob = Peer::default();
        for (k, v) in bob_set {
            bob.put((k, v)).unwrap();
        }

        // run sync with a validate callback returning false, so no new entries are stored on either side
        let res = sync_exchange_messages(alice, bob, &validate_alice, &validate_bob, 100);
        res.assert_alice_set("unchanged", &alice_set);
        res.assert_bob_set("unchanged", &bob_set);

        // assert that the validate callbacks received all expected entries
        assert_eq!(alice_validate_set.take(), bob_set);
        assert_eq!(bob_validate_set.take(), alice_set);
    }

    struct SyncResult<K, V>
    where
        K: RangeKey + PartialEq + Clone + Default + Debug,
        V: Debug + Clone + PartialOrd,
    {
        alice: Peer<(K, V), SimpleStore<K, V>>,
        bob: Peer<(K, V), SimpleStore<K, V>>,
        alice_to_bob: Vec<Message<(K, V)>>,
        bob_to_alice: Vec<Message<(K, V)>>,
    }

    impl<K, V> SyncResult<K, V>
    where
        K: RangeKey + PartialEq + Clone + Default + Debug,
        V: Debug + Clone + PartialOrd + PartialEq,
    {
        fn print_messages(&self) {
            let len = std::cmp::max(self.alice_to_bob.len(), self.bob_to_alice.len());
            for i in 0..len {
                if let Some(msg) = self.alice_to_bob.get(i) {
                    println!("A -> B:");
                    print_message(msg);
                }
                if let Some(msg) = self.bob_to_alice.get(i) {
                    println!("B -> A:");
                    print_message(msg);
                }
            }
        }

        fn assert_alice_set(&self, ctx: &str, expected: &[(K, V)]) {
            dbg!(self.alice.all().unwrap().collect::<Vec<_>>());
            for e in expected {
                assert_eq!(
                    self.alice.store.get(e.key()).unwrap().as_ref(),
                    Some(e),
                    "{}: (alice) missing key {:?}",
                    ctx,
                    e.key()
                );
            }
            assert_eq!(
                expected.len(),
                self.alice.store.len().unwrap(),
                "{}: (alice)",
                ctx
            );
        }

        fn assert_bob_set(&self, ctx: &str, expected: &[(K, V)]) {
            dbg!(self.bob.all().unwrap().collect::<Vec<_>>());

            for e in expected {
                assert_eq!(
                    self.bob.store.get(e.key()).unwrap().as_ref(),
                    Some(e),
                    "{}: (bob) missing key {:?}",
                    ctx,
                    e
                );
            }
            assert_eq!(
                expected.len(),
                self.bob.store.len().unwrap(),
                "{}: (bob)",
                ctx
            );
        }
    }

    fn print_message<E: RangeEntry>(msg: &Message<E>) {
        for part in &msg.parts {
            match part {
                MessagePart::RangeFingerprint(RangeFingerprint { range, fingerprint }) => {
                    println!(
                        "  RangeFingerprint({:?}, {:?}, {:?})",
                        range.x(),
                        range.y(),
                        fingerprint
                    );
                }
                MessagePart::RangeItem(RangeItem {
                    range,
                    values,
                    have_local,
                }) => {
                    println!(
                        "  RangeItem({:?} | {:?}) (local?: {})\n  {:?}",
                        range.x(),
                        range.y(),
                        have_local,
                        values,
                    );
                }
            }
        }
    }

    type ValidateCb<K, V> = Box<dyn Fn(&SimpleStore<K, V>, &(K, V)) -> bool>;

    fn sync<K, V>(alice_set: &[(K, V)], bob_set: &[(K, V)]) -> SyncResult<K, V>
    where
        K: RangeKey + PartialEq + Clone + Default + Debug,
        V: Debug + Clone + PartialOrd,
    {
        let alice_validate_cb: ValidateCb<K, V> = Box::new(|_, _| true);
        let bob_validate_cb: ValidateCb<K, V> = Box::new(|_, _| true);
        sync_with_validate_cb_and_assert(alice_set, bob_set, &alice_validate_cb, &bob_validate_cb)
    }

    fn sync_with_validate_cb_and_assert<K, V, F1, F2>(
        alice_set: &[(K, V)],
        bob_set: &[(K, V)],
        alice_validate_cb: F1,
        bob_validate_cb: F2,
    ) -> SyncResult<K, V>
    where
        K: RangeKey + PartialEq + Clone + Default + Debug,
        V: Debug + Clone + PartialOrd,
        F1: Fn(&SimpleStore<K, V>, &(K, V)) -> bool,
        F2: Fn(&SimpleStore<K, V>, &(K, V)) -> bool,
    {
        let mut expected_set_alice = BTreeMap::new();
        let mut expected_set_bob = BTreeMap::new();

        let mut alice = Peer::<(K, V), SimpleStore<K, V>>::default();
        for e in alice_set {
            alice.put(e.clone()).unwrap();
            expected_set_bob.insert(e.key().clone(), e.1.clone());
            expected_set_alice.insert(e.key().clone(), e.1.clone());
        }

        let mut bob = Peer::<(K, V), SimpleStore<K, V>>::default();
        for e in bob_set {
            bob.put(e.clone()).unwrap();
            expected_set_bob.insert(e.key().clone(), e.1.clone());
            expected_set_alice.insert(e.key().clone(), e.1.clone());
        }

        let res = sync_exchange_messages(alice, bob, alice_validate_cb, bob_validate_cb, 100);

        res.print_messages();

        let alice_now: Vec<_> = res.alice.all().unwrap().collect::<Result<_, _>>().unwrap();
        assert_eq!(
            alice_now.into_iter().collect::<Vec<_>>(),
            expected_set_alice.into_iter().collect::<Vec<_>>(),
            "alice"
        );

        let bob_now: Vec<_> = res.bob.all().unwrap().collect::<Result<_, _>>().unwrap();
        assert_eq!(
            bob_now.into_iter().collect::<Vec<_>>(),
            expected_set_bob.into_iter().collect::<Vec<_>>(),
            "bob"
        );

        // Check that values were never sent twice
        let mut alice_sent = BTreeMap::new();
        for msg in &res.alice_to_bob {
            for part in &msg.parts {
                if let Some(values) = part.values() {
                    for e in values {
                        assert!(
                            alice_sent.insert(e.key(), e).is_none(),
                            "alice: duplicate {:?}",
                            e
                        );
                    }
                }
            }
        }

        let mut bob_sent = BTreeMap::new();
        for msg in &res.bob_to_alice {
            for part in &msg.parts {
                if let Some(values) = part.values() {
                    for e in values {
                        assert!(
                            bob_sent.insert(e.key(), e).is_none(),
                            "bob: duplicate {:?}",
                            e
                        );
                    }
                }
            }
        }

        res
    }

    fn sync_exchange_messages<K, V, F1, F2>(
        mut alice: Peer<(K, V), SimpleStore<K, V>>,
        mut bob: Peer<(K, V), SimpleStore<K, V>>,
        alice_validate_cb: F1,
        bob_validate_cb: F2,
        max_rounds: usize,
    ) -> SyncResult<K, V>
    where
        K: RangeKey + PartialEq + Clone + Default + Debug,
        V: Debug + Clone + PartialOrd,
        F1: Fn(&SimpleStore<K, V>, &(K, V)) -> bool,
        F2: Fn(&SimpleStore<K, V>, &(K, V)) -> bool,
    {
        let mut alice_to_bob = Vec::new();
        let mut bob_to_alice = Vec::new();
        let initial_message = alice.initial_message().unwrap();

        let mut next_to_bob = Some(initial_message);
        let mut rounds = 0;
        while let Some(msg) = next_to_bob.take() {
            assert!(rounds < max_rounds, "too many rounds");
            rounds += 1;
            alice_to_bob.push(msg.clone());

            if let Some(msg) = bob.process_message(msg, &bob_validate_cb).unwrap() {
                bob_to_alice.push(msg.clone());
                next_to_bob = alice.process_message(msg, &alice_validate_cb).unwrap();
            }
        }
        SyncResult {
            alice,
            bob,
            alice_to_bob,
            bob_to_alice,
        }
    }

    #[test]
    fn store_get_range() {
        let mut store = SimpleStore::<&'static str, i32>::default();
        let set = [
            ("bee", 1),
            ("cat", 1),
            ("doe", 1),
            ("eel", 1),
            ("fox", 1),
            ("hog", 1),
        ];
        for (k, v) in &set {
            store.put((*k, *v)).unwrap();
        }

        let all: Vec<_> = store
            .get_range(Range::new("", ""))
            .unwrap()
            .collect::<Result<_, Infallible>>()
            .unwrap();
        assert_eq!(&all, &set[..]);

        let regular: Vec<_> = store
            .get_range(("bee", "eel").into())
            .unwrap()
            .collect::<Result<_, Infallible>>()
            .unwrap();
        assert_eq!(&regular, &set[..3]);

        // empty start
        let regular: Vec<_> = store
            .get_range(("", "eel").into())
            .unwrap()
            .collect::<Result<_, Infallible>>()
            .unwrap();
        assert_eq!(&regular, &set[..3]);

        let regular: Vec<_> = store
            .get_range(("cat", "hog").into())
            .unwrap()
            .collect::<Result<_, Infallible>>()
            .unwrap();

        assert_eq!(&regular, &set[1..5]);

        let excluded: Vec<_> = store
            .get_range(("fox", "bee").into())
            .unwrap()
            .collect::<Result<_, Infallible>>()
            .unwrap();

        assert_eq!(excluded[0].0, "fox");
        assert_eq!(excluded[1].0, "hog");
        assert_eq!(excluded.len(), 2);

        let excluded: Vec<_> = store
            .get_range(("fox", "doe").into())
            .unwrap()
            .collect::<Result<_, Infallible>>()
            .unwrap();

        assert_eq!(excluded.len(), 4);
        assert_eq!(excluded[0].0, "bee");
        assert_eq!(excluded[1].0, "cat");
        assert_eq!(excluded[2].0, "fox");
        assert_eq!(excluded[3].0, "hog");
    }

    type TestSet = BTreeMap<String, ()>;

    fn test_key() -> impl Strategy<Value = String> {
        "[a-z0-9]{0,5}"
    }

    fn test_set() -> impl Strategy<Value = TestSet> {
        prop::collection::btree_map(test_key(), Just(()), 0..10)
    }

    fn test_vec() -> impl Strategy<Value = Vec<(String, ())>> {
        test_set().prop_map(|m| m.into_iter().collect::<Vec<_>>())
    }

    fn test_range() -> impl Strategy<Value = Range<String>> {
        // ranges with x > y are explicitly allowed - they wrap around
        (test_key(), test_key()).prop_map(|(x, y)| Range::new(x, y))
    }

    fn mk_test_set(values: impl IntoIterator<Item = impl AsRef<str>>) -> TestSet {
        values
            .into_iter()
            .map(|v| v.as_ref().to_string())
            .map(|k| (k, ()))
            .collect()
    }

    fn mk_test_vec(values: impl IntoIterator<Item = impl AsRef<str>>) -> Vec<(String, ())> {
        mk_test_set(values).into_iter().collect()
    }

    #[test]
    fn simple_store_sync_1() {
        let alice = mk_test_vec(["3"]);
        let bob = mk_test_vec(["2", "3", "4", "5", "6", "7", "8"]);
        let _res = sync(&alice, &bob);
    }

    #[test]
    fn simple_store_sync_2() {
        let alice = mk_test_vec(["1", "3"]);
        let bob = mk_test_vec(["0", "2", "3"]);
        let _res = sync(&alice, &bob);
    }

    #[test]
    fn simple_store_sync_3() {
        let alice = mk_test_vec(["8", "9"]);
        let bob = mk_test_vec(["1", "2", "3"]);
        let _res = sync(&alice, &bob);
    }

    #[proptest]
    fn simple_store_sync(
        #[strategy(test_vec())] alice: Vec<(String, ())>,
        #[strategy(test_vec())] bob: Vec<(String, ())>,
    ) {
        let _res = sync(&alice, &bob);
    }

    /// A generic fn to make a test for the get_range fn of a store.
    #[allow(clippy::type_complexity)]
    fn store_get_ranges_test<S, E>(
        elems: impl IntoIterator<Item = E>,
        range: Range<E::Key>,
    ) -> (Vec<E>, Vec<E>)
    where
        S: Store<E> + Default,
        E: RangeEntry,
    {
        let mut store = S::default();
        let elems = elems.into_iter().collect::<Vec<_>>();
        for e in elems.iter().cloned() {
            store.put(e).unwrap();
        }
        let mut actual = store
            .get_range(range.clone())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, S::Error>>()
            .unwrap();
        let mut expected = elems
            .into_iter()
            .filter(|e| range.contains(e.key()))
            .collect::<Vec<_>>();

        actual.sort_by(|a, b| a.key().cmp(b.key()));
        expected.sort_by(|a, b| a.key().cmp(b.key()));
        (expected, actual)
    }

    #[proptest]
    fn simple_store_get_ranges(
        #[strategy(test_set())] contents: BTreeMap<String, ()>,
        #[strategy(test_range())] range: Range<String>,
    ) {
        let (expected, actual) = store_get_ranges_test::<SimpleStore<_, _>, _>(contents, range);
        prop_assert_eq!(expected, actual);
    }
}
//! The thread-safe (`Send + Sync`) immutable radix face.
//!
//! [`Radix`] is an **immutable, persistent** trie that fixes the internal pointer
//! kind to [`Arc`](std::sync::Arc) — or, with the `triomphe` feature, the more compact
//! `triomphe::Arc` (no weak count). A value is a cheap, shareable snapshot that is
//! `Send + Sync` exactly when both `C` and `V` are (both are *auto-derived* — the crate
//! declares no explicit `unsafe impl Send`/`Sync` anywhere). Every mutation returns
//! a *new* tree; the original is never observed to change.
//!
//! Mutation goes through a [`Txn`] — open one with [`txn`](Radix::txn), edit the
//! owned working copy freely, then [`commit`](Txn::commit) it into the next
//! immutable tree. A one-shot edit is just `let mut t = r.txn(); t.insert(...); let
//! r = t.commit();`. This mirrors go-immutable-radix's `Tree` / `Txn` split.
//!
//! For a single-threaded trie that never shares a snapshot across threads, prefer
//! the cheaper non-atomic [`crate::unsync`] face.
//!
//! # Lock-free sharing
//!
//! `iradix` ships no built-in concurrent holder. Because a [`Radix`] is an
//! `O(1)`-clone immutable snapshot, you publish new versions yourself — typically
//! through an `arc_swap::ArcSwap<`[`Radix`]`<…>>`: readers `load` a wait-free
//! snapshot, and a writer opens a [`Txn`], commits it into the next `Radix`, and
//! publishes it with a compare-and-swap retry loop. See `examples/sync.rs`.
//!
//! The discipline is **commit → publish → notify**, in that order:
//! [`commit`](Txn::commit) builds the next tree but fires nothing, you make it the
//! live version (a winning compare-and-swap), and only the winner then notifies —
//! via [`notify_changes_since`](Radix::notify_changes_since), or the two folded into
//! one by [`publish_to`](Radix::publish_to). This is what makes the `watch` feature
//! sound under lock-free CAS publishing: a tree that loses the race is discarded
//! without ever notifying, so a lost CAS cannot strand or falsely wake a watcher.
//! With the `watch` feature, see [`Watch`].

use std::vec::Vec;

use core::ops::{Bound, RangeBounds};

// The sync face fixes the trie's pointer kind: `std::sync::Arc` via `ArcK` by default,
// or — with the `triomphe` feature — the more compact `triomphe::Arc` (no weak count)
// via archery's `ArcTK`. The public API is identical either way.
#[cfg(not(feature = "triomphe"))]
pub(crate) use archery::ArcK as ArcP;
#[cfg(feature = "triomphe")]
pub(crate) use archery::ArcTK as ArcP;

#[cfg(all(feature = "watch", not(feature = "triomphe")))]
use std::sync::Arc;

#[cfg(all(feature = "watch", feature = "triomphe"))]
use triomphe::Arc;

#[cfg(feature = "watch")]
use event_listener::EventListener;

// `Listener::{wait, wait_timeout}` (the blocking waits) are the only trait methods
// used; they need std.
#[cfg(all(feature = "watch", feature = "std"))]
use event_listener::Listener;

use crate::{
  RadixKey,
  node::{RangeIter, RevRangeIter, RevValueIter, Root, SliceIter, ValueIter},
};

#[cfg(feature = "watch")]
use crate::node::WatchSlot;

#[cfg(feature = "watch")]
#[cfg_attr(docsrs, doc(cfg(feature = "watch")))]
pub use watch::*;

#[cfg(feature = "watch")]
#[cfg_attr(docsrs, doc(cfg(feature = "watch")))]
mod watch;

/// A generic, persistent (copy-on-write) **immutable** radix trie, shareable across
/// threads.
///
/// Keys are walked into components via [`RadixKey`]; the trie is parameterized over the
/// stored component type `C` (a key's [`RadixKey::Owned`]) and the value type `V`, and
/// uses [`Arc`](std::sync::Arc) pointers internally — or `triomphe::Arc` with the
/// `triomphe` feature — so it is `Send + Sync` whenever both `C` and `V` are
/// (auto-derived — no `unsafe impl`).
///
/// A `Radix` never mutates in place. A [`clone`](Clone::clone) is O(1) and shares
/// all structure with the original; a committed edit produces a *new* tree that
/// copies only the path from the root to the touched node, leaving every untouched
/// subtree physically shared with the prior version. This makes snapshots free and
/// isolation automatic.
///
/// Mutation is via a [`Txn`]: open one with [`txn`](Radix::txn), edit the owned
/// working copy, then [`commit`](Txn::commit) into the next tree (see the example
/// below). To publish versions to shared readers, hold the snapshot in an `ArcSwap`
/// yourself — see the [module docs](crate::sync#lock-free-sharing). The publishing
/// discipline is **commit → publish → notify**: `commit` builds the tree but fires
/// no `watch` events; after a *winning* publish (e.g. a successful compare-and-swap)
/// the new version notifies via
/// [`notify_changes_since`](Radix::notify_changes_since) (or both at once via
/// [`publish_to`](Radix::publish_to)), so a tree that lost the race notifies nothing.
///
/// # Examples
///
/// ```
/// use iradix::sync::Radix;
///
/// // A one-shot edit: open a txn, edit, commit. The original is untouched.
/// let base: Radix<u8, u32> = Radix::new();
/// let mut t = base.txn();
/// assert_eq!(t.insert(b"abc".as_slice(), 1), None);
/// let t1 = t.commit();
/// assert_eq!(base.get(b"abc".as_slice()), None); // original unchanged
/// assert_eq!(t1.get(b"abc".as_slice()), Some(&1));
///
/// // Batch several edits in a transaction, then commit.
/// let mut txn = t1.txn();
/// txn.insert(b"abd".as_slice(), 2);
/// txn.insert(b"b".as_slice(), 3);
/// let t2 = txn.commit();
/// assert_eq!(t2.len(), 3);
/// assert_eq!(t1.len(), 1); // the pre-txn tree is still frozen
/// ```
///
/// # Bounds
///
/// The struct itself puts no bound on `C`. Reads add `C: Ord`; the ordered reads
/// that rebuild a key (`minimum` / `maximum` / `range` / `seek_lower_bound`) also
/// add `C: Clone`. Mutation lives on [`Txn`], whose mutators add
/// `C: Ord + Clone, V: Clone`. Reads are `V`-bound-free and return `&V`.
pub struct Radix<C, V> {
  inner: Root<ArcP, C, V>,
  /// Change slot for the empty (`None`-root) position — there is no node to listen
  /// on while the trie is empty. It is *per empty-epoch*: a run of consecutive empty
  /// versions shares one slot (so a watch armed on any of them sees the first insert),
  /// and a fresh slot begins each time the trie becomes empty again. A later version's
  /// [`notify_changes_since`](Radix::notify_changes_since) fires the *base* version's
  /// slot when the trie went from empty to non-empty.
  #[cfg(feature = "watch")]
  empty: Arc<WatchSlot>,
}

// O(1) clone — no `V: Clone` (the pointer is bumped, values are not touched).
impl<C, V> Clone for Radix<C, V> {
  #[inline]
  fn clone(&self) -> Self {
    Self {
      inner: self.inner.clone(),
      #[cfg(feature = "watch")]
      empty: self.empty.clone(),
    }
  }
}

impl<C, V> Default for Radix<C, V> {
  #[inline]
  fn default() -> Self {
    Self::new()
  }
}

impl<C, V> Radix<C, V> {
  /// Creates an empty trie. `const`: no allocation happens until the first
  /// insert.
  #[cfg(not(feature = "watch"))]
  #[inline]
  pub const fn new() -> Self {
    Self { inner: Root::new() }
  }

  /// Creates an empty trie. With the `watch` feature this allocates the shared
  /// empty-position change channel, so — unlike the default build — it is not
  /// `const`.
  #[cfg(feature = "watch")]
  #[inline]
  pub fn new() -> Self {
    Self {
      inner: Root::new(),
      empty: Arc::new(WatchSlot::new()),
    }
  }

  /// Returns the number of values stored in the trie.
  #[inline]
  pub const fn len(&self) -> usize {
    self.inner.len()
  }

  /// Returns `true` if the trie holds no values.
  #[inline]
  pub const fn is_empty(&self) -> bool {
    self.inner.is_empty()
  }

  /// Opens a [`Txn`] — an owned working copy of this tree. Edit it freely with its
  /// `&mut self` mutators, then [`commit`](Txn::commit) it into the next immutable
  /// `Radix`. Opening a transaction is an `O(1)` structural-sharing clone; this
  /// tree is unaffected by anything the transaction does (and dropping the
  /// transaction without committing discards its edits).
  #[inline]
  #[must_use = "a Txn is an owned working copy with no effect until commit"]
  pub fn txn(&self) -> Txn<C, V> {
    Txn {
      working: self.inner.clone(),
      #[cfg(feature = "watch")]
      base: self.inner.clone(),
      #[cfg(feature = "watch")]
      empty: self.empty.clone(),
    }
  }
}

impl<C, V> Radix<C, V>
where
  C: Ord,
{
  /// Returns a reference to the value stored at exactly `key`, if any.
  ///
  /// The key is walked lazily over its components; the walk is **allocation-free** for
  /// every built-in key, `Path` included — only `insert` allocates the stored form (see
  /// the [`RadixKey`] Allocation section).
  #[inline]
  pub fn get<K>(&self, key: &K) -> Option<&V>
  where
    K: RadixKey<Owned = C> + ?Sized,
  {
    self.inner.get::<K, _>(key.components())
  }

  /// Returns `true` if a value is stored at exactly `key`.
  #[inline]
  pub fn contains<K>(&self, key: &K) -> bool
  where
    K: RadixKey<Owned = C> + ?Sized,
  {
    self.get(key).is_some()
  }

  /// Returns the value of the deepest stored key that is a prefix of `key`,
  /// **inclusive** of an exact match (longest-prefix match).
  #[inline]
  pub fn get_ancestor<K>(&self, key: &K) -> Option<&V>
  where
    K: RadixKey<Owned = C> + ?Sized,
  {
    self.inner.ancestor::<K, _>(key.components(), true)
  }

  /// Returns the value of the deepest stored key that is a *strict* prefix of
  /// `key` (excludes an exact match).
  #[inline]
  pub fn strict_ancestor<K>(&self, key: &K) -> Option<&V>
  where
    K: RadixKey<Owned = C> + ?Sized,
  {
    self.inner.ancestor::<K, _>(key.components(), false)
  }

  /// Returns `true` if any stored key is a prefix of `key` (inclusive).
  #[inline]
  pub fn has_ancestor<K>(&self, key: &K) -> bool
  where
    K: RadixKey<Owned = C> + ?Sized,
  {
    self.get_ancestor(key).is_some()
  }

  /// Iterates references to every value in the trie, in key order.
  #[inline]
  pub fn values(&self) -> Values<'_, C, V> {
    Values {
      inner: self.inner.values(),
    }
  }

  /// Iterates references to the values of every stored key that is a prefix of
  /// `key`, **inclusive** of an exact match (root-to-`key` path).
  #[inline]
  pub fn ancestors<K>(&self, key: &K) -> Ancestors<'_, V>
  where
    K: RadixKey<Owned = C> + ?Sized,
  {
    Ancestors {
      inner: self.inner.ancestors::<K, _>(key.components()),
    }
  }

  /// Iterates references to the values of every *strict* descendant of `key`
  /// (stored keys that strictly extend `key`; the value at `key` is excluded).
  #[inline]
  pub fn descendants<K>(&self, key: &K) -> Descendants<'_, C, V>
  where
    K: RadixKey<Owned = C> + ?Sized,
  {
    Descendants {
      inner: self.inner.descendants::<K, _>(key.components()),
    }
  }

  /// Returns the smallest key (component lexicographic order) and its value, or
  /// `None` if the trie is empty.
  #[inline]
  pub fn minimum(&self) -> Option<(Vec<C>, &V)>
  where
    C: Clone,
  {
    self.inner.minimum()
  }

  /// Returns the largest key (component lexicographic order) and its value, or
  /// `None` if the trie is empty.
  #[inline]
  pub fn maximum(&self) -> Option<(Vec<C>, &V)>
  where
    C: Clone,
  {
    self.inner.maximum()
  }

  /// Iterates references to every value in the trie, in **reverse** key order
  /// (the mirror of [`values`](Radix::values)).
  #[inline]
  #[must_use]
  pub fn values_rev(&self) -> RevValues<'_, C, V> {
    RevValues {
      inner: self.inner.values_rev(),
    }
  }

  /// Iterates references to the values of every *strict* descendant of `key`, in
  /// **reverse** key order (the mirror of [`descendants`](Radix::descendants)).
  #[inline]
  #[must_use]
  pub fn descendants_rev<K>(&self, key: &K) -> RevDescendants<'_, C, V>
  where
    K: RadixKey<Owned = C> + ?Sized,
  {
    RevDescendants {
      inner: self.inner.descendants_rev::<K, _>(key.components()),
    }
  }

  /// Iterates `(key, value)` for every entry whose key lies within `range`, in
  /// ascending key order, reconstructing each key as a `Vec` of its components.
  ///
  /// Every [`Bound`] combination is honored on both ends:
  /// `Included`/`Excluded`/`Unbounded`.
  #[inline]
  #[must_use]
  pub fn range<K, R>(&self, range: R) -> Range<'_, C, V>
  where
    K: RadixKey<Owned = C> + ?Sized,
    R: RangeBounds<K>,
    C: Clone,
  {
    Range {
      inner: self.inner.range(
        materialize_bound(range.start_bound()),
        materialize_bound(range.end_bound()),
      ),
    }
  }

  /// Returns a forward cursor positioned at the first entry whose key is `>= key`,
  /// then ascending (go-immutable-radix's `SeekLowerBound`). Equivalent to
  /// [`range(key..)`](Radix::range), exposed under its own name for parity.
  #[inline]
  #[must_use]
  pub fn seek_lower_bound<K>(&self, key: &K) -> Range<'_, C, V>
  where
    K: RadixKey<Owned = C> + ?Sized,
    C: Clone,
  {
    let lower: Vec<C> = key.components().map(K::to_owned).collect();
    Range {
      inner: self.inner.range(Bound::Included(lower), Bound::Unbounded),
    }
  }

  /// Iterates references to the values of every descendant of `key`, **including
  /// the value at `key` itself** — the node-inclusive form of
  /// [`descendants`](Radix::descendants) (go-immutable-radix's `WalkPrefix`).
  #[inline]
  #[must_use]
  pub fn descendants_inclusive<K>(&self, key: &K) -> Descendants<'_, C, V>
  where
    K: RadixKey<Owned = C> + ?Sized,
  {
    Descendants {
      inner: self.inner.descendants_inclusive::<K, _>(key.components()),
    }
  }

  /// Iterates `(key, value)` for every entry whose key lies within `range`, in
  /// **descending** key order — the reverse of [`range`](Radix::range).
  #[inline]
  #[must_use]
  pub fn range_rev<K, R>(&self, range: R) -> RevRange<'_, C, V>
  where
    K: RadixKey<Owned = C> + ?Sized,
    R: RangeBounds<K>,
    C: Clone,
  {
    RevRange {
      inner: self.inner.rev_range(
        materialize_bound(range.start_bound()),
        materialize_bound(range.end_bound()),
      ),
    }
  }

  /// Returns a **reverse** cursor positioned at the last entry whose key is
  /// `<= key`, then descending (go-immutable-radix's `SeekReverseLowerBound`).
  /// Equivalent to [`range_rev(..=key)`](Radix::range_rev), exposed under its own
  /// name for parity with [`seek_lower_bound`](Radix::seek_lower_bound).
  #[inline]
  #[must_use]
  pub fn seek_reverse_lower_bound<K>(&self, key: &K) -> RevRange<'_, C, V>
  where
    K: RadixKey<Owned = C> + ?Sized,
    C: Clone,
  {
    let upper: Vec<C> = key.components().map(K::to_owned).collect();
    RevRange {
      inner: self
        .inner
        .rev_range(Bound::Unbounded, Bound::Included(upper)),
    }
  }

  /// Iterates `(key, value)` for every entry having `prefix` as a prefix, **including
  /// the value at `prefix` itself**, ascending — go-immutable-radix's `WalkPrefix`;
  /// the key-carrying form of [`descendants_inclusive`](Radix::descendants_inclusive).
  #[inline]
  #[must_use]
  pub fn walk_prefix<K>(&self, prefix: &K) -> Range<'_, C, V>
  where
    K: RadixKey<Owned = C> + ?Sized,
    C: Clone,
  {
    Range {
      inner: self.inner.prefix_entries::<K, _>(prefix.components(), true),
    }
  }

  /// The descending form of [`walk_prefix`](Radix::walk_prefix).
  #[inline]
  #[must_use]
  pub fn walk_prefix_rev<K>(&self, prefix: &K) -> RevRange<'_, C, V>
  where
    K: RadixKey<Owned = C> + ?Sized,
    C: Clone,
  {
    RevRange {
      inner: self
        .inner
        .prefix_entries_rev::<K, _>(prefix.components(), true),
    }
  }

  /// Iterates `(key, value)` for every *strict* descendant of `prefix`, ascending —
  /// the key-carrying form of [`descendants`](Radix::descendants).
  #[inline]
  #[must_use]
  pub fn walk_prefix_strict<K>(&self, prefix: &K) -> Range<'_, C, V>
  where
    K: RadixKey<Owned = C> + ?Sized,
    C: Clone,
  {
    Range {
      inner: self
        .inner
        .prefix_entries::<K, _>(prefix.components(), false),
    }
  }

  /// The descending form of [`walk_prefix_strict`](Radix::walk_prefix_strict).
  #[inline]
  #[must_use]
  pub fn walk_prefix_strict_rev<K>(&self, prefix: &K) -> RevRange<'_, C, V>
  where
    K: RadixKey<Owned = C> + ?Sized,
    C: Clone,
  {
    RevRange {
      inner: self
        .inner
        .prefix_entries_rev::<K, _>(prefix.components(), false),
    }
  }

  /// Iterates `(key, value)` for every stored key that is a prefix of `key`
  /// (inclusive), in root-to-`key` order — go-immutable-radix's `WalkPath`; the
  /// key-carrying form of [`ancestors`](Radix::ancestors).
  #[inline]
  #[must_use]
  pub fn walk_path<K>(&self, key: &K) -> PathEntries<'_, C, V>
  where
    K: RadixKey<Owned = C> + ?Sized,
    C: Clone,
  {
    PathEntries::new(self.inner.ancestor_entries::<K, _>(key.components()))
  }

  /// The reverse (`key`-to-root) form of [`walk_path`](Radix::walk_path).
  #[inline]
  #[must_use]
  pub fn walk_path_rev<K>(&self, key: &K) -> PathEntries<'_, C, V>
  where
    K: RadixKey<Owned = C> + ?Sized,
    C: Clone,
  {
    let mut entries = self.inner.ancestor_entries::<K, _>(key.components());
    entries.reverse();
    PathEntries::new(entries)
  }
}

// Builds a tree from `(key, value)` pairs whose key is a `Vec` of components,
// inserting through one transaction. A clean generic `FromIterator` over arbitrary
// `RadixKey` is not expressible (the item would have to name an associated type),
// so the owned `Vec<C>` key form is provided; for a borrowed-key bulk build, open a
// `txn` and `insert` each key.
impl<C, V> FromIterator<(Vec<C>, V)> for Radix<C, V>
where
  C: Ord + Clone,
  V: Clone,
{
  fn from_iter<T: IntoIterator<Item = (Vec<C>, V)>>(iter: T) -> Self {
    let mut t = Self::new().txn();
    for (key, value) in iter {
      t.insert(key.as_slice(), value);
    }
    t.commit()
  }
}

/// An owned, mutable working copy of a [`Radix`] — go-immutable-radix's `Txn`.
///
/// A transaction holds an `O(1)` structural-sharing clone of the tree it was opened
/// from (via [`Radix::txn`]). Mutate it freely with its `&mut self` methods — they
/// edit the working copy in place (copy-on-write), so reads are read-your-writes —
/// then [`commit`](Txn::commit) it to obtain the next immutable [`Radix`]. The tree
/// the transaction was opened from is never affected; dropping a transaction without
/// committing simply discards its edits.
///
/// A transaction owns its data: it borrows nothing and has no lifetime. Because it
/// is `Arc`-backed, `Txn<C, V>` is `Send` / `Sync` — movable and shareable across
/// threads — exactly when both `C` and `V` are `Send + Sync`.
///
/// # Examples
///
/// ```
/// use iradix::sync::Radix;
///
/// let base: Radix<u8, u32> = Radix::new();
/// let mut txn = base.txn();
/// txn.insert(b"a".as_slice(), 1);
/// txn.insert(b"ab".as_slice(), 2);
/// assert_eq!(txn.get(b"a".as_slice()), Some(&1)); // read-your-writes
/// let tree = txn.commit();
/// assert_eq!(tree.len(), 2);
/// assert_eq!(base.len(), 0); // the source tree never changed
/// ```
pub struct Txn<C, V> {
  working: Root<ArcP, C, V>,
  /// The version this transaction was opened from, kept so `commit` can decide the
  /// committed tree's empty-epoch slot (carry the base's, or open a fresh one). The
  /// replaced-node diff itself runs later, in [`Radix::notify_changes_since`].
  #[cfg(feature = "watch")]
  base: Root<ArcP, C, V>,
  /// The base version's empty-position slot. `commit` carries it into the committed
  /// tree while the empty-epoch is unbroken (see [`Radix::notify_changes_since`]).
  #[cfg(feature = "watch")]
  empty: Arc<WatchSlot>,
}

impl<C, V> Txn<C, V> {
  /// Returns the number of values currently in the working copy.
  #[inline]
  pub const fn len(&self) -> usize {
    self.working.len()
  }

  /// Returns `true` if the working copy holds no values.
  #[inline]
  pub const fn is_empty(&self) -> bool {
    self.working.is_empty()
  }

  /// Removes every value from the working copy, resetting it to empty.
  #[inline]
  pub fn clear(&mut self) {
    self.working.clear();
  }

  /// Consumes the transaction and returns the next immutable [`Radix`] holding all
  /// committed edits.
  ///
  /// Committing builds the next tree only; it fires **no** `watch` events. With the
  /// `watch` feature, publishing follows **commit → publish → notify**: after the
  /// returned tree wins publication (e.g. a successful compare-and-swap), call
  /// [`Radix::notify_changes_since`] against the version this transaction was opened
  /// from — or fold the publish and notify together with [`Radix::publish_to`]. A
  /// committed-then-discarded tree (a lost CAS) must NOT notify.
  #[cfg(not(feature = "watch"))]
  #[inline]
  #[must_use = "returns the new tree holding the committed edits"]
  pub fn commit(self) -> Radix<C, V> {
    Radix {
      inner: self.working,
    }
  }
}

impl<C, V> Txn<C, V>
where
  C: Ord,
{
  /// Returns a reference to the value stored at exactly `key` in the working copy,
  /// if any (read-your-writes).
  #[inline]
  pub fn get<K>(&self, key: &K) -> Option<&V>
  where
    K: RadixKey<Owned = C> + ?Sized,
  {
    self.working.get::<K, _>(key.components())
  }

  /// Returns `true` if a value is stored at exactly `key` in the working copy.
  #[inline]
  pub fn contains<K>(&self, key: &K) -> bool
  where
    K: RadixKey<Owned = C> + ?Sized,
  {
    self.get(key).is_some()
  }

  /// Returns the value of the deepest stored key that is a prefix of `key`,
  /// **inclusive** of an exact match (longest-prefix match).
  #[inline]
  pub fn get_ancestor<K>(&self, key: &K) -> Option<&V>
  where
    K: RadixKey<Owned = C> + ?Sized,
  {
    self.working.ancestor::<K, _>(key.components(), true)
  }

  /// Returns the value of the deepest stored key that is a *strict* prefix of
  /// `key` (excludes an exact match).
  #[inline]
  pub fn strict_ancestor<K>(&self, key: &K) -> Option<&V>
  where
    K: RadixKey<Owned = C> + ?Sized,
  {
    self.working.ancestor::<K, _>(key.components(), false)
  }

  /// Returns `true` if any stored key is a prefix of `key` (inclusive).
  #[inline]
  pub fn has_ancestor<K>(&self, key: &K) -> bool
  where
    K: RadixKey<Owned = C> + ?Sized,
  {
    self.get_ancestor(key).is_some()
  }

  /// Iterates references to every value in the working copy, in key order.
  #[inline]
  pub fn values(&self) -> Values<'_, C, V> {
    Values {
      inner: self.working.values(),
    }
  }

  /// Iterates references to the values of every stored key that is a prefix of
  /// `key`, **inclusive** of an exact match (root-to-`key` path).
  #[inline]
  pub fn ancestors<K>(&self, key: &K) -> Ancestors<'_, V>
  where
    K: RadixKey<Owned = C> + ?Sized,
  {
    Ancestors {
      inner: self.working.ancestors::<K, _>(key.components()),
    }
  }

  /// Iterates references to the values of every *strict* descendant of `key`
  /// (stored keys that strictly extend `key`; the value at `key` is excluded).
  #[inline]
  pub fn descendants<K>(&self, key: &K) -> Descendants<'_, C, V>
  where
    K: RadixKey<Owned = C> + ?Sized,
  {
    Descendants {
      inner: self.working.descendants::<K, _>(key.components()),
    }
  }

  /// Iterates references to the values of every descendant of `key`, **including
  /// the value at `key` itself** — the node-inclusive form of
  /// [`descendants`](Txn::descendants).
  #[inline]
  pub fn descendants_inclusive<K>(&self, key: &K) -> Descendants<'_, C, V>
  where
    K: RadixKey<Owned = C> + ?Sized,
  {
    Descendants {
      inner: self.working.descendants_inclusive::<K, _>(key.components()),
    }
  }

  /// Iterates `(key, value)` for every entry having `prefix` as a prefix,
  /// **inclusive** of the value at `prefix` (go-immutable-radix's `WalkPrefix`).
  #[inline]
  #[must_use]
  pub fn walk_prefix<K>(&self, prefix: &K) -> Range<'_, C, V>
  where
    K: RadixKey<Owned = C> + ?Sized,
    C: Clone,
  {
    Range {
      inner: self
        .working
        .prefix_entries::<K, _>(prefix.components(), true),
    }
  }

  /// Iterates `(key, value)` for every *strict* descendant of `prefix`, ascending.
  #[inline]
  #[must_use]
  pub fn walk_prefix_strict<K>(&self, prefix: &K) -> Range<'_, C, V>
  where
    K: RadixKey<Owned = C> + ?Sized,
    C: Clone,
  {
    Range {
      inner: self
        .working
        .prefix_entries::<K, _>(prefix.components(), false),
    }
  }

  /// Iterates `(key, value)` for every stored key that is a prefix of `key`
  /// (inclusive), in root-to-`key` order (go-immutable-radix's `WalkPath`).
  #[inline]
  #[must_use]
  pub fn walk_path<K>(&self, key: &K) -> PathEntries<'_, C, V>
  where
    K: RadixKey<Owned = C> + ?Sized,
    C: Clone,
  {
    PathEntries::new(self.working.ancestor_entries::<K, _>(key.components()))
  }
}

impl<C, V> Txn<C, V>
where
  C: Ord + Clone,
  V: Clone,
{
  /// Inserts `value` at `key` in the working copy, returning the previous value if
  /// the key was set.
  pub fn insert<K>(&mut self, key: &K, value: V) -> Option<V>
  where
    K: RadixKey<Owned = C> + ?Sized,
  {
    self.working.insert::<K, _>(key.components(), value)
  }

  /// Removes and returns the value at exactly `key` in the working copy, if any.
  pub fn remove<K>(&mut self, key: &K) -> Option<V>
  where
    K: RadixKey<Owned = C> + ?Sized,
  {
    self.working.remove::<K, _>(key.components())
  }

  /// Removes every *strict* descendant of `key` (the value at `key`, if any, is
  /// kept). Returns the number of values removed. Clones no *removed* value — only
  /// the copy-on-write path to `key` may be duplicated, as in every mutator.
  pub fn remove_descendants<K>(&mut self, key: &K) -> usize
  where
    K: RadixKey<Owned = C> + ?Sized,
  {
    self
      .working
      .remove_descendants::<K, _, _>(|| key.components())
  }

  /// Removes every *strict* descendant of `key` and returns their values (the
  /// value at `key`, if any, is kept). Clones values out before unlinking.
  pub fn drain_descendants<K>(&mut self, key: &K) -> Vec<V>
  where
    K: RadixKey<Owned = C> + ?Sized,
  {
    self
      .working
      .drain_descendants::<K, _, _>(|| key.components())
  }

  /// Removes the value at `key` **and** every strict descendant (node-inclusive;
  /// go-immutable-radix's `DeletePrefix`), returning the number of values removed.
  ///
  /// Contrast [`remove_descendants`](Txn::remove_descendants), which keeps the
  /// value stored at `key` itself. Clones no *removed* value — only retained
  /// ancestors on the copied path may be cloned by copy-on-write, as in every
  /// mutator.
  pub fn delete_prefix<K>(&mut self, key: &K) -> usize
  where
    K: RadixKey<Owned = C> + ?Sized,
  {
    self.working.delete_prefix::<K, _, _>(|| key.components())
  }

  /// Removes the value at `key` **and** every strict descendant (node-inclusive)
  /// and returns their values in ascending key order (the value at `key` itself,
  /// if any, first). Clones values out before unlinking.
  pub fn drain_prefix<K>(&mut self, key: &K) -> Vec<V>
  where
    K: RadixKey<Owned = C> + ?Sized,
  {
    self.working.drain_prefix::<K, _, _>(|| key.components())
  }
}

/// Iterator over references to every value in a [`Radix`], in key order.
///
/// Created by [`Radix::values`].
pub struct Values<'a, C, V> {
  inner: ValueIter<'a, ArcP, C, V>,
}

impl<'a, C, V> Iterator for Values<'a, C, V> {
  type Item = &'a V;

  #[inline]
  fn next(&mut self) -> Option<Self::Item> {
    self.inner.next()
  }
}

/// Iterator over references to the values of `key`'s ancestors (inclusive).
///
/// Created by [`Radix::ancestors`].
pub struct Ancestors<'a, V> {
  inner: SliceIter<'a, V>,
}

impl<'a, V> Iterator for Ancestors<'a, V> {
  type Item = &'a V;

  #[inline]
  fn next(&mut self) -> Option<Self::Item> {
    self.inner.next()
  }
}

/// Iterator over references to the values of `key`'s descendants — strict, or
/// node-inclusive.
///
/// Created by [`Radix::descendants`] (strict) and
/// [`Radix::descendants_inclusive`] (node-inclusive).
pub struct Descendants<'a, C, V> {
  inner: ValueIter<'a, ArcP, C, V>,
}

impl<'a, C, V> Iterator for Descendants<'a, C, V> {
  type Item = &'a V;

  #[inline]
  fn next(&mut self) -> Option<Self::Item> {
    self.inner.next()
  }
}

/// Iterator over references to every value in a [`Radix`], in reverse key order.
///
/// Created by [`Radix::values_rev`].
pub struct RevValues<'a, C, V> {
  inner: RevValueIter<'a, ArcP, C, V>,
}

impl<'a, C, V> Iterator for RevValues<'a, C, V> {
  type Item = &'a V;

  #[inline]
  fn next(&mut self) -> Option<Self::Item> {
    self.inner.next()
  }
}

/// Iterator over references to `key`'s strict descendants, in reverse key order.
///
/// Created by [`Radix::descendants_rev`].
pub struct RevDescendants<'a, C, V> {
  inner: RevValueIter<'a, ArcP, C, V>,
}

impl<'a, C, V> Iterator for RevDescendants<'a, C, V> {
  type Item = &'a V;

  #[inline]
  fn next(&mut self) -> Option<Self::Item> {
    self.inner.next()
  }
}

/// Iterator over `(key, value)` entries within a range, in ascending key order.
///
/// Each item's key is reconstructed as a `Vec` of its components. Created by
/// [`Radix::range`] and [`Radix::seek_lower_bound`].
pub struct Range<'a, C, V> {
  inner: RangeIter<'a, ArcP, C, V>,
}

impl<'a, C, V> Iterator for Range<'a, C, V>
where
  C: Ord + Clone,
{
  type Item = (Vec<C>, &'a V);

  #[inline]
  fn next(&mut self) -> Option<Self::Item> {
    self.inner.next()
  }
}

/// Iterator over `(key, value)` entries within a range, in **descending** key order
/// (the reverse of [`Range`]).
///
/// Created by [`Radix::range_rev`] and [`Radix::seek_reverse_lower_bound`].
pub struct RevRange<'a, C, V> {
  inner: RevRangeIter<'a, ArcP, C, V>,
}

impl<'a, C, V> Iterator for RevRange<'a, C, V>
where
  C: Ord + Clone,
{
  type Item = (Vec<C>, &'a V);

  #[inline]
  fn next(&mut self) -> Option<Self::Item> {
    self.inner.next()
  }
}

/// The owned `(key, value)` entry a [`PathEntries`] iterator yields.
type PathEntry<'a, C, V> = (Vec<C>, &'a V);

/// Iterator over `(key, value)` entries along a root-to-`key` path — the stored keys
/// that are prefixes of the query key.
///
/// Created by [`Radix::walk_path`] (ascending) and [`Radix::walk_path_rev`]
/// (descending).
pub struct PathEntries<'a, C, V> {
  inner: std::vec::IntoIter<PathEntry<'a, C, V>>,
}

impl<'a, C, V> PathEntries<'a, C, V> {
  #[inline]
  fn new(entries: Vec<PathEntry<'a, C, V>>) -> Self {
    Self {
      inner: entries.into_iter(),
    }
  }
}

impl<'a, C, V> Iterator for PathEntries<'a, C, V> {
  type Item = PathEntry<'a, C, V>;

  #[inline]
  fn next(&mut self) -> Option<Self::Item> {
    self.inner.next()
  }
}

/// Re-owns a `RangeBounds` endpoint into a `Bound` of materialized components for
/// the internal range cursor.
fn materialize_bound<C, K>(bound: Bound<&K>) -> Bound<Vec<C>>
where
  C: Clone,
  K: RadixKey<Owned = C> + ?Sized,
{
  match bound {
    Bound::Included(k) => Bound::Included(k.components().map(K::to_owned).collect()),
    Bound::Excluded(k) => Bound::Excluded(k.components().map(K::to_owned).collect()),
    Bound::Unbounded => Bound::Unbounded,
  }
}

#[cfg(test)]
mod tests;

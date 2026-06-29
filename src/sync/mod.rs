//! The thread-safe (`Send + Sync`) immutable radix face.
//!
//! [`Radix`] is an **immutable, persistent** trie that fixes the internal pointer
//! kind to [`Arc`](std::sync::Arc): a value is a cheap, shareable snapshot that is
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

use core::{
  borrow::Borrow,
  ops::{Bound, RangeBounds},
};

use archery::ArcK;

#[cfg(feature = "watch")]
use event_listener::EventListener;

// `Listener::{wait, wait_timeout}` (the blocking waits) are the only trait methods
// used; they need std.
#[cfg(all(feature = "watch", feature = "std"))]
use event_listener::Listener;

use crate::{
  RadixKey,
  node::{RangeIter, RevValueIter, Root, SliceIter, ValueIter},
};

#[cfg(feature = "watch")]
use crate::node::WatchSlot;

/// A generic, persistent (copy-on-write) **immutable** radix trie, shareable across
/// threads.
///
/// Keys decompose into [`Component`](RadixKey::Component)s via [`RadixKey`]; the
/// trie is parameterized over the component type `C` and the value type `V`, and
/// uses [`Arc`](std::sync::Arc) pointers internally, so it is `Send + Sync`
/// whenever both `C` and `V` are (auto-derived — no `unsafe impl`).
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
  inner: Root<ArcK, C, V>,
  /// Change slot for the empty (`None`-root) position — there is no node to listen
  /// on while the trie is empty. It is *per empty-epoch*: a run of consecutive empty
  /// versions shares one slot (so a watch armed on any of them sees the first insert),
  /// and a fresh slot begins each time the trie becomes empty again. A later version's
  /// [`notify_changes_since`](Radix::notify_changes_since) fires the *base* version's
  /// slot when the trie went from empty to non-empty.
  #[cfg(feature = "watch")]
  empty: std::sync::Arc<WatchSlot>,
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
      empty: std::sync::Arc::new(WatchSlot::new()),
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
  /// Zero allocation: the key is walked lazily over its components.
  #[inline]
  pub fn get<K>(&self, key: &K) -> Option<&V>
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    self.inner.get(key.components())
  }

  /// Returns `true` if a value is stored at exactly `key`.
  #[inline]
  pub fn contains<K>(&self, key: &K) -> bool
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    self.get(key).is_some()
  }

  /// Returns the value of the deepest stored key that is a prefix of `key`,
  /// **inclusive** of an exact match (longest-prefix match).
  #[inline]
  pub fn get_ancestor<K>(&self, key: &K) -> Option<&V>
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    self.inner.ancestor(key.components(), true)
  }

  /// Returns the value of the deepest stored key that is a *strict* prefix of
  /// `key` (excludes an exact match).
  #[inline]
  pub fn strict_ancestor<K>(&self, key: &K) -> Option<&V>
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    self.inner.ancestor(key.components(), false)
  }

  /// Returns `true` if any stored key is a prefix of `key` (inclusive).
  #[inline]
  pub fn has_ancestor<K>(&self, key: &K) -> bool
  where
    K: RadixKey<Component = C> + ?Sized,
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
    K: RadixKey<Component = C> + ?Sized,
  {
    Ancestors {
      inner: self.inner.ancestors(key.components()),
    }
  }

  /// Iterates references to the values of every *strict* descendant of `key`
  /// (stored keys that strictly extend `key`; the value at `key` is excluded).
  #[inline]
  pub fn descendants<K>(&self, key: &K) -> Descendants<'_, C, V>
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    Descendants {
      inner: self.inner.descendants(key.components()),
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
    K: RadixKey<Component = C> + ?Sized,
  {
    RevDescendants {
      inner: self.inner.descendants_rev(key.components()),
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
    K: RadixKey<Component = C> + ?Sized,
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
    K: RadixKey<Component = C> + ?Sized,
    C: Clone,
  {
    let lower: Vec<C> = key.components().map(|c| c.borrow().clone()).collect();
    Range {
      inner: self.inner.range(Bound::Included(lower), Bound::Unbounded),
    }
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
  working: Root<ArcK, C, V>,
  /// The version this transaction was opened from, kept so `commit` can decide the
  /// committed tree's empty-epoch slot (carry the base's, or open a fresh one). The
  /// replaced-node diff itself runs later, in [`Radix::notify_changes_since`].
  #[cfg(feature = "watch")]
  base: Root<ArcK, C, V>,
  /// The base version's empty-position slot. `commit` carries it into the committed
  /// tree while the empty-epoch is unbroken (see [`Radix::notify_changes_since`]).
  #[cfg(feature = "watch")]
  empty: std::sync::Arc<WatchSlot>,
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

  /// Consumes the transaction and returns the next immutable [`Radix`] holding all
  /// committed edits.
  ///
  /// Committing builds the next tree only; it fires **no** `watch` events. Publishing
  /// follows **commit → publish → notify**: after the returned tree wins publication
  /// (e.g. a successful compare-and-swap), call [`Radix::notify_changes_since`]
  /// against the version this transaction was opened from — or fold the publish and
  /// notify together with [`Radix::publish_to`]. A committed-then-discarded tree (a
  /// lost CAS) must NOT notify.
  #[cfg(feature = "watch")]
  #[inline]
  #[must_use = "returns the new tree holding the committed edits"]
  pub fn commit(self) -> Radix<C, V> {
    let mut working = self.working;
    // Canonicalize a *logically* empty working copy to physical `None`-root FIRST, so
    // "empty ⟺ root is None" holds: a net-empty txn (insert-then-remove from an empty
    // base) commits `len == 0` but with a physical `root = Some(empty node)`, which
    // the publish-time diff would otherwise misread as a None -> Some transition and
    // use to spuriously wake empty-position watchers though nothing was published.
    working.canonicalize_empty();
    // Carry an empty-position slot *per empty-epoch* so the empty-then-insert no-op
    // case still wakes watchers (see `Radix::notify_changes_since`): a run of
    // consecutive empty versions shares one slot, and a fresh slot opens only when
    // the trie newly becomes empty. The slot is dormant for non-empty versions.
    let empty = if working.root_is_none() {
      if self.base.root_is_none() {
        // None -> None: same empty epoch, carry the base's slot so an arm on the
        // base and an arm on this commit fire together when a later insert lands.
        self.empty.clone()
      } else {
        // Some -> None: a new empty epoch begins; nothing armed on the prior
        // (non-empty) version's empty slot, so start fresh.
        std::sync::Arc::new(WatchSlot::new())
      }
    } else {
      // Non-empty: the empty slot is dormant; carry it so the epoch is preserved if
      // a later commit empties the trie back out without changing it in between.
      self.empty.clone()
    };
    Radix {
      inner: working,
      empty,
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
    K: RadixKey<Component = C> + ?Sized,
  {
    self.working.get(key.components())
  }

  /// Returns `true` if a value is stored at exactly `key` in the working copy.
  #[inline]
  pub fn contains<K>(&self, key: &K) -> bool
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    self.get(key).is_some()
  }

  /// Returns the value of the deepest stored key that is a prefix of `key`,
  /// **inclusive** of an exact match (longest-prefix match).
  #[inline]
  pub fn get_ancestor<K>(&self, key: &K) -> Option<&V>
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    self.working.ancestor(key.components(), true)
  }

  /// Returns the value of the deepest stored key that is a *strict* prefix of
  /// `key` (excludes an exact match).
  #[inline]
  pub fn strict_ancestor<K>(&self, key: &K) -> Option<&V>
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    self.working.ancestor(key.components(), false)
  }

  /// Returns `true` if any stored key is a prefix of `key` (inclusive).
  #[inline]
  pub fn has_ancestor<K>(&self, key: &K) -> bool
  where
    K: RadixKey<Component = C> + ?Sized,
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
    K: RadixKey<Component = C> + ?Sized,
  {
    Ancestors {
      inner: self.working.ancestors(key.components()),
    }
  }

  /// Iterates references to the values of every *strict* descendant of `key`
  /// (stored keys that strictly extend `key`; the value at `key` is excluded).
  #[inline]
  pub fn descendants<K>(&self, key: &K) -> Descendants<'_, C, V>
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    Descendants {
      inner: self.working.descendants(key.components()),
    }
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
    K: RadixKey<Component = C> + ?Sized,
  {
    self.working.insert(key.components(), value)
  }

  /// Removes and returns the value at exactly `key` in the working copy, if any.
  pub fn remove<K>(&mut self, key: &K) -> Option<V>
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    self.working.remove(|| key.components())
  }

  /// Removes every *strict* descendant of `key` (the value at `key`, if any, is
  /// kept). Returns the number of values removed. Clones no *removed* value — only
  /// the copy-on-write path to `key` may be duplicated, as in every mutator.
  pub fn remove_descendants<K>(&mut self, key: &K) -> usize
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    self.working.remove_descendants(|| key.components())
  }

  /// Removes every *strict* descendant of `key` and returns their values (the
  /// value at `key`, if any, is kept). Clones values out before unlinking.
  pub fn drain_descendants<K>(&mut self, key: &K) -> Vec<V>
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    self.working.drain_descendants(|| key.components())
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
    K: RadixKey<Component = C> + ?Sized,
  {
    self.working.delete_prefix(|| key.components())
  }

  /// Removes the value at `key` **and** every strict descendant (node-inclusive)
  /// and returns their values in ascending key order (the value at `key` itself,
  /// if any, first). Clones values out before unlinking.
  pub fn drain_prefix<K>(&mut self, key: &K) -> Vec<V>
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    self.working.drain_prefix(|| key.components())
  }
}

/// Iterator over references to every value in a [`Radix`], in key order.
///
/// Created by [`Radix::values`].
pub struct Values<'a, C, V> {
  inner: ValueIter<'a, ArcK, C, V>,
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

/// Iterator over references to the values of `key`'s strict descendants.
///
/// Created by [`Radix::descendants`].
pub struct Descendants<'a, C, V> {
  inner: ValueIter<'a, ArcK, C, V>,
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
  inner: RevValueIter<'a, ArcK, C, V>,
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
  inner: RevValueIter<'a, ArcK, C, V>,
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
  inner: RangeIter<'a, ArcK, C, V>,
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

/// Re-owns a `RangeBounds` endpoint into a `Bound` of materialized components for
/// the internal range cursor.
fn materialize_bound<C, K>(bound: Bound<&K>) -> Bound<Vec<C>>
where
  C: Clone,
  K: RadixKey<Component = C> + ?Sized,
{
  match bound {
    Bound::Included(k) => Bound::Included(k.components().map(|c| c.borrow().clone()).collect()),
    Bound::Excluded(k) => Bound::Excluded(k.components().map(|c| c.borrow().clone()).collect()),
    Bound::Unbounded => Bound::Unbounded,
  }
}

#[cfg(test)]
impl<C, V> Radix<C, V>
where
  C: Ord,
{
  /// Test-only: the true value count by walking the trie.
  pub(crate) fn count_values(&self) -> usize {
    self.inner.count_values()
  }

  /// Test-only: whether the trie is in canonical path-compressed form.
  pub(crate) fn is_canonical(&self) -> bool {
    self.inner.is_canonical()
  }

  /// Test-only: the child subtree pointer for the root edge starting with
  /// `first`, for structural-sharing (`ptr_eq`) assertions.
  pub(crate) fn edge_child(
    &self,
    first: &C,
  ) -> Option<archery::SharedPointer<crate::node::Node<ArcK, C, V>, ArcK>> {
    self.inner.edge_child(first)
  }
}

#[cfg(test)]
impl<C, V> Txn<C, V>
where
  C: Ord,
{
  /// Test-only: the true value count by walking the working copy.
  pub(crate) fn count_values(&self) -> usize {
    self.working.count_values()
  }

  /// Test-only: whether the working copy is in canonical path-compressed form.
  pub(crate) fn is_canonical(&self) -> bool {
    self.working.is_canonical()
  }
}

/// A pending observation of a key or prefix, returned by [`Radix::watch`] /
/// [`Radix::watch_prefix`] / [`Radix::get_watch`].
///
/// A `Watch` is **edge-triggered against the exact snapshot it was armed on**: it
/// resolves *once*, for the next change to the watched key (or anything in its
/// subtree) that is **published** to that version — that is, a later
/// [`commit`](Txn::commit) whose tree is then made live and calls
/// [`notify_changes_since`](Radix::notify_changes_since) (committing alone fires
/// nothing). It is single-use, and bound to that one version: never reuse a `Watch`
/// after it fires, and never arm against a snapshot different from the one you read.
///
/// A `Watch` is armed against one immutable snapshot: it registers a listener on
/// that snapshot's node slot and re-checks the node's sticky flag, so a change
/// published to that snapshot is never missed — and if the node was already replaced
/// by an earlier publish, the `Watch` is created already-resolved. (The listener plus
/// the sticky-flag re-check is what closes the race; the snapshot is immutable, so
/// whether you read a value before or after arming does not matter.) Use
/// [`get_watch`](Radix::get_watch) to read a value and arm against the *same*
/// snapshot in one call.
///
/// It may **over-notify**: because there is one change channel per node and any
/// ancestor of a change is path-copied, a change to a *descendant* (or a sibling
/// merged onto the same node) can wake a key watcher whose own value did not change
/// — re-read to confirm. It never under-notifies, given non-panicking key
/// comparisons and async wakers (a panic in either during notification is out of
/// scope, like a panicking `Drop`).
///
/// To track a key across versions, loop: read-and-arm via
/// [`get_watch`](Radix::get_watch), wait, then **reload the holder and re-arm**
/// against the new live version before waiting again.
///
/// ```no_run
/// # #[cfg(all(feature = "watch", feature = "std"))] {
/// use std::sync::Arc;
/// use arc_swap::ArcSwap;
/// use iradix::sync::Radix;
///
/// let holder: Arc<ArcSwap<Radix<u8, u32>>> = Arc::new(ArcSwap::from_pointee(Radix::new()));
/// loop {
///   let snap = holder.load_full();              // reload the live version
///   let (value, watch) = snap.get_watch(b"k".as_slice()); // read + arm on one snapshot
///   # let _ = value;
///   watch.block_wait();                         // block until the next published change
///   # break;
/// }
/// # }
/// ```
#[cfg(feature = "watch")]
pub struct Watch(Option<EventListener>);

/// The future returned by [`Watch::changed`].
///
/// Resolves (to `()`) when the watched key or prefix changes in a published
/// version — immediately if it already has. Runtime-agnostic and `no_std + alloc`:
/// drive it with any async executor.
#[cfg(feature = "watch")]
#[must_use = "futures do nothing unless awaited"]
pub struct Changed(Option<EventListener>);

#[cfg(feature = "watch")]
impl core::future::Future for Changed {
  type Output = ();

  #[inline]
  fn poll(
    self: core::pin::Pin<&mut Self>,
    cx: &mut core::task::Context<'_>,
  ) -> core::task::Poll<Self::Output> {
    // `EventListener` is `Unpin`, so the inner listener can be polled through a
    // plain `&mut` without structural pinning.
    match &mut self.get_mut().0 {
      Some(listener) => core::pin::Pin::new(listener).poll(cx),
      None => core::task::Poll::Ready(()),
    }
  }
}

#[cfg(feature = "watch")]
impl Watch {
  /// Blocks the current thread until the watched key or prefix changes (returns at
  /// once if it already has). The blocking counterpart to `changed`.
  #[cfg(feature = "std")]
  #[inline]
  pub fn block_wait(self) {
    if let Some(listener) = self.0 {
      listener.wait();
    }
  }

  /// Blocks until the watched key or prefix changes, or `timeout` elapses. Returns
  /// `true` if it changed (or already had), `false` on timeout. The blocking
  /// counterpart to `changed_timeout`.
  #[cfg(feature = "std")]
  #[inline]
  pub fn block_wait_timeout(self, timeout: core::time::Duration) -> bool {
    match self.0 {
      None => true,
      Some(listener) => listener.wait_timeout(timeout).is_some(),
    }
  }

  /// Returns a future that resolves when the watched key or prefix changes
  /// (immediately if it already has) — the async counterpart to `block_wait`.
  /// Runtime-agnostic: drive it with any async executor; works on `no_std + alloc`.
  #[inline]
  #[must_use = "the returned future does nothing unless awaited"]
  pub fn changed(self) -> Changed {
    Changed(self.0)
  }

  /// Like `changed`, but gives up after `timeout` — a lazy [`ChangedTimeout`] future.
  ///
  /// Resolves to `Ok(())` if the watched key or prefix changed in time, or
  /// `Err(`[`Elapsed`](agnostic_lite::time::Elapsed)`)` on timeout — the async
  /// counterpart to `block_wait_timeout`. The runtime is chosen with the type
  /// parameter `R` (e.g. `TokioRuntime`, `SmolRuntime`, `WasmRuntime`,
  /// `EmbassyRuntime`), so the crate stays runtime-agnostic; works on
  /// `no_std + alloc` (e.g. the embassy backend). The `agnostic-lite` feature brings
  /// only the `RuntimeLite` trait; turn on the `tokio`
  /// or `smol` feature for those backends, or add `agnostic-lite` with another
  /// backend, then name `R` from `agnostic_lite` (e.g.
  /// `agnostic_lite::tokio::TokioRuntime`).
  ///
  /// ```no_run
  /// # #[cfg(feature = "tokio")] {
  /// use core::time::Duration;
  /// use iradix::{TokioRuntime, sync::Watch};
  ///
  /// async fn reload_on_change(watch: Watch) {
  ///   match watch.changed_timeout::<TokioRuntime>(Duration::from_secs(5)).await {
  ///     Ok(()) => { /* changed in time — reload the holder and re-arm */ }
  ///     Err(_elapsed) => { /* timed out — still pending */ }
  ///   }
  /// }
  /// # }
  /// ```
  #[cfg(feature = "agnostic-lite")]
  #[cfg_attr(docsrs, doc(cfg(feature = "agnostic-lite")))]
  #[inline]
  pub fn changed_timeout<R>(self, timeout: core::time::Duration) -> ChangedTimeout<R>
  where
    R: agnostic_lite::RuntimeLite,
  {
    ChangedTimeout {
      changed: Some(self.changed()),
      timeout,
      armed: None,
    }
  }
}

/// The future returned by [`Watch::changed_timeout`]. Resolves to `Ok(())` when the
/// watched key or prefix changes in a published version, or
/// `Err(`[`Elapsed`](agnostic_lite::time::Elapsed)`)` once the timeout elapses.
///
/// Lazy: the runtime timer is built on the first poll (inside the executor), not when
/// `changed_timeout` is called — so constructing it never touches the runtime, and the
/// timeout budget starts when the future is first polled.
#[cfg(feature = "agnostic-lite")]
#[cfg_attr(docsrs, doc(cfg(feature = "agnostic-lite")))]
#[must_use = "futures do nothing unless awaited"]
pub struct ChangedTimeout<R: agnostic_lite::RuntimeLite> {
  changed: Option<Changed>,
  timeout: core::time::Duration,
  // Boxed so this future is `Unpin` even when the runtime's timeout is not, keeping
  // the crate free of unsafe pin projection; built on first poll for laziness.
  armed: Option<core::pin::Pin<std::boxed::Box<R::Timeout<Changed>>>>,
}

#[cfg(feature = "agnostic-lite")]
impl<R: agnostic_lite::RuntimeLite> core::future::Future for ChangedTimeout<R> {
  type Output = Result<(), agnostic_lite::time::Elapsed>;

  fn poll(
    self: core::pin::Pin<&mut Self>,
    cx: &mut core::task::Context<'_>,
  ) -> core::task::Poll<Self::Output> {
    let this = self.get_mut();
    if let Some(armed) = this.armed.as_mut() {
      return armed.as_mut().poll(cx);
    }
    // First poll: build the runtime timeout now, inside the executor — never at
    // construction, so it neither touches the runtime early nor starts the budget late.
    let changed = this
      .changed
      .take()
      .expect("changed is Some until the timeout is armed on first poll");
    this
      .armed
      .insert(std::boxed::Box::pin(R::timeout(this.timeout, changed)))
      .as_mut()
      .poll(cx)
  }
}

#[cfg(feature = "watch")]
impl<C, V> Radix<C, V>
where
  C: Ord,
{
  /// Wake every [`Watch`] armed on `base` whose key — or any key in its subtree —
  /// differs in `self`.
  ///
  /// Call EXACTLY ONCE, and ONLY AFTER `self` has been published as the live version
  /// (e.g. a winning [`ArcSwap`](https://docs.rs/arc-swap) compare-and-swap).
  /// Committing produces a tree but does not notify; a committed-then-discarded tree
  /// (a lost CAS) must NOT call this. May over-notify (a descendant change can wake a
  /// key watcher — re-read to confirm); never under-notifies, given non-panicking key
  /// comparisons.
  ///
  /// Firing is **all-or-nothing**: the changed nodes' slots are *collected* by a
  /// fallible diff walk (which runs user `C` comparisons), then fired in a separate
  /// loop. So a panicking `Ord`/`PartialEq` aborts the walk having fired nothing for
  /// this transition — matching the crate's strong-exception style. The fire loop
  /// calls each slot's `Event::notify`, which wakes any registered async waker; the
  /// guarantee assumes those wakers do not panic. A panicking waker (a `Waker`-
  /// contract violation) can abort the loop after a partial fire and is **out of
  /// scope**, exactly as a panicking `Drop` is for the mutators (see the crate docs).
  ///
  /// `self` is the newly published tree and `base` the version the producing
  /// transaction was opened from. See [`publish_to`](Radix::publish_to) to fold the
  /// publish and this notify into one call, and the [module docs](crate::sync#lock-free-sharing)
  /// for the commit → publish → notify discipline.
  #[inline]
  pub fn notify_changes_since(&self, base: &Self) {
    // Collect first (fallible: user comparisons), then fire (infallible). A panic in
    // the collect walk drops `slots`, firing nothing — so a transition either fires
    // every changed slot or none of them, never a partial set that strands watchers.
    let mut slots = Vec::new();
    let fire_empty = self.inner.collect_changes(&base.inner, &mut slots);
    for slot in slots {
      slot.fire();
    }
    if fire_empty {
      base.empty.fire();
    }
  }

  /// Publish-then-notify in one call.
  ///
  /// `swap` attempts to install `self` as the live version and returns `true` iff it
  /// won (became visible). On a win, fires notifications relative to `base` (via
  /// [`notify_changes_since`](Radix::notify_changes_since)); on a loss, fires
  /// nothing. This is the [`watch`](Radix::watch)-safe shape of the commit → publish
  /// → notify discipline: a tree that lost the race never notifies.
  ///
  /// ```no_run
  /// # #[cfg(feature = "watch")] {
  /// use std::sync::Arc;
  /// use arc_swap::ArcSwap;
  /// use iradix::sync::Radix;
  ///
  /// let holder: Arc<ArcSwap<Radix<u8, u32>>> = Arc::new(ArcSwap::from_pointee(Radix::new()));
  /// let base = holder.load_full();
  /// let next = Arc::new({
  ///   let mut t = base.txn();
  ///   t.insert(b"k".as_slice(), 1);
  ///   t.commit()
  /// });
  /// next.publish_to(&base, || {
  ///   let prev = holder.compare_and_swap(&base, Arc::clone(&next));
  ///   Arc::ptr_eq(&base, &prev) // true iff our CAS won
  /// });
  /// # }
  /// ```
  #[inline]
  pub fn publish_to(&self, base: &Self, swap: impl FnOnce() -> bool) {
    if swap() {
      self.notify_changes_since(base);
    }
  }

  /// Returns a [`Watch`] that fires when the value at `key` next changes in a
  /// published version.
  ///
  /// One change channel exists per node, so a change to a *descendant* of `key` also
  /// fires (the watch may wake without `key`'s own value changing); re-read and
  /// re-arm with a fresh `watch` on the new live version. On an empty trie the watch
  /// fires when the first value is inserted. The change must be *published* — a
  /// committed tree fires nothing until its
  /// [`notify_changes_since`](Radix::notify_changes_since) runs after it wins
  /// publication. Prefer [`get_watch`](Radix::get_watch) to read and arm on one snapshot.
  #[inline]
  pub fn watch<K>(&self, key: &K) -> Watch
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    self.watch_at(key)
  }

  /// Returns a [`Watch`] that fires when any key under `prefix` next changes in a
  /// published version (the whole subtree, inclusive of an exact match at `prefix`).
  #[inline]
  pub fn watch_prefix<K>(&self, prefix: &K) -> Watch
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    self.watch_at(prefix)
  }

  /// Read the value at `key` and arm a [`Watch`] for its next change against this one
  /// immutable snapshot, so the value and the watch are consistent. A version
  /// published afterward is caught by the `Watch` — or, if it already replaced this
  /// node, the `Watch` is already-resolved. Prefer this over separate `get` + `watch`.
  /// See [`watch`](Radix::watch) for the reload-and-re-arm loop.
  #[inline]
  pub fn get_watch<K>(&self, key: &K) -> (Option<&V>, Watch)
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    (self.get(key), self.watch_at(key))
  }

  #[inline]
  fn watch_at<K>(&self, key: &K) -> Watch
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    let (listener, already) = match self.inner.watch_slot(key.components()) {
      Some(slot) => slot.listen(),
      None => self.empty.listen(),
    };
    Watch(if already { None } else { Some(listener) })
  }
}

#[cfg(test)]
mod tests;

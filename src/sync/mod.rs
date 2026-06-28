//! The thread-safe (`Send + Sync`) immutable radix face.
//!
//! [`Radix`] is an **immutable, persistent** trie that fixes the internal pointer
//! kind to [`Arc`](std::sync::Arc): a value is a cheap, shareable snapshot that is
//! `Send + Sync` exactly when `V: Send + Sync` (both are *auto-derived* — the crate
//! declares no explicit `unsafe impl Send`/`Sync` anywhere). Every mutation returns
//! a *new* tree; the original is never observed to change.
//!
//! Mutate either with the one-op `&self` methods ([`insert`](Radix::insert),
//! [`remove`](Radix::remove), …), each of which returns the next tree, or by
//! opening a [`Txn`] — an owned working copy you edit freely and then
//! [`commit`](Txn::commit) into the next immutable tree. This mirrors
//! go-immutable-radix's `Tree` / `Txn` split.
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

use std::vec::Vec;

use core::{
  borrow::Borrow,
  ops::{Bound, RangeBounds},
};

use archery::ArcK;

use crate::{
  RadixKey,
  node::{RangeIter, RevValueIter, Root, SliceIter, ValueIter},
};

/// A generic, persistent (copy-on-write) **immutable** radix trie, shareable across
/// threads.
///
/// Keys decompose into [`Component`](RadixKey::Component)s via [`RadixKey`]; the
/// trie is parameterized over the component type `C` and the value type `V`, and
/// uses [`Arc`](std::sync::Arc) pointers internally. It is `Send + Sync` whenever
/// `V` is (auto-derived; the crate declares no explicit `unsafe impl`).
///
/// A `Radix` never mutates in place. A [`clone`](Clone::clone) is O(1) and shares
/// all structure with the original; each one-op mutator returns a *new* tree that
/// copies only the path from the root to the touched node, leaving every untouched
/// subtree physically shared with the prior version. This makes snapshots free and
/// isolation automatic.
///
/// Mutate one of two ways:
///
/// - one-op `&self` methods ([`insert`](Radix::insert) / [`remove`](Radix::remove)
///   / [`delete_prefix`](Radix::delete_prefix) / …), each returning the next tree
///   alongside whatever the operation reports; or
/// - a [`Txn`] (open with [`txn`](Radix::txn)) for a batch of edits, then
///   [`commit`](Txn::commit) into the next tree.
///
/// `iradix` ships no built-in concurrent holder: to publish versions atomically to
/// shared readers, hold the snapshot yourself — wrap it in an
/// `arc_swap::ArcSwap<Radix<…>>` (load to read; txn → commit → CAS to publish).
/// See the module docs and `examples/sync.rs`.
///
/// # Examples
///
/// ```
/// use iradix::sync::Radix;
///
/// // One-op insert returns the NEW tree; the original is untouched.
/// let base: Radix<u8, u32> = Radix::new();
/// let (t1, prev) = base.insert(b"abc".as_slice(), 1);
/// assert_eq!(prev, None);
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
/// add `C: Clone`; persistent mutators add `C: Ord + Clone, V: Clone`. Reads are
/// `V`-bound-free and return `&V`.
pub struct Radix<C, V> {
  inner: Root<ArcK, C, V>,
}

// O(1) clone — no `V: Clone` (the pointer is bumped, values are not touched).
impl<C, V> Clone for Radix<C, V> {
  #[inline]
  fn clone(&self) -> Self {
    Self {
      inner: self.inner.clone(),
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
  #[inline]
  pub const fn new() -> Self {
    Self { inner: Root::new() }
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
  pub fn txn(&self) -> Txn<C, V> {
    Txn {
      working: self.inner.clone(),
    }
  }

  /// Returns an empty tree. (Provided for symmetry with the other one-op
  /// mutators; equivalent to [`Radix::new`].)
  #[inline]
  pub fn clear(&self) -> Self {
    Self::new()
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

impl<C, V> Radix<C, V>
where
  C: Ord + Clone,
  V: Clone,
{
  /// Returns the tree with `value` inserted at `key`, alongside the previous value
  /// if the key was already set. The original tree is unchanged.
  ///
  /// A one-shot [`txn`](Radix::txn) → mutate → [`commit`](Txn::commit); to apply
  /// several edits, open a [`Txn`] yourself.
  pub fn insert<K>(&self, key: &K, value: V) -> (Self, Option<V>)
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    let mut t = self.txn();
    let prev = t.insert(key, value);
    (t.commit(), prev)
  }

  /// Returns the tree with the value at exactly `key` removed, alongside that value
  /// if it was present. The original tree is unchanged.
  pub fn remove<K>(&self, key: &K) -> (Self, Option<V>)
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    let mut t = self.txn();
    let prev = t.remove(key);
    (t.commit(), prev)
  }

  /// Returns the tree with every *strict* descendant of `key` removed (the value at
  /// `key`, if any, is kept), alongside the number removed. The original tree is
  /// unchanged.
  pub fn remove_descendants<K>(&self, key: &K) -> (Self, usize)
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    let mut t = self.txn();
    let removed = t.remove_descendants(key);
    (t.commit(), removed)
  }

  /// Returns the tree with every *strict* descendant of `key` removed (the value at
  /// `key`, if any, is kept), alongside the removed values. The original tree is
  /// unchanged.
  pub fn drain_descendants<K>(&self, key: &K) -> (Self, Vec<V>)
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    let mut t = self.txn();
    let drained = t.drain_descendants(key);
    (t.commit(), drained)
  }

  /// Returns the tree with the value at `key` **and** every strict descendant
  /// removed (node-inclusive; go-immutable-radix's `DeletePrefix`), alongside the
  /// number removed. The original tree is unchanged.
  ///
  /// Contrast [`remove_descendants`](Radix::remove_descendants), which keeps the
  /// value stored at `key` itself.
  pub fn delete_prefix<K>(&self, key: &K) -> (Self, usize)
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    let mut t = self.txn();
    let removed = t.delete_prefix(key);
    (t.commit(), removed)
  }

  /// Returns the tree with the value at `key` **and** every strict descendant
  /// removed (node-inclusive), alongside the removed values in ascending key order
  /// (the value at `key` itself, if any, first). The original tree is unchanged.
  pub fn drain_prefix<K>(&self, key: &K) -> (Self, Vec<V>)
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    let mut t = self.txn();
    let drained = t.drain_prefix(key);
    (t.commit(), drained)
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
/// A transaction owns its data: it borrows nothing and has no lifetime, so it is
/// freely movable (including across threads when `C` and `V` are `Send`).
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
  #[inline]
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
    let components: Vec<C> = key.components().map(|c| c.borrow().clone()).collect();
    self.working.insert(&components, value)
  }

  /// Removes and returns the value at exactly `key` in the working copy, if any.
  pub fn remove<K>(&mut self, key: &K) -> Option<V>
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    let components: Vec<C> = key.components().map(|c| c.borrow().clone()).collect();
    self.working.remove(&components)
  }

  /// Removes every *strict* descendant of `key` (the value at `key`, if any, is
  /// kept). Returns the number of values removed. Clones no *removed* value — only
  /// the copy-on-write path to `key` may be duplicated, as in every mutator.
  pub fn remove_descendants<K>(&mut self, key: &K) -> usize
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    let components: Vec<C> = key.components().map(|c| c.borrow().clone()).collect();
    self.working.remove_descendants(&components)
  }

  /// Removes every *strict* descendant of `key` and returns their values (the
  /// value at `key`, if any, is kept). Clones values out before unlinking.
  pub fn drain_descendants<K>(&mut self, key: &K) -> Vec<V>
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    let components: Vec<C> = key.components().map(|c| c.borrow().clone()).collect();
    self.working.drain_descendants(&components)
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
    let components: Vec<C> = key.components().map(|c| c.borrow().clone()).collect();
    self.working.delete_prefix(&components)
  }

  /// Removes the value at `key` **and** every strict descendant (node-inclusive)
  /// and returns their values in ascending key order (the value at `key` itself,
  /// if any, first). Clones values out before unlinking.
  pub fn drain_prefix<K>(&mut self, key: &K) -> Vec<V>
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    let components: Vec<C> = key.components().map(|c| c.borrow().clone()).collect();
    self.working.drain_prefix(&components)
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

#[cfg(test)]
mod tests;

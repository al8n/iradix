//! The single-threaded (`!Send`) persistent radix face.
//!
//! [`Radix`] fixes the internal pointer kind to [`Rc`](std::rc::Rc): clones are
//! cheap, refcounts are non-atomic, and the trie is confined to one thread. It is
//! the right choice when no snapshot ever crosses a thread boundary. For a
//! cross-thread snapshot, use [`crate::sync`].

use std::{borrow::ToOwned, vec::Vec};

use core::{
  borrow::Borrow,
  ops::{Bound, RangeBounds},
};

use archery::RcK;

use crate::{
  RadixKey,
  node::{RangeIter, RevValueIter, Root, SliceIter, ValueIter},
};

/// A generic, persistent (copy-on-write) radix trie, confined to one thread.
///
/// Keys decompose into [`Component`](RadixKey::Component)s via [`RadixKey`]; the
/// trie is parameterized over the component type `C` and the value type `V`, and
/// uses [`Rc`](std::rc::Rc) pointers internally (so it is `!Send` / `!Sync`).
///
/// Every mutation produces a logically new trie. A [`clone`](Clone::clone) is
/// O(1) and shares all structure with the original; a write copies only the path
/// from the root to the touched node, leaving every untouched subtree physically
/// shared with prior versions. This makes snapshots free and isolation automatic.
///
/// A batch of edits is simply a sequence of direct `&mut self` calls; `.clone()`
/// before the batch keeps the prior snapshot fully isolated.
///
/// # Bounds
///
/// The struct itself requires only the structural `C: ?Sized + ToOwned`. Reads
/// add `C: Ord`; persistent mutators add `C: Ord, C::Owned: Clone, V: Clone`.
/// Reads are `V`-bound-free and return `&V`.
pub struct Radix<C, V>
where
  C: ?Sized + ToOwned,
{
  inner: Root<RcK, C, V>,
}

// O(1) clone — no `V: Clone` (the pointer is bumped, values are not touched).
impl<C, V> Clone for Radix<C, V>
where
  C: ?Sized + ToOwned,
{
  #[inline]
  fn clone(&self) -> Self {
    Self {
      inner: self.inner.clone(),
    }
  }
}

impl<C, V> Default for Radix<C, V>
where
  C: ?Sized + ToOwned,
{
  #[inline]
  fn default() -> Self {
    Self::new()
  }
}

impl<C, V> Radix<C, V>
where
  C: ?Sized + ToOwned,
{
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

  /// Removes every value, resetting the trie to empty.
  #[inline]
  pub fn clear(&mut self) {
    self.inner.clear();
  }
}

// ----- reads (C: Ord, V-bound-free) ---------------------------------------

impl<C, V> Radix<C, V>
where
  C: ?Sized + ToOwned + Ord,
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
  pub fn minimum(&self) -> Option<(Vec<C::Owned>, &V)> {
    self.inner.minimum()
  }

  /// Returns the largest key (component lexicographic order) and its value, or
  /// `None` if the trie is empty.
  #[inline]
  pub fn maximum(&self) -> Option<(Vec<C::Owned>, &V)> {
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
  {
    let lower: Vec<C::Owned> = key.components().map(|c| c.borrow().to_owned()).collect();
    Range {
      inner: self.inner.range(Bound::Included(lower), Bound::Unbounded),
    }
  }
}

// ----- mutators (C: Ord, C::Owned: Clone, V: Clone) -----------------------

impl<C, V> Radix<C, V>
where
  C: ?Sized + ToOwned + Ord,
  C::Owned: Clone,
  V: Clone,
{
  /// Inserts `value` at `key`, returning the previous value if the key was set.
  pub fn insert<K>(&mut self, key: &K, value: V) -> Option<V>
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    let components: Vec<C::Owned> = key.components().map(|c| c.borrow().to_owned()).collect();
    self.inner.insert(&components, value)
  }

  /// Removes and returns the value at exactly `key`, if any.
  pub fn remove<K>(&mut self, key: &K) -> Option<V>
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    let components: Vec<C::Owned> = key.components().map(|c| c.borrow().to_owned()).collect();
    self.inner.remove(&components)
  }

  /// Removes every *strict* descendant of `key` (the value at `key`, if any, is
  /// kept). Returns the number of values removed. Never clones a `V`.
  pub fn remove_descendants<K>(&mut self, key: &K) -> usize
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    let components: Vec<C::Owned> = key.components().map(|c| c.borrow().to_owned()).collect();
    self.inner.remove_descendants(&components)
  }

  /// Removes every *strict* descendant of `key` and returns their values (the
  /// value at `key`, if any, is kept). Clones values out before unlinking.
  pub fn drain_descendants<K>(&mut self, key: &K) -> Vec<V>
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    let components: Vec<C::Owned> = key.components().map(|c| c.borrow().to_owned()).collect();
    self.inner.drain_descendants(&components)
  }

  /// Removes the value at `key` **and** every strict descendant (node-inclusive;
  /// go-immutable-radix's `DeletePrefix`), returning the number of values removed.
  ///
  /// Contrast [`remove_descendants`](Radix::remove_descendants), which keeps the
  /// value stored at `key` itself. Never clones a `V`.
  pub fn delete_prefix<K>(&mut self, key: &K) -> usize
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    let components: Vec<C::Owned> = key.components().map(|c| c.borrow().to_owned()).collect();
    self.inner.delete_prefix(&components)
  }

  /// Removes the value at `key` **and** every strict descendant (node-inclusive)
  /// and returns their values in ascending key order (the value at `key` itself,
  /// if any, first). Clones values out before unlinking.
  pub fn drain_prefix<K>(&mut self, key: &K) -> Vec<V>
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    let components: Vec<C::Owned> = key.components().map(|c| c.borrow().to_owned()).collect();
    self.inner.drain_prefix(&components)
  }
}

// ----- iterators ----------------------------------------------------------

/// Iterator over references to every value in a [`Radix`], in key order.
///
/// Created by [`Radix::values`].
pub struct Values<'a, C, V>
where
  C: ?Sized + ToOwned,
{
  inner: ValueIter<'a, RcK, C, V>,
}

impl<'a, C, V> Iterator for Values<'a, C, V>
where
  C: ?Sized + ToOwned,
{
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
pub struct Descendants<'a, C, V>
where
  C: ?Sized + ToOwned,
{
  inner: ValueIter<'a, RcK, C, V>,
}

impl<'a, C, V> Iterator for Descendants<'a, C, V>
where
  C: ?Sized + ToOwned,
{
  type Item = &'a V;

  #[inline]
  fn next(&mut self) -> Option<Self::Item> {
    self.inner.next()
  }
}

/// Iterator over references to every value in a [`Radix`], in reverse key order.
///
/// Created by [`Radix::values_rev`].
pub struct RevValues<'a, C, V>
where
  C: ?Sized + ToOwned,
{
  inner: RevValueIter<'a, RcK, C, V>,
}

impl<'a, C, V> Iterator for RevValues<'a, C, V>
where
  C: ?Sized + ToOwned,
{
  type Item = &'a V;

  #[inline]
  fn next(&mut self) -> Option<Self::Item> {
    self.inner.next()
  }
}

/// Iterator over references to `key`'s strict descendants, in reverse key order.
///
/// Created by [`Radix::descendants_rev`].
pub struct RevDescendants<'a, C, V>
where
  C: ?Sized + ToOwned,
{
  inner: RevValueIter<'a, RcK, C, V>,
}

impl<'a, C, V> Iterator for RevDescendants<'a, C, V>
where
  C: ?Sized + ToOwned,
{
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
pub struct Range<'a, C, V>
where
  C: ?Sized + ToOwned,
{
  inner: RangeIter<'a, RcK, C, V>,
}

impl<'a, C, V> Iterator for Range<'a, C, V>
where
  C: ?Sized + ToOwned + Ord,
{
  type Item = (Vec<C::Owned>, &'a V);

  #[inline]
  fn next(&mut self) -> Option<Self::Item> {
    self.inner.next()
  }
}

/// Re-owns a `RangeBounds` endpoint into a `Bound` of materialized components for
/// the internal range cursor.
fn materialize_bound<C, K>(bound: Bound<&K>) -> Bound<Vec<C::Owned>>
where
  C: ?Sized + ToOwned,
  K: RadixKey<Component = C> + ?Sized,
{
  match bound {
    Bound::Included(k) => Bound::Included(k.components().map(|c| c.borrow().to_owned()).collect()),
    Bound::Excluded(k) => Bound::Excluded(k.components().map(|c| c.borrow().to_owned()).collect()),
    Bound::Unbounded => Bound::Unbounded,
  }
}

#[cfg(test)]
impl<C, V> Radix<C, V>
where
  C: ?Sized + ToOwned + Ord,
{
  /// Test-only: number of direct edges from the root.
  pub(crate) fn root_child_count(&self) -> usize {
    self.inner.root_child_count()
  }

  /// Test-only: whether the root holds a value.
  pub(crate) fn root_has_value(&self) -> bool {
    self.inner.root_has_value()
  }

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
  ) -> Option<archery::SharedPointer<crate::node::Node<RcK, C, V>, RcK>> {
    self.inner.edge_child(first)
  }
}

#[cfg(test)]
mod tests;

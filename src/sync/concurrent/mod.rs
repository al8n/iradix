//! Lock-free shared holder for a [`sync::Radix`](super::Radix).
//!
//! [`ConcurrentRadix`] lets many readers take wait-free consistent snapshots while
//! writers build a private working copy and publish it with a single
//! compare-and-swap. Because the working copy is private until [`Txn::commit`], a
//! whole batch of edits is atomic *and* panic-safe: a mid-build panic just drops
//! the private copy and publishes nothing.

use core::fmt;

use super::Radix;

#[cfg(feature = "std")]
use std::sync::Arc;

#[cfg(all(not(feature = "std"), feature = "alloc"))]
use std::borrow::ToOwned;

/// Returned by [`Txn::commit`] when another writer published a new version
/// between [`ConcurrentRadix::txn`] and the commit (the transaction lost the
/// race). The caller should rebuild on a fresh [`txn`](ConcurrentRadix::txn) and
/// retry; [`ConcurrentRadix::commit_with`] does this automatically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Conflict;

impl Conflict {
  /// Returns a human-readable description of the conflict.
  #[inline]
  pub const fn as_str(&self) -> &'static str {
    "iradix commit conflict: the holder was updated since the transaction began"
  }
}

impl fmt::Display for Conflict {
  #[inline]
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

#[cfg(feature = "std")]
impl std::error::Error for Conflict {}

// ===== std backend: arc_swap ==============================================

/// A lock-free shared holder for a [`sync::Radix`](super::Radix).
///
/// Readers call [`load`](ConcurrentRadix::load) for a wait-free, point-in-time
/// snapshot. Writers call [`txn`](ConcurrentRadix::txn) to obtain an owned working
/// copy, mutate it freely, then [`commit`](Txn::commit) to publish it with one
/// compare-and-swap; a concurrent publish since `txn` makes `commit` return a
/// [`Conflict`] for the caller to retry.
///
/// # Backends
///
/// - **`std`** — [`arc_swap::ArcSwap`] holding an `Arc<Radix>`: reads are
///   wait-free and `commit` is a single CAS.
/// - **`alloc`** (no_std) — a [`spin::RwLock`] over the root with a generation
///   counter, preserving the same conflict-on-lost-race semantics.
///
/// The `lockfree-nostd` feature is **reserved** for a future hazard-pointer
/// (`haphazard`) lock-free no_std backend; it currently only ensures the `alloc`
/// tier (and thus the `spin` backend) is present.
#[cfg(feature = "std")]
pub struct ConcurrentRadix<C, V>
where
  C: ?Sized + ToOwned,
{
  current: arc_swap::ArcSwap<Radix<C, V>>,
}

#[cfg(feature = "std")]
impl<C, V> ConcurrentRadix<C, V>
where
  C: ?Sized + ToOwned,
{
  /// Creates a holder around an empty trie.
  #[inline]
  pub fn new() -> Self {
    Self::from_radix(Radix::new())
  }

  /// Creates a holder around an existing trie.
  #[inline]
  pub fn from_radix(radix: Radix<C, V>) -> Self {
    Self {
      current: arc_swap::ArcSwap::from_pointee(radix),
    }
  }

  /// Returns a wait-free, point-in-time snapshot. O(1); shares structure with the
  /// live trie.
  #[inline]
  pub fn load(&self) -> Radix<C, V> {
    Radix::clone(&self.current.load())
  }

  /// Starts a transaction: snapshots the current root and hands back an owned
  /// working copy to mutate. The edits are private until [`Txn::commit`].
  #[inline]
  pub fn txn(&self) -> Txn<'_, C, V> {
    let base = self.current.load_full();
    let working = Radix::clone(&base);
    Txn {
      holder: self,
      base,
      working,
    }
  }
}

#[cfg(feature = "std")]
impl<C, V> Default for ConcurrentRadix<C, V>
where
  C: ?Sized + ToOwned,
{
  #[inline]
  fn default() -> Self {
    Self::new()
  }
}

/// A private working copy of a [`ConcurrentRadix`], published atomically on
/// [`commit`](Txn::commit).
///
/// Mutate the working copy through the forwarded [`sync::Radix`](super::Radix)
/// mutators (or [`radix_mut`](Txn::radix_mut) for the full API); reads observe
/// the in-progress working copy. Dropping a `Txn` without committing discards every
/// edit — nothing is published — which is exactly what makes a panicking build
/// safe.
#[cfg(feature = "std")]
pub struct Txn<'a, C, V>
where
  C: ?Sized + ToOwned,
{
  holder: &'a ConcurrentRadix<C, V>,
  base: Arc<Radix<C, V>>,
  working: Radix<C, V>,
}

#[cfg(feature = "std")]
impl<C, V> Txn<'_, C, V>
where
  C: ?Sized + ToOwned,
{
  /// Publishes the working copy with a single compare-and-swap.
  ///
  /// Succeeds if no other writer published since [`txn`](ConcurrentRadix::txn);
  /// otherwise the working copy is dropped and [`Conflict`] is returned so the
  /// caller can retry on a fresh transaction.
  pub fn commit(self) -> Result<(), Conflict> {
    let new = Arc::new(self.working);
    // Pointer-identity CAS: if the holder still points at the exact `Arc` we
    // snapshotted, the swap happens and the returned previous equals `base`.
    let prev = self.holder.current.compare_and_swap(&self.base, new);
    if Arc::ptr_eq(&arc_swap::Guard::into_inner(prev), &self.base) {
      Ok(())
    } else {
      Err(Conflict)
    }
  }
}

// ===== no_std (alloc) backend: spin::RwLock ===============================

/// A lock-free shared holder for a [`sync::Radix`](super::Radix). See the `std`
/// variant for full documentation; this no_std build uses a [`spin::RwLock`] over
/// the root with a generation counter to keep the same conflict semantics.
#[cfg(all(not(feature = "std"), feature = "alloc"))]
pub struct ConcurrentRadix<C, V>
where
  C: ?Sized + ToOwned,
{
  // The generation increments on every successful commit; a `Txn` records the
  // generation it snapshotted, and `commit` only publishes if it is unchanged.
  current: spin::RwLock<(u64, Radix<C, V>)>,
}

#[cfg(all(not(feature = "std"), feature = "alloc"))]
impl<C, V> ConcurrentRadix<C, V>
where
  C: ?Sized + ToOwned,
{
  /// Creates a holder around an empty trie.
  #[inline]
  pub fn new() -> Self {
    Self::from_radix(Radix::new())
  }

  /// Creates a holder around an existing trie.
  #[inline]
  pub fn from_radix(radix: Radix<C, V>) -> Self {
    Self {
      current: spin::RwLock::new((0, radix)),
    }
  }

  /// Returns a point-in-time snapshot. O(1); shares structure with the live trie.
  #[inline]
  pub fn load(&self) -> Radix<C, V> {
    self.current.read().1.clone()
  }

  /// Starts a transaction: snapshots the current root (and its generation) and
  /// hands back an owned working copy to mutate. Edits are private until commit.
  #[inline]
  pub fn txn(&self) -> Txn<'_, C, V> {
    let (generation, working) = {
      let guard = self.current.read();
      (guard.0, guard.1.clone())
    };
    Txn {
      holder: self,
      generation,
      working,
    }
  }
}

#[cfg(all(not(feature = "std"), feature = "alloc"))]
impl<C, V> Default for ConcurrentRadix<C, V>
where
  C: ?Sized + ToOwned,
{
  #[inline]
  fn default() -> Self {
    Self::new()
  }
}

/// A private working copy of a [`ConcurrentRadix`], published atomically on
/// [`commit`](Txn::commit). See the `std` variant for full documentation.
#[cfg(all(not(feature = "std"), feature = "alloc"))]
pub struct Txn<'a, C, V>
where
  C: ?Sized + ToOwned,
{
  holder: &'a ConcurrentRadix<C, V>,
  generation: u64,
  working: Radix<C, V>,
}

#[cfg(all(not(feature = "std"), feature = "alloc"))]
impl<C, V> Txn<'_, C, V>
where
  C: ?Sized + ToOwned,
{
  /// Publishes the working copy under the write lock.
  ///
  /// Succeeds if no other writer committed since [`txn`](ConcurrentRadix::txn)
  /// (the generation is unchanged); otherwise returns [`Conflict`].
  pub fn commit(self) -> Result<(), Conflict> {
    let old = {
      let mut guard = self.holder.current.write();
      if guard.0 != self.generation {
        return Err(Conflict);
      }
      guard.0 = guard.0.wrapping_add(1);
      // Swap the new trie in and lift the old one OUT under the lock, but defer
      // its drop until after the guard is released (below). A stored value's
      // (non-panicking) `Drop` may re-enter this holder via `load`/`txn`/
      // `commit_with`; dropping it while the write lock is held would deadlock on
      // the non-reentrant spin lock.
      core::mem::replace(&mut guard.1, self.working)
    };
    drop(old);
    Ok(())
  }
}

// ===== shared `Txn` surface (both backends) ===============================

impl<C, V> Txn<'_, C, V>
where
  C: ?Sized + ToOwned,
{
  /// Borrows the working copy for reads (sees the in-progress edits).
  #[inline]
  pub const fn radix(&self) -> &Radix<C, V> {
    &self.working
  }

  /// Borrows the working copy mutably for the full [`sync::Radix`](super::Radix)
  /// mutation API.
  #[inline]
  pub const fn radix_mut(&mut self) -> &mut Radix<C, V> {
    &mut self.working
  }
}

impl<C, V> Txn<'_, C, V>
where
  C: ?Sized + ToOwned + Ord,
{
  /// Returns the value of the deepest stored prefix of `key`, inclusive.
  #[inline]
  pub fn get_ancestor<K>(&self, key: &K) -> Option<&V>
  where
    K: crate::RadixKey<Component = C> + ?Sized,
  {
    self.working.get_ancestor(key)
  }

  /// Returns the value of the deepest stored *strict* prefix of `key`.
  #[inline]
  pub fn strict_ancestor<K>(&self, key: &K) -> Option<&V>
  where
    K: crate::RadixKey<Component = C> + ?Sized,
  {
    self.working.strict_ancestor(key)
  }

  /// Returns `true` if any stored key is a prefix of `key` (inclusive).
  #[inline]
  pub fn has_ancestor<K>(&self, key: &K) -> bool
  where
    K: crate::RadixKey<Component = C> + ?Sized,
  {
    self.working.has_ancestor(key)
  }

  /// Returns the smallest key (component lexicographic order) and its value in the
  /// working copy.
  #[inline]
  pub fn minimum(&self) -> Option<(std::vec::Vec<C::Owned>, &V)> {
    self.working.minimum()
  }

  /// Returns the largest key (component lexicographic order) and its value in the
  /// working copy.
  #[inline]
  pub fn maximum(&self) -> Option<(std::vec::Vec<C::Owned>, &V)> {
    self.working.maximum()
  }

  /// Iterates references to every value in the working copy, in reverse key order.
  #[inline]
  #[must_use]
  pub fn values_rev(&self) -> super::RevValues<'_, C, V> {
    self.working.values_rev()
  }

  /// Iterates references to `key`'s strict descendants in the working copy, in
  /// reverse key order.
  #[inline]
  #[must_use]
  pub fn descendants_rev<K>(&self, key: &K) -> super::RevDescendants<'_, C, V>
  where
    K: crate::RadixKey<Component = C> + ?Sized,
  {
    self.working.descendants_rev(key)
  }

  /// Iterates `(key, value)` for every working-copy entry within `range`, in
  /// ascending key order.
  #[inline]
  #[must_use]
  pub fn range<K, R>(&self, range: R) -> super::Range<'_, C, V>
  where
    K: crate::RadixKey<Component = C> + ?Sized,
    R: core::ops::RangeBounds<K>,
  {
    self.working.range(range)
  }

  /// Returns a forward cursor over the working copy positioned at the first entry
  /// whose key is `>= key`, then ascending.
  #[inline]
  #[must_use]
  pub fn seek_lower_bound<K>(&self, key: &K) -> super::Range<'_, C, V>
  where
    K: crate::RadixKey<Component = C> + ?Sized,
  {
    self.working.seek_lower_bound(key)
  }
}

impl<C, V> Txn<'_, C, V>
where
  C: ?Sized + ToOwned + Ord,
  C::Owned: Clone,
  V: Clone,
{
  /// Inserts `value` at `key` in the working copy, returning the previous value.
  #[inline]
  pub fn insert<K>(&mut self, key: &K, value: V) -> Option<V>
  where
    K: crate::RadixKey<Component = C> + ?Sized,
  {
    self.working.insert(key, value)
  }

  /// Removes and returns the value at exactly `key` from the working copy.
  #[inline]
  pub fn remove<K>(&mut self, key: &K) -> Option<V>
  where
    K: crate::RadixKey<Component = C> + ?Sized,
  {
    self.working.remove(key)
  }

  /// Removes every strict descendant of `key` from the working copy, returning
  /// the count.
  #[inline]
  pub fn remove_descendants<K>(&mut self, key: &K) -> usize
  where
    K: crate::RadixKey<Component = C> + ?Sized,
  {
    self.working.remove_descendants(key)
  }

  /// Removes every strict descendant of `key` from the working copy, returning
  /// their values.
  #[inline]
  pub fn drain_descendants<K>(&mut self, key: &K) -> std::vec::Vec<V>
  where
    K: crate::RadixKey<Component = C> + ?Sized,
  {
    self.working.drain_descendants(key)
  }

  /// Removes the value at `key` and every strict descendant (node-inclusive) from
  /// the working copy, returning the count.
  #[inline]
  pub fn delete_prefix<K>(&mut self, key: &K) -> usize
  where
    K: crate::RadixKey<Component = C> + ?Sized,
  {
    self.working.delete_prefix(key)
  }

  /// Removes the value at `key` and every strict descendant (node-inclusive) from
  /// the working copy, returning their values in ascending key order.
  #[inline]
  pub fn drain_prefix<K>(&mut self, key: &K) -> std::vec::Vec<V>
  where
    K: crate::RadixKey<Component = C> + ?Sized,
  {
    self.working.drain_prefix(key)
  }
}

// ===== retry convenience (both backends) ==================================

impl<C, V> ConcurrentRadix<C, V>
where
  C: ?Sized + ToOwned,
{
  /// Runs `build` against a fresh transaction and commits, retrying from a new
  /// snapshot on every [`Conflict`] until the publish wins.
  ///
  /// `build` must be idempotent across retries (it may run more than once); it is
  /// handed the working copy's [`Txn`] and returns a value carried out on success.
  pub fn commit_with<R>(&self, mut build: impl FnMut(&mut Txn<'_, C, V>) -> R) -> R {
    loop {
      let mut txn = self.txn();
      let out = build(&mut txn);
      if txn.commit().is_ok() {
        return out;
      }
    }
  }
}

#[cfg(test)]
mod tests;

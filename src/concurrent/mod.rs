use std::borrow::ToOwned;

use archery::{ArcK, SharedPointerKind};

use crate::{RadixKey, radix::Radix};

#[cfg(feature = "std")]
use std::sync::Mutex;

/// A shared, concurrently-readable [`Radix`] holder.
///
/// Wraps a [`Radix`] so that readers take wait-free, point-in-time
/// [`snapshot`](ConcurrentRadix::snapshot)s while a single serialized writer
/// applies a [`write`](ConcurrentRadix::write) transaction and publishes the new
/// root in one step. Readers never block writers and writers never block readers;
/// concurrent writers are serialized so each sees the others' committed state.
///
/// # Backends
///
/// The atomic-root mechanism is feature-selected:
///
/// - **`std`** — [`arc_swap::ArcSwap`], a wait-free RCU-style swap. Reads
///   ([`snapshot`](ConcurrentRadix::snapshot), [`get_ancestor`](ConcurrentRadix::get_ancestor))
///   are lock-free.
/// - **`alloc`** (no_std) — [`spin::RwLock`]. Reads take a brief read lock.
///
/// The `lockfree-nostd` feature is **reserved** for a future hazard-pointer
/// (`haphazard`) lock-free no_std backend; it is not yet active, and enabling it
/// currently leaves the `spin::RwLock` backend in place. A correct hazard-pointer
/// implementation needs careful `Domain` management and `'static` bounds, and is
/// deferred rather than landed unverified.
///
/// `P` defaults to [`ArcK`] because snapshots cross threads.
pub struct ConcurrentRadix<C, V, P = ArcK>
where
  C: ?Sized + ToOwned,
  P: SharedPointerKind,
{
  inner: Inner<C, V, P>,
}

#[cfg(feature = "std")]
struct Inner<C, V, P>
where
  C: ?Sized + ToOwned,
  P: SharedPointerKind,
{
  current: arc_swap::ArcSwap<Radix<C, V, P>>,
  write_lock: Mutex<()>,
}

#[cfg(all(not(feature = "std"), feature = "alloc"))]
struct Inner<C, V, P>
where
  C: ?Sized + ToOwned,
  P: SharedPointerKind,
{
  current: spin::RwLock<Radix<C, V, P>>,
}

impl<C, V, P> ConcurrentRadix<C, V, P>
where
  C: ?Sized + ToOwned,
  P: SharedPointerKind,
{
  /// Creates a holder around an empty trie.
  #[inline]
  pub fn new() -> Self {
    Self::from_radix(Radix::new())
  }
}

impl<C, V, P> Default for ConcurrentRadix<C, V, P>
where
  C: ?Sized + ToOwned,
  P: SharedPointerKind,
{
  #[inline]
  fn default() -> Self {
    Self::new()
  }
}

#[cfg(feature = "std")]
impl<C, V, P> ConcurrentRadix<C, V, P>
where
  C: ?Sized + ToOwned,
  P: SharedPointerKind,
{
  /// Creates a holder around an existing trie.
  #[inline]
  pub fn from_radix(radix: Radix<C, V, P>) -> Self {
    Self {
      inner: Inner {
        current: arc_swap::ArcSwap::from_pointee(radix),
        write_lock: Mutex::new(()),
      },
    }
  }

  /// Returns a point-in-time snapshot. O(1); shares structure with the live trie.
  #[inline]
  pub fn snapshot(&self) -> Radix<C, V, P> {
    Radix::clone(&self.inner.current.load())
  }
}

#[cfg(all(not(feature = "std"), feature = "alloc"))]
impl<C, V, P> ConcurrentRadix<C, V, P>
where
  C: ?Sized + ToOwned,
  P: SharedPointerKind,
{
  /// Creates a holder around an existing trie.
  #[inline]
  pub fn from_radix(radix: Radix<C, V, P>) -> Self {
    Self {
      inner: Inner {
        current: spin::RwLock::new(radix),
      },
    }
  }

  /// Returns a point-in-time snapshot. O(1); shares structure with the live trie.
  #[inline]
  pub fn snapshot(&self) -> Radix<C, V, P> {
    self.inner.current.read().clone()
  }
}

// ----- reads --------------------------------------------------------------

impl<C, V, P> ConcurrentRadix<C, V, P>
where
  C: ?Sized + ToOwned + Ord,
  P: SharedPointerKind,
{
  /// Returns the value of the deepest stored prefix of `key`, inclusive,
  /// cloning it out of the current snapshot.
  ///
  /// Lock-free under the `std` backend.
  pub fn get_ancestor<K>(&self, key: &K) -> Option<V>
  where
    K: RadixKey<Component = C> + ?Sized,
    V: Clone,
  {
    self.snapshot_read(|r| r.get_ancestor(key).cloned())
  }

  /// Returns whether any stored key is a prefix of `key` (inclusive).
  ///
  /// Lock-free under the `std` backend.
  pub fn has_ancestor<K>(&self, key: &K) -> bool
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    self.snapshot_read(|r| r.has_ancestor(key))
  }

  #[cfg(feature = "std")]
  #[inline]
  fn snapshot_read<R>(&self, f: impl FnOnce(&Radix<C, V, P>) -> R) -> R {
    f(&self.inner.current.load())
  }

  #[cfg(all(not(feature = "std"), feature = "alloc"))]
  #[inline]
  fn snapshot_read<R>(&self, f: impl FnOnce(&Radix<C, V, P>) -> R) -> R {
    f(&self.inner.current.read())
  }
}

// ----- writes -------------------------------------------------------------

impl<C, V, P> ConcurrentRadix<C, V, P>
where
  C: ?Sized + ToOwned + Ord,
  C::Owned: Clone,
  V: Clone,
  P: SharedPointerKind,
{
  /// Applies a transaction and publishes the result as one atomic root swap.
  ///
  /// The closure receives a [`Txn`](crate::Txn) over a private working copy;
  /// concurrent writers are serialized so each observes the others' committed
  /// state. The closure's return value is forwarded to the caller.
  #[cfg(feature = "std")]
  pub fn write<R>(&self, f: impl FnOnce(&mut crate::Txn<'_, C, V, P>) -> R) -> R {
    let _guard = self
      .inner
      .write_lock
      .lock()
      .unwrap_or_else(|e| e.into_inner());
    // Clone the current trie (O(1)); the working copy diverges via copy-on-write.
    let mut working = Radix::clone(&self.inner.current.load());
    let result = {
      let mut txn = working.txn();
      f(&mut txn)
    };
    self.inner.current.store(std::sync::Arc::new(working));
    result
  }

  /// Applies a transaction under the holder's write lock (no_std backend).
  #[cfg(all(not(feature = "std"), feature = "alloc"))]
  pub fn write<R>(&self, f: impl FnOnce(&mut crate::Txn<'_, C, V, P>) -> R) -> R {
    let mut guard = self.inner.current.write();
    let mut txn = guard.txn();
    f(&mut txn)
  }
}

#[cfg(test)]
mod tests;

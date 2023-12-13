#![doc = include_str!("../README.md")]
#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(docsrs, allow(unused_attributes))]
#![deny(missing_docs)]

#[cfg(not(feature = "std"))]
extern crate alloc;

use bytes::Bytes;
use node::Inner;

mod iter;
pub use iter::*;
mod node;
mod txn;
pub use txn::*;
mod util;

#[cfg(test)]
mod tests;

mod maybestd {
  #[cfg(feature = "std")]
  pub(crate) use std::{boxed, collections::BTreeMap, sync, vec};

  #[cfg(not(feature = "std"))]
  pub(crate) use alloc::{boxed, collections::BTreeMap, sync, vec};
}

pub use sync::*;

mod sync {
  #[cfg(not(loom))]
  pub use super::maybestd::sync::Arc;
  #[cfg(not(loom))]
  pub use core::sync::atomic::*;

  #[cfg(loom)]
  pub use loom::sync::{atomic::*, Arc};

  #[cfg(loom)]
  pub(crate) trait AtomicMut<T> {}

  #[cfg(not(loom))]
  pub(crate) trait AtomicMut<T> {
    fn with_mut<F, R>(&mut self, f: F) -> R
    where
      F: FnOnce(&mut *mut T) -> R;
  }

  #[cfg(not(loom))]
  impl<T> AtomicMut<T> for AtomicPtr<T> {
    fn with_mut<F, R>(&mut self, f: F) -> R
    where
      F: FnOnce(&mut *mut T) -> R,
    {
      f(self.get_mut())
    }
  }
}

/// The default size of the modified node
/// cache used per transaction. This is used to cache the updates
/// to the nodes near the root, while the leaves do not need to be
/// cached. This is important for very large transactions to prevent
/// the modified cache from growing to be enormous. This is also used
/// to set the max size of the mutation notify maps since those should
/// also be bounded in a similar way.
const DEFAULT_MODIFIED_CACHE: usize = 8192;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum Kind {
  Vec,
  BTree,
}

/// Tree implements an immutable radix tree. This can be treated as a
/// Dictionary abstract data type. The main advantage over a standard
/// hash map is prefix-based lookups and ordered iteration. The immutability
/// means that it is safe to concurrently read from a Tree without any
/// coordination.
pub struct Tree<V> {
  root: Node<V>,
  size: usize,
  kind: Kind,
}

impl<V> Tree<V> {
  /// Returns a `Vec`-backed empty tree.
  #[inline]
  pub fn vec() -> Self {
    Self {
      root: Node::from(Inner::vec()),
      size: 0,
      kind: Kind::Vec,
    }
  }

  /// Returns a `BTreeMap`-backed empty tree.
  #[inline]
  pub fn btree() -> Self {
    Self {
      root: Node::from(Inner::btree()),
      size: 0,
      kind: Kind::BTree,
    }
  }

  /// Returns the number of elements in the tree.
  #[inline]
  pub const fn len(&self) -> usize {
    self.size
  }

  /// Returns true if the tree contains no elements.
  #[inline]
  pub const fn is_empty(&self) -> bool {
    self.size == 0
  }

  /// Starts a new transaction that can be used to mutate the tree
  #[inline]
  pub fn txn(&self) -> Txn<V> {
    Txn {
      root: self.root.clone(),
      snap: self.root.clone(),
      size: self.size,
      cache: None,
      kind: self.kind,
    }
  }

  /// Starts a new transaction with custom hasher that can be used to mutate the tree
  #[inline]
  pub fn txn_with_hasher<S: core::hash::Hash>(self, hasher: S) -> Txn<V, S> {
    Txn {
      root: self.root.clone(),
      snap: self.root.clone(),
      size: self.size,
      cache: None,
      kind: self.kind,
    }
  }

  /// Used to lookup a specific key, returning
  /// the value and if it was found
  pub fn get(&self, k: impl AsRef<[u8]>) -> Option<&V> {
    self.root.get(k.as_ref())
  }

  /// Adds or updates a given key. The return provides
  /// the new tree, and previous value if any was set.
  pub fn insert(&self, k: Bytes, v: V) -> (Self, Option<Value<V>>) {
    let mut txn = self.txn();
    let prev = txn.insert(k, v);
    let tree = txn.commit();
    (tree, prev)
  }

  /// Removes a given key. Returns the new tree,
  /// old value if any.
  pub fn remove(&self, k: impl AsRef<[u8]>) -> (Self, Option<Value<V>>) {
    let mut txn = self.txn();
    let prev = txn.remove(k.as_ref());
    let tree = txn.commit();
    (tree, prev)
  }

  /// Used to remove all nodes starting with a given prefix.
  /// Returns the new tree if the prefix matched any nodes
  pub fn remove_prefix(&self, prefix: impl AsRef<[u8]>) -> Option<Self> {
    let mut txn = self.txn();
    let prev = txn.remove_prefix(prefix.as_ref());
    let tree = txn.commit();
    if prev {
      Some(tree)
    } else {
      None
    }
  }

  /// Returns the root node of the tree which can be used for richer
  /// query operations.
  pub fn root(&self) -> &Node<V> {
    &self.root
  }
}

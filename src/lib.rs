#![doc = include_str!("../README.md")]
#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(docsrs, allow(unused_attributes))]
#![allow(warnings)]
#![deny(missing_docs)]


use core::num::NonZeroUsize;

use bytes::Bytes;
use lru::LruCache;
use node::{Edge, Inner, LeafNode};
pub use node::{Node, Value};

#[cfg(not(feature = "std"))]
extern crate alloc;

mod node;

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

/// Tree implements an immutable radix tree. This can be treated as a
/// Dictionary abstract data type. The main advantage over a standard
/// hash map is prefix-based lookups and ordered iteration. The immutability
/// means that it is safe to concurrently read from a Tree without any
/// coordination.
pub struct Tree<V> {
  root: Node<V>,
  size: usize,
}

impl<V> Tree<V> {
  /// Returns an empty tree.
  #[inline]
  pub fn new() -> Self {
    Self {
      root: Node::dangling(),
      size: 0,
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
    }
  }
}

/// A transaction on the tree. This transaction is applied
/// atomically and returns a new tree when committed. A transaction
/// is not thread safe, and should not be used concurrently.
pub struct Txn<V, S = lru::DefaultHasher> {
  /// The modified root for the transaction.
  root: Node<V>,

  /// A snapshot of the root node for use if we have to run the
  /// slow notify algorithm.
  snap: Node<V>,

  /// Tracks the size of the tree as it is modified during the
  /// transaction.
  size: usize,

  cache: Option<LruCache<usize, Node<V>, S>>,
}

impl<V> Txn<V> {

  /// Finalize the transaction and return a new tree. If mutation
  /// tracking is turned on then notifications will also be issued.
  #[cfg(feature = "std")]
  pub fn commit_and_notify(self) -> Tree<V> {
    todo!()
  }

  /// Finalize the transaction and return a new tree, but
  /// does not issue any notifications until Notify is called.
  pub fn commit(self) -> Tree<V> {
    Tree {
      root: self.root,
      size: self.size,
    }
  }

  /// Used to lookup a specific key, returning
  /// the value and if it was found
  pub fn get(&self, k: &[u8]) -> Option<&V> {
    self.root.get(k)
  }

  /// Used to add or update a given key. The return provides
  /// the previous value if exist.
  pub fn insert(&mut self, k: Bytes, v: V) -> Option<Value<V>> {
    let mut root = self.root.clone();
    let (new_root, old_val) = self.insert_in(&mut root, k.clone(), k, v);
    if !new_root.is_null() {
      self.root = new_root;
    }
    if old_val.is_none() {
      self.size += 1;
    }
    old_val
  }
}

impl<V> Txn<V> { 
  /// Returns a node to be modified, if the current node has already been
  /// modified during the course of the transaction, it is used in-place. Set
  /// `for_leaf_update` to true if you are getting a write node to update the leaf,
  /// which will set leaf mutation tracking appropriately as well.
  fn write_node(&mut self, n: &mut Node<V>, for_leaf_update: bool) -> Node<V> {
    // Ensure the writable set exists.
    let cache = self.cache.get_or_insert(LruCache::new(
      NonZeroUsize::new(DEFAULT_MODIFIED_CACHE).unwrap(),
    ));

    // If this node has already been modified, we can continue to use it
    // during this transaction. We know that we don't need to track it for
    // a node update since the node is writable, but if this is for a leaf
    // update we track it, in case the initial write to this node didn't
    // update the leaf.
    if cache.contains(&n.ptr()) {
      #[cfg(feature = "std")]
      {
        // TODO:
        // if t.trackMutate && forLeafUpdate && n.leaf != nil {
        //   t.trackChannel(n.leaf.mutateCh)
        // }
      }

      return n.clone();
    }

    // Copy the existing node. If you have set forLeafUpdate it will be
    // safe to replace this leaf with another after you get your node for
    // writing. You MUST replace it, because the channel associated with
    // this leaf will be closed when this transaction is committed.
    let nref = n.as_ref();
    let nc = Node::new(nref.prefix.clone(), nref.edges.clone());

    // Mark this node as writable.
    cache.get_or_insert(nc.ptr(), || nc).clone()
  }

  /// Does a recursive insertion
  fn insert_in(
    &mut self,
    n: &mut Node<V>,
    key: Bytes,
    mut search: Bytes,
    val: V,
  ) -> (Node<V>, Option<Value<V>>) {
    // Handle key exhaustion
    if search.is_empty() {
      let mut old_val = None;
      let nr = n.as_ref();
      if let Some(leaf) = &nr.leaf {
        old_val = Some(leaf.val.clone());
      }

      let mut nc = self.write_node(n, true);
      nc.set_leaf(LeafNode {
        key,
        val: Value::new(val),
      });
      return (nc.clone(), old_val);
    }

    // Look for the edge
    match n.get_edge(search[0]).cloned() {
      None => {
        let e = Edge::new(
          search[0],
          Node::from(Inner::new(
            search,
            Some(LeafNode {
              key,
              val: Value::new(val),
            }),
            Default::default(),
          )),
        );
        let nc = self.write_node(n, false);
        nc.add_edge(e);
        (nc, None)
      }
      Some(mut child) => {
        // Determine longest prefix of the search key on match
        let child_ref = child.as_ref();
        let common_prefix = longest_prefix(&search, &child_ref.prefix);
        if common_prefix == child_ref.prefix.len() {
          search = search.slice(common_prefix..);
          let prefix = search[0];
          let (new_child, old_val) = self.insert_in(&mut child, key, search, val);

          if !new_child.is_null() {
            let nc = self.write_node(n, false);
            let nc_ref = nc.as_mut();
            nc_ref.edges.insert(prefix, new_child);
            return (nc, old_val);
          }
          
          return (Node::dangling(), old_val);
        }

        // Split the node
        let nc = self.write_node(n, false);
        let mut split_node = Node::from(Inner::new(Bytes::copy_from_slice(&search[..common_prefix]), None, Default::default()));
        nc.replace_edge(Edge::new(search[0], split_node.clone()));

        // Restore the existing child node
        let mod_child = self.write_node(&mut child, false);
        let mod_child_ref = mod_child.as_mut();
        split_node.add_edge(Edge::new(mod_child_ref.prefix[common_prefix], mod_child.clone()));
        mod_child_ref.prefix = mod_child_ref.prefix.slice(common_prefix..);

        // Create the new leaf node
        let new_leaf = LeafNode {
          key,
          val: Value::new(val),
        };

        // If the new key is a subset, add to this node
        search = search.slice(common_prefix..);
        if search.is_empty() {
          split_node.set_leaf(new_leaf);
          return (nc, None);
        }
        
        // Create a new edge for the node
        split_node.add_edge(Edge::new(search[0], Node::from(Inner::new(search, Some(new_leaf), Default::default()))));
        (nc, None)
      }
    }
  }
}

fn longest_prefix(k1: &[u8], k2: &[u8]) -> usize {
  let max = core::cmp::min(k1.len(), k2.len());
  let mut i = 0;
  while i < max && k1[i] == k2[i] {
      i += 1;
  }
  i
}

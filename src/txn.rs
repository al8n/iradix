use core::{num::NonZeroUsize, cell::RefCell};

use bytes::{Bytes, BytesMut};
use lru::LruCache;

pub use super::node::{Node, Value};
use super::{
  node::{Edge, Inner, LeafNode},
  Kind, Tree, DEFAULT_MODIFIED_CACHE,
};

/// A transaction on the tree. This transaction is applied
/// atomically and returns a new tree when committed. A transaction
/// is not thread safe, and should not be used concurrently.
pub struct Txn<V, S = lru::DefaultHasher> {
  /// The modified root for the transaction.
  pub(super) root: Node<V>,

  /// A snapshot of the root node for use if we have to run the
  /// slow notify algorithm.
  pub(super) snap: Node<V>,

  /// Tracks the size of the tree as it is modified during the
  /// transaction.
  pub(super) size: usize,

  /// The transaction kind
  pub(super) kind: Kind,

  pub(super) cache: Option<LruCache<usize, Node<V>, S>>,
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
      kind: self.kind,
    }
  }

  /// Used to lookup a specific key, returning
  /// the value and if it was found
  pub fn get(&self, k: impl AsRef<[u8]>) -> Option<&V> {
    self.root.get(k.as_ref())
  }

  /// Used to add or update a given key. The return provides
  /// the previous value if exist.
  pub fn insert(&mut self, k: Bytes, v: V) -> Option<Value<V>> {
    let root = self.root.clone();
    let (new_root, old_val) = self.insert_in(root, k.clone(), k, v);
    if let Some(new_root) = new_root {
      self.root = new_root;
    }
    if old_val.is_none() {
      self.size += 1;
    }
    old_val
  }

  /// Remove a given key. Returns the old value if any,
  /// and a bool indicating if the key was set.
  pub fn remove(&mut self, k: &[u8]) -> Option<Value<V>> {
    let root = self.root.clone();
    let (new_root, leaf) = self.remove_in(root, k);
    if let Some(new_root) = new_root {
      self.root = new_root;
    }
    leaf.map(|l| {
      self.size -= 1;
      l.val
    })
  }
  
  /// Used to delete an entire subtree that matches the prefix
  /// This will delete all nodes under that prefix
  pub fn remove_prefix(&mut self, prefix: impl AsRef<[u8]>) -> bool {
    let root = self.root.clone();
    let (new_root, num_deletions) = self.remove_prefix_in(&root, prefix.as_ref());
    if let Some(new_root) = new_root {
      self.root = new_root;
      self.size -= num_deletions;
      return true;
    }
    false
  }
}

impl<V> Txn<V> {
  /// Returns a node to be modified, if the current node has already been
  /// modified during the course of the transaction, it is used in-place. Set
  /// `for_leaf_update` to true if you are getting a write node to update the leaf,
  /// which will set leaf mutation tracking appropriately as well.
  fn write_node(&mut self, n: &Node<V>, for_leaf_update: bool) -> Node<V> {
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
    let nc = Node::from(nref.clone_self());

    // Mark this node as writable.
    cache.get_or_insert(nc.ptr(), || nc).clone()
  }

  /// Does a recursive insertion
  fn insert_in(
    &mut self,
    n: Node<V>,
    key: Bytes,
    mut search: Bytes,
    val: V,
  ) -> (Option<Node<V>>, Option<Value<V>>) {
    // Handle key exhaustion
    if search.is_empty() {
      let mut old_val = None;
      let nr = n.as_ref();
      if let Some(leaf) = nr.leaf() {
        old_val = Some(leaf.val.clone());
      }

      let mut nc = self.write_node(&n, true);
      nc.set_leaf(LeafNode {
        key,
        val: Value::new(val),
      });
      return (Some(nc.clone()), old_val);
    }

    // Look for the edge
    match n.get_edge(search[0]) {
      None => {
        let e = Edge::new(
          search[0],
          Node::from(Inner::new_with_empty_edges(
            search,
            Some(LeafNode {
              key,
              val: Value::new(val),
            }),
            self.kind,
          )),
        );
        let nc = self.write_node(&n, false);
        nc.add_edge(e);
        (Some(nc), None)
      }
      Some((idx, child)) => {
        // Determine longest prefix of the search key on match
        let child_ref = child.as_ref();
        let child_prefix = child_ref.prefix();
        let common_prefix = longest_prefix(&search, child_prefix);

        if common_prefix == child_prefix.len() {
          search = search.slice(common_prefix..);
          let prefix = search[0];
          let (new_child, old_val) = self.insert_in(child, key, search, val);

          if let Some(new_child) = new_child {
            let nc = self.write_node(&n, false);
            let nc_ref = nc.as_mut();
            nc_ref.update_edge(idx, new_child);
            return (Some(nc), old_val);
          }

          return (None, old_val);
        }

        // Split the node
        let nc = self.write_node(&n, false);
        let mut split_node = Node::from(Inner::new_with_empty_edges(
          Bytes::copy_from_slice(&search[..common_prefix]),
          None,
          self.kind,
        ));
        nc.replace_edge(Edge::new(search[0], split_node.clone()));
        // Restore the existing child node
        let mod_child = self.write_node(&child, false);
        let mod_child_ref = mod_child.as_mut();
        let mod_child_prefix = mod_child_ref.prefix();
        split_node.add_edge(Edge::new(
          mod_child_prefix[common_prefix],
          mod_child.clone(),
        ));
        mod_child_ref.set_prefix(mod_child_prefix.slice(common_prefix..));

        // Create the new leaf node
        let new_leaf = LeafNode {
          key,
          val: Value::new(val),
        };

        // If the new key is a subset, add to this node
        search = search.slice(common_prefix..);
        if search.is_empty() {
          split_node.set_leaf(new_leaf);
          return (Some(nc), None);
        }

        // Create a new edge for the node
        split_node.add_edge(Edge::new(
          search[0],
          Node::from(Inner::new_with_empty_edges(
            search,
            Some(new_leaf),
            self.kind,
          )),
        ));
        (Some(nc), None)
      }
    }
  }

  /// Does a recursive deletion
  fn remove_in(&mut self, n: Node<V>, mut search: &[u8]) -> (Option<Node<V>>, Option<LeafNode<V>>) {
    // Check for key exhaustion
    if search.is_empty() {
      let nr = n.as_ref();
      match nr.leaf() {
        None => {
          return (None, None);
        }
        Some(leaf) => {
          // Copy the pointer in case we are in a transaction that already
          // modified this node since the node will be reused. Any changes
          // made to the node will not affect returning the original leaf
          // value.
          let mut old_leaf = leaf.clone();

          // Remove the leaf node
          let mut nc = self.write_node(&n, true);
          nc.clear_leaf();

          // Check if this node should be merged
          let nc_ref = nc.as_mut();
          let nc_edges = nc_ref.num_edges();
          if n.ptr() != self.root.ptr() && nc_edges == 1 {
            self.merge_child(nc_ref);
          }
          return (Some(nc), Some(old_leaf.clone()));
        }
      }
    }

    // Look for an edge
    let label = search[0];
    match n.get_edge(label) {
      None => (None, None),
      Some((ek, child)) => {
        let child_ref = child.as_ref();
        let child_prefix = child_ref.prefix();
        if !search.starts_with(child_prefix) {
          return (None, None);
        }

        // Consume the search prefix
        search = &search[child_prefix.len()..];
        let (new_child, leaf) = self.remove_in(child, search);
        match new_child {
          None => return (None, None),
          Some(new_child) => {
            // Copy this node. WATCH OUT - it's safe to pass "false" here because we
            // will only ADD a leaf via nc.merge_child() if there isn't one due to
            // the !nc.is_leaf() check in the logic just below. This is pretty subtle,
            // so be careful if you change any of the logic here.
            let nc = self.write_node(&n, false);
            let nc_ref = nc.as_mut();
            let new_child_ref = new_child.as_ref();
            let new_child_edges = new_child_ref.num_edges();
            // Delete the edge if the node has no edges
            if new_child_ref.leaf().is_none() && new_child_edges == 0 {
              nc.remove_edge(label);
              if n.ptr() != self.root.ptr() && nc_ref.num_edges() == 1 && !nc.is_leaf() {
                self.merge_child(nc_ref);
              }
            } else {
              nc_ref.update_edge(ek, new_child);
            }
            (Some(nc), leaf)
          }
        }
      }
    }
  }

  /// Does a recursive deletion
  fn remove_prefix_in(&mut self, n: &Node<V>, mut search: &[u8]) -> (Option<Node<V>>, usize) {
    // Check for key exhaustion
    if search.is_empty() {
      let mut nc = self.write_node(&n, true);
      if n.is_leaf() {
        nc.clear_leaf();
      }
      nc.clear_edges();
      return (Some(nc), self.track_channels_and_count(n));
    }

    // Look for an edge
    let label = search[0];
    // We make sure that either the child node's prefix starts with the search term, or the search term starts with the child node's prefix
	  // Need to do both so that we can delete prefixes that don't correspond to any node in the tree
    match n.get_edge(label) {
      None => (None, 0),
      Some((idx, child)) => {
        let child_ref = child.as_ref();
        let child_prefix = child_ref.prefix();
        if !child_prefix.starts_with(search) && !search.starts_with(child_prefix) {
          return (None, 0);
        }
        
        // Consume the search prefix
        if child_prefix.len() > search.len() {
          search = &[];
        } else {
          search = &search[child_prefix.len()..];
        }

        let (new_child, num_deletions) = self.remove_prefix_in(&child, search);
        match new_child {
          None => (None, 0),
          Some(new_child) => {
            let new_child_ref = new_child.as_mut();
            // Copy this node. WATCH OUT - it's safe to pass "false" here because we
            // will only ADD a leaf via nc.mergeChild() if there isn't one due to
            // the !nc.isLeaf() check in the logic just below. This is pretty subtle,
            // so be careful if you change any of the logic here.
            let nc = self.write_node(n, false);

            // Delete the edge if the node has no edges
            if new_child_ref.leaf().is_none() && new_child_ref.num_edges() == 0 {
              nc.remove_edge(label);
              let nc_ref = nc.as_mut();
              if n.ptr() != self.root.ptr() && nc_ref.num_edges() == 1 && !nc.is_leaf() {
                self.merge_child(nc_ref);
              }
            } else {
              nc.as_mut().update_edge(idx, new_child);
            }

            (Some(nc), num_deletions)
          },
        }
      }
    }
  }

  /// Called to collapse the given node with its child. This is only
  /// called when the given node is not a leaf and has a single edge.
  fn merge_child(&mut self, n: &mut Inner<V>) {
    n.merge_child();
  }

  fn track_channels_and_count(&self, n: &Node<V>) -> usize {
    // Count only leaf nodes
    let mut leaves = if !n.is_leaf() {
      RefCell::new(1)
    } else {
      RefCell::new(0)
    };
    

    #[cfg(feature = "track")]
    {
    
    }

    // Recurse on the children
    n.for_each_edge(|n| {
      *leaves.borrow_mut() += self.track_channels_and_count(n);
    });

    leaves.into_inner()
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

pub(crate) fn concat(a: &[u8], b: &[u8]) -> Bytes {
  let mut v = BytesMut::with_capacity(a.len() + b.len());
  v.extend_from_slice(a);
  v.extend_from_slice(b);
  v.freeze()
}

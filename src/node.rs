use core::{cell::UnsafeCell, fmt::Debug};

use bytes::Bytes;

use super::{
  maybestd::{boxed::Box, vec::Vec, BTreeMap},
  sync::{Arc, AtomicUsize, Ordering},
};

/// Value
#[derive(Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
#[repr(transparent)]
pub struct Value<T>(Arc<T>);

impl<T> Clone for Value<T> {
  fn clone(&self) -> Self {
    Self(self.0.clone())
  }
}

impl<T> core::ops::Deref for Value<T> {
  type Target = T;

  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

impl<T> AsRef<T> for Value<T> {
  fn as_ref(&self) -> &T {
    &self.0
  }
}

impl<T> Value<T> {
  pub(super) fn new(val: T) -> Self {
    Self(Arc::new(val))
  }
}

/// Used to represent a value
#[derive(Debug)]
pub(super) struct LeafNode<T> {
  pub(super) key: Bytes,
  pub(super) val: Value<T>,
}

impl<T> Clone for LeafNode<T> {
  fn clone(&self) -> Self {
    Self {
      key: self.key.clone(),
      val: self.val.clone(),
    }
  }
}

#[derive(Debug)]
pub(super) struct Edge<T> {
  pub(super) label: u8,
  pub(super) node: Node<T>,
}

impl<T> Edge<T> {
  #[inline]
  pub(super) const fn new(label: u8, node: Node<T>) -> Self {
    Self { label, node }
  }
}

impl<T> Clone for Edge<T> {
  fn clone(&self) -> Self {
    Self {
      label: self.label,
      node: self.node.clone(),
    }
  }
}

pub(super) enum Edges<T> {
  Vec(Vec<Edge<T>>),
  BTreeMap(BTreeMap<u8, Node<T>>),
}

pub(super) struct Inner<T> {
  /// Used to store possible leaf
  pub(super) leaf: Option<LeafNode<T>>,

  /// The common prefix we ignore
  pub(super) prefix: Bytes,

  /// Should be stored in-order for iteration.
  /// We avoid a fully materialized slice to save memory,
  /// since in most cases we expect to be sparse
  pub(super) edges: Vec<Edge<T>>,
}

impl<T> Inner<T> {
  #[inline]
  pub(super) fn new(prefix: Bytes, leaf: Option<LeafNode<T>>, edges: Vec<Edge<T>>) -> Self {
    Self {
      leaf,
      prefix,
      edges,
    }
  }
}

/// An immutable node in the radix tree
pub struct Node<T> {
  ptr: Arc<UnsafeCell<Inner<T>>>,
}

// Safety: node will never be mutated
unsafe impl<T> Send for Node<T> {}
// Safety: node will never be mutated
unsafe impl<T> Sync for Node<T> {}

impl<T: Debug> Debug for Node<T> {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    f.debug_struct("Node")
      .field("leaf", &self.as_ref().leaf)
      .field("prefix", &self.as_ref().prefix)
      .field("edges", &self.as_ref().edges)
      .finish()
  }
}

impl<T> Clone for Node<T> {
  fn clone(&self) -> Self {
    Self {
      ptr: self.ptr.clone(),
    }
  }
}

impl<T> Node<T> {
  /// Return the minimum value in the tree
  pub fn minimum(&self) -> Option<(&[u8], &T)> {
    let mut current = self;
    loop {
      let n = current.as_ref();
      if let Some(leaf) = &n.leaf {
        return Some((&leaf.key, &leaf.val));
      }
      if !n.edges.is_empty() {
        current = &n.edges[0].node;
      } else {
        break;
      }
    }
    None
  }

  /// Return the maximum value in the tree
  pub fn maximum(&self) -> Option<(&[u8], &T)> {
    let mut current = self;
    loop {
      let n = current.as_ref();
      let num = n.edges.len();
      if num > 0 {
        current = &n.edges[num - 1].node;
        continue;
      }

      // If the current node is a leaf, return its key and value
      if let Some(leaf) = &n.leaf {
        return Some((&leaf.key, &leaf.val));
      } else {
        break;
      }
    }

    None
  }

  /// Returns the value associated with the given key, if it exists.
  pub fn get(&self, key: &[u8]) -> Option<&T> {
    let mut current = self;
    let mut search = key;

    loop {
      let n = current.as_ref();

      // Check if the current node is a leaf and the search key is exhausted
      if search.is_empty() {
        if let Some(leaf) = &n.leaf {
          return Some(&leaf.val);
        }
        break;
      }

      // Try to find the edge corresponding to the next byte in the search key
      match current.get_edge_ref(search[0]) {
        Some((_, node)) => {
          let nref = node.as_ref();
          // Check if the search key starts with the node's prefix
          if search.starts_with(&nref.prefix) {
            search = &search[nref.prefix.len()..];
            current = node;
          } else {
            // Prefix mismatch; stop searching
            break;
          }
        }
        None => break, // Edge not found; stop searching
      }
    }

    None
  }

  /// Like [`get`], but instead of an
  /// exact match, it will return the longest prefix match.
  ///
  /// [`get`]: crate::node::Node#get
  pub fn longest_prefix(&self, key: &[u8]) -> Option<(&[u8], &T)> {
    let mut current = self;
    let mut last_leaf: Option<(&[u8], &T)> = None;

    let mut search = key;
    loop {
      let n = current.as_ref();
      // Update last_leaf if current node is a leaf
      if let Some(leaf) = &n.leaf {
        last_leaf = Some((&leaf.key, &leaf.val));
      }

      // Check if the search key is exhausted
      if search.is_empty() {
        break;
      }

      // Try to find the edge corresponding to the next byte in the search key
      match current.get_edge_ref(search[0]) {
        Some((_, node)) => {
          let nref = node.as_ref();
          // If the current node's prefix matches the search key,
          // continue searching deeper in the tree
          if search.starts_with(&nref.prefix) {
            search = &search[nref.prefix.len()..];
            current = node;
          } else {
            // Prefix mismatch; stop searching
            break;
          }
        }
        None => break, // Edge not found; stop searching
      }
    }

    last_leaf
  }
}

impl<T> From<Inner<T>> for Node<T> {
  fn from(inner: Inner<T>) -> Self {
    Self {
      ptr: Arc::new(UnsafeCell::new(inner)),
    }
  }
}

impl<T> Node<T> {
  pub(super) fn ptr(&self) -> usize {
    self.ptr.get() as usize
  }

  #[inline]
  pub(super) fn as_ref(&self) -> &Inner<T> {
    unsafe { &*self.ptr.get() }
  }

  #[allow(clippy::mut_from_ref)]
  #[inline]
  pub(super) fn as_mut(&self) -> &mut Inner<T> {
    unsafe { &mut *self.ptr.get() }
  }

  #[inline]
  pub(super) fn dangling() -> Self {
    Self {
      ptr: Arc::new(UnsafeCell::new(Inner {
        leaf: None,
        prefix: Bytes::new(),
        edges: Default::default(),
      })),
    }
  }

  pub(super) fn new(prefix: Bytes, edges: Vec<Edge<T>>) -> Self {
    Self {
      ptr: Arc::new(UnsafeCell::new(Inner {
        leaf: None,
        prefix,
        edges,
      })),
    }
  }

  pub(super) fn set_leaf(&mut self, leaf: LeafNode<T>) {
    self.as_mut().leaf = Some(leaf);
  }

  pub(super) fn clear_leaf(&mut self) {
    self.as_mut().leaf = None;
  }

  #[inline]
  pub(super) fn is_leaf(&self) -> bool {
    self.as_ref().leaf.is_some()
  }

  pub(super) fn add_edge(&self, e: Edge<T>) {
    let this = self.as_mut();
    let num = this.edges.len();
    let idx = indexsort::search(num, |i| this.edges[i].label >= e.label);

    if idx != num {
      this.edges.insert(idx, e);
    } else {
      this.edges.push(e);
    }
  }

  pub(super) fn replace_edge(&self, e: Edge<T>) {
    let this = self.as_mut();
    let num = this.edges.len();
    let idx = indexsort::search(num, |i| this.edges[i].label >= e.label);
    if idx < num && this.edges[idx].label == e.label {
      this.edges[idx].node = e.node;
    } else {
      panic!("replacing missing edge");
    }
  }

  pub(super) fn get_edge(&self, label: u8) -> Option<(usize, Node<T>)> {
    self.get_edge_ref(label).map(|(idx, n)| (idx, n.clone()))
  }

  pub(super) fn get_edge_ref(&self, label: u8) -> Option<(usize, &Node<T>)> {
    let this = self.as_mut();
    let num = this.edges.len();
    let idx = indexsort::search(num, |i| this.edges[i].label >= label);
    if idx < num && this.edges[idx].label == label {
      Some((idx, &this.edges[idx].node))
    } else {
      None
    }
  }

  pub(super) fn get_lower_bound_edge(&self, label: u8) -> Option<Node<T>> {
    let this = self.as_mut();
    let num = this.edges.len();
    let idx = indexsort::search(num, |i| this.edges[i].label >= label);
    if idx < num {
      Some(this.edges[idx].node.clone())
    } else {
      None
    }
  }

  pub(super) fn remove_edge(&self, label: u8) -> Option<Node<T>> {
    let this = self.as_mut();
    let num = this.edges.len();
    let idx = indexsort::search(num, |i| this.edges[i].label >= label);
    if idx < num && this.edges[idx].label == label {
      Some(this.edges.remove(idx).node)
    } else {
      None
    }
  }
}

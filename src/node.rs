use core::{cell::UnsafeCell, fmt::Debug};

use bytes::Bytes;

use crate::Kind;

use self::{btree::BTreeInner, vec::VecInner};

use super::{
  maybestd::{vec::Vec, BTreeMap},
  sync::Arc,
};

mod btree;
mod vec;

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

pub(super) trait NodeInner<T> {
  type Key;

  fn is_leaf(&self) -> bool;
  fn set_leaf(&mut self, leaf: LeafNode<T>);
  fn clear_leaf(&mut self);
  fn leaf(&self) -> Option<&LeafNode<T>>;

  fn prefix(&self) -> &Bytes;
  fn set_prefix(&mut self, prefix: Bytes);

  fn num_edges(&self) -> usize;
  fn update_edge(&mut self, idx: Self::Key, node: Node<T>);
  fn clear_edges(&mut self);
  fn add_edge(&mut self, e: Edge<T>);
  fn replace_edge(&mut self, e: Edge<T>);
  fn get_edge(&self, label: u8) -> Option<(Self::Key, Node<T>)>;
  fn get_edge_ref(&self, label: u8) -> Option<(Self::Key, &Node<T>)>;
  fn get_lower_bound_edge(&self, label: u8) -> Option<Node<T>>;
  fn remove_edge(&mut self, label: u8) -> Option<Node<T>>;
}

#[non_exhaustive]
pub(super) enum Inner<T> {
  Vec(VecInner<T>),
  BTree(BTreeInner<T>),
}

impl<T> Inner<T> {
  fn as_vec(&self) -> &VecInner<T> {
    match self {
      Self::Vec(v) => v,
      _ => panic!("Inner::as_vec called on non-Vec"),
    }
  }

  fn as_btree(&self) -> &BTreeInner<T> {
    match self {
      Self::BTree(v) => v,
      _ => panic!("Inner::as_btree called on non-BTree"),
    }
  }

  #[cfg(test)]
  fn as_vec_mut(&mut self) -> &mut VecInner<T> {
    match self {
      Self::Vec(v) => v,
      _ => panic!("Inner::as_vec called on non-Vec"),
    }
  }

  #[cfg(test)]
  fn as_btree_mut(&mut self) -> &mut BTreeInner<T> {
    match self {
      Self::BTree(v) => v,
      _ => panic!("Inner::as_btree called on non-BTree"),
    }
  }

  pub(super) fn merge_child(&mut self) {
    match self {
      Self::Vec(v) => v.merge_child(),
      Self::BTree(v) => v.merge_child(),
    }
  }

  pub(super) fn new_with_empty_edges(prefix: Bytes, leaf: Option<LeafNode<T>>, kind: Kind) -> Self {
    match kind {
      Kind::Vec => Self::Vec(VecInner::new(prefix, leaf, Default::default())),
      Kind::BTree => Self::BTree(BTreeInner::new(prefix, leaf, Default::default())),
    }
  }

  pub(super) fn update_edge(&mut self, idx: InnerEdgeKey, node: Node<T>) {
    match self {
      Self::Vec(v) => v.update_edge(idx.unwrap_vec(), node),
      Self::BTree(v) => v.update_edge(idx.unwrap_btree(), node),
    }
  }

  pub(super) fn vec() -> Self {
    Self::Vec(VecInner::default())
  }

  pub(super) fn btree() -> Self {
    Self::BTree(BTreeInner::default())
  }

  pub(super) fn clone_self(&self) -> Self {
    match self {
      Self::Vec(v) => Self::Vec(VecInner::new(
        v.prefix.clone(),
        v.leaf.clone(),
        v.edges.to_borrowed(),
      )),
      Self::BTree(v) => Self::BTree(BTreeInner::new(
        v.prefix.clone(),
        v.leaf.clone(),
        v.edges.to_borrowed(),
      )),
    }
  }

  fn add_edge(&mut self, e: Edge<T>) {
    match self {
      Self::Vec(v) => v.add_edge(e),
      Self::BTree(v) => v.add_edge(e),
    }
  }

  fn replace_edge(&mut self, e: Edge<T>) {
    match self {
      Self::Vec(v) => v.replace_edge(e),
      Self::BTree(v) => v.replace_edge(e),
    }
  }

  fn get_edge(&self, label: u8) -> Option<(InnerEdgeKey, Node<T>)> {
    match self {
      Self::Vec(v) => v
        .get_edge(label)
        .map(|(idx, node)| (InnerEdgeKey::Vec(idx), node)),
      Self::BTree(v) => v
        .get_edge(label)
        .map(|(idx, node)| (InnerEdgeKey::BTree(idx), node)),
    }
  }

  fn get_edge_ref(&self, label: u8) -> Option<(InnerEdgeKey, &Node<T>)> {
    match self {
      Self::Vec(v) => v
        .get_edge_ref(label)
        .map(|(idx, node)| (InnerEdgeKey::Vec(idx), node)),
      Self::BTree(v) => v
        .get_edge_ref(label)
        .map(|(idx, node)| (InnerEdgeKey::BTree(idx), node)),
    }
  }

  fn get_lower_bound_edge(&self, label: u8) -> Option<Node<T>> {
    match self {
      Self::Vec(v) => v.get_lower_bound_edge(label),
      Self::BTree(v) => v.get_lower_bound_edge(label),
    }
  }

  fn remove_edge(&mut self, label: u8) -> Option<Node<T>> {
    match self {
      Self::Vec(v) => v.remove_edge(label),
      Self::BTree(v) => v.remove_edge(label),
    }
  }

  pub(super) fn is_leaf(&self) -> bool {
    match self {
      Self::Vec(v) => v.is_leaf(),
      Self::BTree(v) => v.is_leaf(),
    }
  }

  fn set_leaf(&mut self, leaf: LeafNode<T>) {
    match self {
      Self::Vec(v) => v.set_leaf(leaf),
      Self::BTree(v) => v.set_leaf(leaf),
    }
  }

  fn clear_leaf(&mut self) {
    match self {
      Self::Vec(v) => v.clear_leaf(),
      Self::BTree(v) => v.clear_leaf(),
    }
  }

  #[inline]
  pub(super) fn leaf(&self) -> Option<&LeafNode<T>> {
    match self {
      Self::Vec(v) => v.leaf(),
      Self::BTree(v) => v.leaf(),
    }
  }

  #[inline]
  pub(super) fn prefix(&self) -> &Bytes {
    match self {
      Self::Vec(v) => v.prefix(),
      Self::BTree(v) => v.prefix(),
    }
  }

  #[inline]
  pub(super) fn set_prefix(&mut self, prefix: Bytes) {
    match self {
      Self::Vec(v) => v.set_prefix(prefix),
      Self::BTree(v) => v.set_prefix(prefix),
    }
  }

  #[inline]
  pub(super) fn num_edges(&self) -> usize {
    match self {
      Self::Vec(v) => v.num_edges(),
      Self::BTree(v) => v.num_edges(),
    }
  }

  fn minimum(&self) -> Option<(&[u8], &T)> {
    match self {
      Self::Vec(v) => {
        let mut current = v;
        loop {
          if let Some(leaf) = current.leaf() {
            return Some((&leaf.key, &leaf.val));
          }
          let num_edges = current.num_edges();
          if num_edges > 0 {
            current = current.edges[0].node.as_ref().as_vec();
          } else {
            break;
          }
        }
        None
      }
      Self::BTree(v) => {
        let mut current = v;
        loop {
          if let Some(leaf) = current.leaf() {
            return Some((&leaf.key, &leaf.val));
          }
          if let Some((_, node)) = current.edges.iter().next() {
            current = node.as_ref().as_btree();
          } else {
            break;
          }
        }
        None
      }
    }
  }

  fn maximum(&self) -> Option<(&[u8], &T)> {
    match self {
      Self::Vec(v) => {
        let mut current = v;
        loop {
          let num = current.num_edges();
          if num > 0 {
            current = current.edges[num - 1].node.as_ref().as_vec();
            continue;
          }

          // If the current node is a leaf, return its key and value
          if let Some(leaf) = &current.leaf {
            return Some((&leaf.key, &leaf.val));
          } else {
            break;
          }
        }

        None
      }
      Self::BTree(v) => {
        let mut current = v;
        loop {
          // If the current node is a leaf, return its key and value
          if let Some(leaf) = current.leaf() {
            return Some((&leaf.key, &leaf.val));
          }

          // Otherwise, go to the right-most (maximum) edge
          if let Some((_, node)) = current.edges.iter().next_back() {
            current = node.as_ref().as_btree();
          } else {
            // No edges to follow, exit the loop
            break;
          }
        }

        None
      }
    }
  }
}

pub(super) enum InnerEdgeKey {
  Vec(usize),
  BTree(u8),
}

impl InnerEdgeKey {
  fn unwrap_vec(self) -> usize {
    match self {
      Self::Vec(idx) => idx,
      _ => panic!("InnerEdgeKey::unwrap_vec called on non-Vec"),
    }
  }

  fn unwrap_btree(self) -> u8 {
    match self {
      Self::BTree(idx) => idx,
      _ => panic!("InnerEdgeKey::unwrap_btree called on non-BTree"),
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
      .field("leaf", &self.as_ref().leaf())
      .field("prefix", &self.as_ref().prefix())
      .field("edges", &self.as_ref().num_edges())
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
    self.as_ref().minimum()
  }

  /// Return the maximum value in the tree
  pub fn maximum(&self) -> Option<(&[u8], &T)> {
    self.as_ref().maximum()
  }

  /// Returns the value associated with the given key, if it exists.
  pub fn get(&self, key: &[u8]) -> Option<&T> {
    let mut current = self;
    let mut search = key;

    loop {
      let n = current.as_ref();

      // Check if the current node is a leaf and the search key is exhausted
      if search.is_empty() {
        if let Some(leaf) = n.leaf() {
          return Some(&leaf.val);
        }
        break;
      }

      // Try to find the edge corresponding to the next byte in the search key
      match current.get_edge_ref(search[0]) {
        Some((_, node)) => {
          let nref = node.as_ref();
          let prefix = nref.prefix();
          // Check if the search key starts with the node's prefix
          if search.starts_with(prefix) {
            search = &search[prefix.len()..];
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
      if let Some(leaf) = n.leaf() {
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
          let prefix = nref.prefix();
          // If the current node's prefix matches the search key,
          // continue searching deeper in the tree
          if search.starts_with(prefix) {
            search = &search[prefix.len()..];
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

  /// Used to walk the tree, but only visiting nodes
  /// from the root down to a given leaf. Where WalkPrefix walks
  /// all the entries *under* the given prefix, this walks the
  /// entries *above* the given prefix.
  pub fn walk<F>(&self, mut f: F)
  where
    F: FnMut(&[u8], &T) -> bool,
  {
    self.recursive_walk(&mut f);
  }

  /// Used to walk the tree under a prefix
  pub fn walk_prefix<F>(&self, prefix: impl AsRef<[u8]>, mut f: F)
  where
    F: FnMut(&[u8], &T) -> bool,
  {
    let mut search = prefix.as_ref();
    let mut current = self;
    loop {
      // Check for key exhaustion
      if search.is_empty() {
        current.recursive_walk(&mut f);
        return;
      }

      // Look for an edge
      match current.get_edge_ref(search[0]) {
        Some((_, node)) => {
          // Consume the search prefix
          current = node;
          let current_ref = current.as_ref();
          let current_prefix = current_ref.prefix();
          if search.starts_with(current_prefix) {
            search = &search[current_prefix.len()..];
          } else if current_prefix.starts_with(search) {
            // Child may be under our search prefix
            current.recursive_walk(&mut f);
            return;
          } else {
            return;
          }
        }
        None => return, // Edge not found; stop searching
      }
    }
  }
}

impl<T> From<VecInner<T>> for Node<T> {
  fn from(inner: VecInner<T>) -> Self {
    Self {
      ptr: Arc::new(UnsafeCell::new(Inner::Vec(inner))),
    }
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

  fn recursive_walk<F>(&self, f: &mut F) -> bool
  where
    F: FnMut(&[u8], &T) -> bool,
  {
    // Visit the leaf values if any
    if let Some(leaf) = self.as_ref().leaf() {
      return f(&leaf.key, &leaf.val);
    }

    // Recurse on the children
    match self.as_ref() {
      Inner::Vec(v) => {
        for e in v.edges.iter() {
          if e.node.recursive_walk(f) {
            return true;
          }
        }
      }
      Inner::BTree(t) => {
        for (_, n) in t.edges.iter() {
          if n.recursive_walk(f) {
            return true;
          }
        }
      }
    }
    false
  }

  pub(super) fn for_each_edge<F>(&self, f: F)
  where
    F: Fn(&Node<T>),
  {
    match self.as_ref() {
      Inner::Vec(v) => {
        v.edges.iter().for_each(|e| {
          f(&e.node);
        });
      }
      Inner::BTree(v) => {
        v.edges.iter().for_each(|(_, n)| {
          f(n);
        });
      }
    }
  }

  pub(super) fn clear_edges(&self) {
    match self.as_mut() {
      Inner::Vec(v) => v.clear_edges(),
      Inner::BTree(v) => v.clear_edges(),
    }
  }

  pub(super) fn set_leaf(&mut self, leaf: LeafNode<T>) {
    self.as_mut().set_leaf(leaf);
  }

  pub(super) fn leaf(&self) -> Option<&LeafNode<T>> {
    self.as_ref().leaf()
  }

  pub(super) fn clear_leaf(&mut self) {
    self.as_mut().clear_leaf();
  }

  #[inline]
  pub(super) fn is_leaf(&self) -> bool {
    self.as_ref().is_leaf()
  }

  pub(super) fn add_edge(&self, e: Edge<T>) {
    self.as_mut().add_edge(e);
  }

  pub(super) fn replace_edge(&self, e: Edge<T>) {
    self.as_mut().replace_edge(e);
  }

  pub(super) fn get_edge(&self, label: u8) -> Option<(InnerEdgeKey, Node<T>)> {
    self.as_ref().get_edge(label)
  }

  pub(super) fn get_edge_ref(&self, label: u8) -> Option<(InnerEdgeKey, &Node<T>)> {
    self.as_ref().get_edge_ref(label)
  }

  pub(super) fn get_lower_bound_edge(&self, label: u8) -> Option<Node<T>> {
    self.as_ref().get_lower_bound_edge(label)
  }

  pub(super) fn remove_edge(&self, label: u8) -> Option<Node<T>> {
    self.as_mut().remove_edge(label)
  }
}

#[cfg(test)]
pub(crate) fn copy_node<T>(n: &Node<T>) -> Node<T> {
  let nn = Node {
    ptr: Arc::new(UnsafeCell::new(match n.as_ref() {
      Inner::Vec(_) => Inner::Vec(Default::default()),
      Inner::BTree(_) => Inner::BTree(Default::default()),
    })),
  };

  // TODO: track

  if !n.as_ref().prefix().is_empty() {
    nn.as_mut().set_prefix(n.as_ref().prefix().clone());
  }

  if let Some(leaf) = n.as_ref().leaf() {
    nn.as_mut().set_leaf(leaf.clone());
  }

  match n.as_ref() {
    Inner::Vec(n) => {
      let nn_ref = nn.as_mut().as_vec_mut();
      for e in n.edges.iter() {
        nn_ref.edges.push(Edge::new(e.label, copy_node(&e.node)));
      }
    }
    Inner::BTree(n) => {
      let nn_ref = nn.as_mut().as_btree_mut();
      for (label, node) in n.edges.iter() {
        nn_ref.edges.insert(*label, copy_node(node));
      }
    }
  }
  nn
}

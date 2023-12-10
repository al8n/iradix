use super::*;
use crate::concat;

pub(super) struct VecInner<T> {
  /// Used to store possible leaf
  pub(super) leaf: Option<LeafNode<T>>,

  /// The common prefix we ignore
  pub(super) prefix: Bytes,

  /// Should be stored in-order for iteration.
  /// We avoid a fully materialized slice to save memory,
  /// since in most cases we expect to be sparse
  pub(super) edges: Vec<Edge<T>>,
}

impl<T> Default for VecInner<T> {
  fn default() -> Self {
    Self {
      leaf: None,
      prefix: Bytes::new(),
      edges: Vec::new(),
    }
  }
}

impl<T> VecInner<T> {
  #[inline]
  pub(super) fn new(prefix: Bytes, leaf: Option<LeafNode<T>>, edges: Vec<Edge<T>>) -> Self {
    Self {
      leaf,
      prefix,
      edges,
    }
  }

  pub(super) fn merge_child(&mut self) {
    // Mark the child node as being mutated since we are about to abandon
    // it. We don't need to mark the leaf since we are retaining it if it
    // is there.
    let e = self.edges.pop().unwrap();
    // TODO: track

    let child_ref = e.node.as_ref().as_vec();
    // Merge the nodes.
    self.prefix = concat(&self.prefix, &child_ref.prefix);
    self.leaf = child_ref.leaf.clone();
    if !child_ref.edges.is_empty() {
      self.edges = child_ref.edges.clone();
    } else {
      self.edges.clear();
    }
  }
}

impl<T> NodeInner<T> for VecInner<T> {
  type Key = usize;

  fn is_leaf(&self) -> bool {
    self.leaf.is_some()
  }

  fn set_leaf(&mut self, leaf: LeafNode<T>) {
    self.leaf = Some(leaf);
  }

  fn clear_leaf(&mut self) {
    self.leaf = None;
  }

  fn leaf(&self) -> Option<&LeafNode<T>> {
    self.leaf.as_ref()
  }

  fn prefix(&self) -> &Bytes {
    &self.prefix
  }

  fn set_prefix(&mut self, prefix: Bytes) {
    self.prefix = prefix;
  }

  fn num_edges(&self) -> usize {
    self.edges.len()
  }

  fn update_edge(&mut self, idx: Self::Key, node: Node<T>) {
    self.edges[idx].node = node;
  }

  fn clear_edges(&mut self) {
    self.edges.clear();
  }

  fn add_edge(&mut self, e: Edge<T>) {
    let num = self.edges.len();
    let idx = indexsort::search(num, |i| self.edges[i].label >= e.label);

    if idx != num {
      self.edges.insert(idx, e);
    } else {
      self.edges.push(e);
    }
  }

  fn replace_edge(&mut self, e: Edge<T>) {
    let num = self.edges.len();
    let idx = indexsort::search(num, |i| self.edges[i].label >= e.label);
    if idx < num && self.edges[idx].label == e.label {
      self.edges[idx].node = e.node;
    } else {
      panic!("replacing missing edge");
    }
  }

  fn get_edge(&self, label: u8) -> Option<(Self::Key, Node<T>)> {
    self
      .get_edge_ref(label)
      .map(|(idx, node)| (idx, node.clone()))
  }

  fn get_edge_ref(&self, label: u8) -> Option<(Self::Key, &Node<T>)> {
    let num = self.edges.len();
    let idx = indexsort::search(num, |i| self.edges[i].label >= label);
    if idx < num && self.edges[idx].label == label {
      Some((idx, &self.edges[idx].node))
    } else {
      None
    }
  }

  fn get_lower_bound_edge(&self, label: u8) -> Option<Node<T>> {
    let num = self.edges.len();
    let idx = indexsort::search(num, |i| self.edges[i].label >= label);
    if idx < num {
      Some(self.edges[idx].node.clone())
    } else {
      None
    }
  }

  fn remove_edge(&mut self, label: u8) -> Option<Node<T>> {
    let num = self.edges.len();
    let idx = indexsort::search(num, |i| self.edges[i].label >= label);
    if idx < num && self.edges[idx].label == label {
      Some(self.edges.remove(idx).node)
    } else {
      None
    }
  }
}

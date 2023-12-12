use super::*;
use crate::{concat, util::Cow};

pub(super) struct BTreeInner<T> {
  /// Used to store possible leaf
  pub(super) leaf: Option<LeafNode<T>>,

  /// The common prefix we ignore
  pub(super) prefix: Bytes,

  /// Should be stored in-order for iteration.
  /// We avoid a fully materialized slice to save memory,
  /// since in most cases we expect to be sparse
  pub(super) edges: Cow<BTreeMap<u8, Node<T>>>,
}

impl<T> Default for BTreeInner<T> {
  fn default() -> Self {
    Self {
      leaf: None,
      prefix: Bytes::new(),
      edges: Cow::Owned(BTreeMap::new()),
    }
  }
}

impl<T> BTreeInner<T> {
  #[inline]
  pub(super) fn new(
    prefix: Bytes,
    leaf: Option<LeafNode<T>>,
    edges: Cow<BTreeMap<u8, Node<T>>>,
  ) -> Self {
    Self {
      leaf,
      prefix,
      edges,
    }
  }

  pub(super) fn merge_child(&mut self) {
    match self.edges {
      Cow::Borrowed(ref t) => {
        // self.edges = Cow::Owned((**t).clone());
        // Mark the child node as being mutated since we are about to abandon
        // it. We don't need to mark the leaf since we are retaining it if it
        // is there.
        let (_, node) = t.first_key_value().unwrap();
        // TODO: track

        let child_ref = node.as_ref().as_btree();
        // Merge the nodes.
        self.prefix = concat(&self.prefix, &child_ref.prefix);
        self.leaf = child_ref.leaf.clone();
        if !child_ref.edges.is_empty() {
          self.edges = child_ref.edges.to_borrowed();
        } else {
          self.edges = Cow::Owned(BTreeMap::new());
        }
      }
      Cow::Owned(ref mut t) => {
        // Mark the child node as being mutated since we are about to abandon
        // it. We don't need to mark the leaf since we are retaining it if it
        // is there.
        let (_, node) = t.first_key_value().unwrap();
        // TODO: track

        let child_ref = node.as_ref().as_btree();
        // Merge the nodes.
        self.prefix = concat(&self.prefix, &child_ref.prefix);
        self.leaf = child_ref.leaf.clone();
        if !child_ref.edges.is_empty() {
          self.edges = child_ref.edges.to_borrowed();
        } else {
          t.clear();
        }
      }
    }
  }
}

impl<T> NodeInner<T> for BTreeInner<T> {
  type Key = u8;

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
    self.edges.insert(idx, node);
  }

  fn clear_edges(&mut self) {
    self.edges.clear();
  }

  fn add_edge(&mut self, e: Edge<T>) {
    self.edges.insert(e.label, e.node);
  }

  fn replace_edge(&mut self, e: Edge<T>) {
    if let Some(node) = self.edges.get_mut(&e.label) {
      *node = e.node;
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
    self.edges.get(&label).map(|n| (label, n))
  }

  fn get_lower_bound_edge(&self, label: u8) -> Option<Node<T>> {
    self
      .edges
      .range(label..)
      .next()
      .map(|(_, node)| node.clone())
  }

  fn remove_edge(&mut self, label: u8) -> Option<Node<T>> {
    self.edges.remove(&label)
  }
}

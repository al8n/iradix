use super::*;
use crate::{concat, util::Cow};

#[derive(Debug)]
pub(super) struct VecInner<T> {
  /// Used to store possible leaf
  pub(super) leaf: Option<LeafNode<T>>,

  /// The common prefix we ignore
  pub(super) prefix: Bytes,

  /// Should be stored in-order for iteration.
  /// We avoid a fully materialized slice to save memory,
  /// since in most cases we expect to be sparse
  pub(super) edges: Cow<Vec<Edge<T>>>,
}

impl<T> Default for VecInner<T> {
  fn default() -> Self {
    Self {
      leaf: None,
      prefix: Bytes::new(),
      edges: Cow::Owned(Vec::new()),
    }
  }
}

impl<T> VecInner<T> {
  #[inline]
  pub(super) fn new(prefix: Bytes, leaf: Option<LeafNode<T>>, edges: Cow<Vec<Edge<T>>>) -> Self {
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
    match self.edges {
      Cow::Borrowed(ref edges) => {
        let e = &edges[0];
        // TODO: track

        let child_ref = e.node.as_ref().as_vec();
        // Merge the nodes.
        self.prefix = concat(&self.prefix, &child_ref.prefix);
        self.leaf = child_ref.leaf.clone();
        if !child_ref.edges.is_empty() {
          self.edges = child_ref.edges.to_borrowed();
        } else {
          self.edges = Cow::Owned(Vec::new());
        }
      }
      Cow::Owned(ref mut edges) => {
        let e = &edges[0];
        // TODO: track

        let child_ref = e.node.as_ref().as_vec();
        // Merge the nodes.
        self.prefix = concat(&self.prefix, &child_ref.prefix);
        self.leaf = child_ref.leaf.clone();
        if !child_ref.edges.is_empty() {
          self.edges = child_ref.edges.to_borrowed();
        } else {
          edges.clear();
        }
      }
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
    let idx = self
      .edges
      .binary_search_by(|edge| edge.label.cmp(&e.label))
      .unwrap_or_else(|x| x);

    self.edges.insert(idx, e);
  }

  fn replace_edge(&mut self, e: Edge<T>) {
    let idx = self.edges.binary_search_by(|edge| edge.label.cmp(&e.label));

    match idx {
      Ok(index) => self.edges[index].node = e.node,
      Err(_) => panic!("replacing missing edge"),
    }
  }

  fn get_edge(&self, label: u8) -> Option<(Self::Key, Node<T>)> {
    self
      .get_edge_ref(label)
      .map(|(idx, node)| (idx, node.clone()))
  }

  fn get_edge_ref(&self, label: u8) -> Option<(Self::Key, &Node<T>)> {
    let idx = self.edges.binary_search_by(|edge| edge.label.cmp(&label));
    idx.ok().map(|index| (index, &self.edges[index].node))
  }

  fn get_lower_bound_edge(&self, label: u8) -> Option<Node<T>> {
    let idx = self.edges.partition_point(|edge| edge.label < label);
    self.edges.get(idx).map(|edge| edge.node.clone())
  }

  fn remove_edge(&mut self, label: u8) -> Option<Node<T>> {
    let idx = self.edges.binary_search_by(|edge| edge.label.cmp(&label));
    if let Ok(index) = idx {
      Some(self.edges.remove(index).node)
    } else {
      None
    }
  }
}

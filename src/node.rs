use std::sync::atomic::Ordering;

use bytes::Bytes;
use crossbeam_epoch::Atomic;

#[derive(Debug, Clone)]
struct Edge<V> {
  label: u8,
  node: Atomic<Node<V>>,
}

#[derive(Debug, Clone)]
struct LeafNode<V> {
  key: Bytes,
  value: V,
}

/// An immutable node in the radix tree.
#[derive(Debug, Clone)]
pub struct Node<V> {
  /// leaf is used to store possible leaf
  leaf: Option<LeafNode<V>>,

  /// The common prefix we ignore
  prefix: Bytes,

  /// Edges should be stored in-order for iteration.
	/// We avoid a fully materialized slice to save memory,
	/// since in most cases we expect to be sparse
  edges: Vec<Edge<V>>,
}

impl<V> Node<V> {
  #[inline]
  pub(super) const fn is_leaf(&self) -> bool {
    self.leaf.is_some()
  }

  #[inline]
  pub(super) fn add_edge(&mut self, e: Edge<V>) {
    let num = self.edges.len();
    let idx = indexsort::search(num, |i| {
      self.edges[i].label >= e.label
    });

    if idx != num {
      self.edges.insert(idx, e);
    }
  }

  #[inline]
  pub(super) fn replace_edge(&mut self, e: Edge<V>) {
    let num = self.edges.len();
    let idx = indexsort::search(num, |i| {
      self.edges[i].label >= e.label
    });

    if idx < num && self.edges[idx].label == e.label {
      self.edges[idx].node.store(e.node.load_consume(&crossbeam_epoch::pin()), Ordering::Release);

      return;
    }
    panic!("replacing missing edge");
  }

  #[inline]
  pub(super) fn get_edge(&self, label: u8) -> Option<(usize, Atomic<Node<V>>)> {
    let num: usize = self.edges.len();
    let idx = indexsort::search(num, |i| {
      self.edges[i].label >= label
    });

    if idx < num && self.edges[idx].label == label {
      return Some((idx, self.edges[idx].node.clone()));
    }

    None
  }

  #[inline]
  pub(super) fn get_lower_bound_edge(&self, label: u8) -> Option<(usize, Atomic<Node<V>>)> {
    let num: usize = self.edges.len();
    let idx = indexsort::search(num, |i| {
      self.edges[i].label >= label
    });

    // we want lower bound behavior so return even if it's not an exact match
    if idx < num {
      return Some((idx, self.edges[idx].node.clone()));
    }

    None
  }

  #[inline]
  pub(super) fn remove_edge(&mut self, label: u8) -> Option<Edge<V>> {
    let num: usize = self.edges.len();
    let idx = indexsort::search(num, |i| {
      self.edges[i].label >= label
    });

    if idx < num && self.edges[idx].label == label {
      return Some(self.edges.remove(idx));
    }

    None
  }

  #[inline]
  pub fn get_watch(&self, mut search: &[u8]) -> Option<((), &V)> {
    loop {
      // check for key exhaustion
      if search.is_empty() {
        if let Some(leaf) = &self.leaf {
          return Some(((), &leaf.value));
        }
        break;
      }

      // look for an edge
      if let Some((_, _n)) = self.get_edge(search[0]) {
        // Update to the finest granularity as the search makes progress

        // Consume the search prefix
        if search.starts_with(&self.prefix) {
          search = &search[self.prefix.len()..];
        } else {
          break;
        }
      } else {
        break;
      }
    }

    None
  }

  #[inline]
  pub fn get(&self, k: &[u8]) -> Option<&V> {
    self.get_watch(k).map(|(_, v)| v)
  }
}
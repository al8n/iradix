use crate::{Node, Tree, Txn};

macro_rules! next {
  () => {
    type Item = (&'a [u8], &'a V);

    fn next(&mut self) -> Option<Self::Item> {
      let mut leaf = None;
  
      while leaf.is_none() && self.node.is_some() {
        // visit the leaf values if any
        if let Some(l) = self.node.unwrap().leaf() {
          leaf = Some(l);
        }
  
        self.iterate();
      }
  
      if let Some(l) = leaf {
        return Some((&l.key, &l.val));
      }
  
      None
    }  
  };
}

macro_rules! iterate {
  () => {
    fn iterate(&mut self) {
      // Check for key exhaustion
      if self.path.is_empty() {
        self.node = None;
        return;
      }
  
      // Look for an edge
      if let Some(node) = self.node {
        if let Some((_, child)) = node.get_edge_ref(self.path[0]) {
          // Consume the search prefix
          let child_prefix = child.as_ref().prefix();
          if self.path.starts_with(child_prefix) {
            self.path = &self.path[child_prefix.len()..];
            self.node = Some(child);
          } else {
            self.node = None;
          }
        }
      }
    }  
  };
}

/// Used to iterate over a set of nodes from the root
/// down to a specified path. This will iterate over the same values that
/// the [`Node::walk_path`] method will.
/// 
/// [`Node::walk_path`]: struct.Node.html#method.walk_path
pub struct PathIterator<'a, V> {
  _t: &'a Tree<V>,
  node: Option<&'a Node<V>>,
  path: &'a [u8],
}

impl<'a, V> Iterator for PathIterator<'a, V> {
  next!();
}

impl<'a, V> PathIterator<'a, V> {
  iterate!();
}

/// Used to iterate over a set of nodes from the root
/// down to a specified path. This will iterate over the same values that
/// the [`Node::walk_path`] method will.
/// 
/// [`Node::walk_path`]: struct.Node.html#method.walk_path
pub struct TxnPathIterator<'a, V, S> {
  _t: &'a Txn<V, S>,
  node: Option<&'a Node<V>>,
  path: &'a [u8],
}

impl<'a, V, S> Iterator for TxnPathIterator<'a, V, S> {
  next!();
}

impl<'a, V, S> TxnPathIterator<'a, V, S> {
  iterate!();
}
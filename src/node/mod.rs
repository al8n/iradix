use std::{borrow::ToOwned, boxed::Box, vec::Vec};

use core::borrow::Borrow;

use archery::{SharedPointer, SharedPointerKind};

/// An edge from a parent node to a child, carrying the path-compressed label.
///
/// The label lives in the parent (edge-in-parent), so splitting an edge never
/// rewrites the child subtree's stored values — it only re-labels the edge and
/// reparents the existing (shared) child.
pub(crate) struct Edge<C, V, P>
where
  C: ?Sized + ToOwned,
  P: SharedPointerKind,
{
  pub(crate) label: Box<[C::Owned]>,
  pub(crate) child: SharedPointer<Node<C, V, P>, P>,
}

// Structural `Clone` (rust-type-conventions §8 exception): `SharedPointer::make_mut`
// requires the pointee to be `Clone`, and a clone is taken only when a node is
// shared between versions (refcount > 1).
impl<C, V, P> Clone for Edge<C, V, P>
where
  C: ?Sized + ToOwned,
  C::Owned: Clone,
  V: Clone,
  P: SharedPointerKind,
{
  #[inline]
  fn clone(&self) -> Self {
    Self {
      label: self.label.clone(),
      child: self.child.clone(),
    }
  }
}

/// A node in the trie. Values are stored inline as `Option<V>`; children are kept
/// sorted by their edge label's first component, located by binary search.
pub(crate) struct Node<C, V, P>
where
  C: ?Sized + ToOwned,
  P: SharedPointerKind,
{
  pub(crate) value: Option<V>,
  pub(crate) children: Vec<Edge<C, V, P>>,
}

impl<C, V, P> Clone for Node<C, V, P>
where
  C: ?Sized + ToOwned,
  C::Owned: Clone,
  V: Clone,
  P: SharedPointerKind,
{
  #[inline]
  fn clone(&self) -> Self {
    Self {
      value: self.value.clone(),
      children: self.children.clone(),
    }
  }
}

impl<C, V, P> Node<C, V, P>
where
  C: ?Sized + ToOwned,
  P: SharedPointerKind,
{
  #[inline]
  pub(crate) const fn new() -> Self {
    Self {
      value: None,
      children: Vec::new(),
    }
  }

  /// Counts values stored in this subtree (this node plus all descendants).
  #[cfg(test)]
  pub(crate) fn count(&self) -> usize {
    let mut total = usize::from(self.value.is_some());
    for edge in &self.children {
      total += edge.child.count();
    }
    total
  }

  /// Checks the canonical (path-compressed) invariants for a subtree rooted here:
  /// no edge label is empty, and no non-root node is a redundant valueless
  /// single-child node. `is_root` exempts the root, which is never merged.
  #[cfg(test)]
  pub(crate) fn is_canonical(&self, is_root: bool) -> bool {
    if !is_root && self.value.is_none() && self.children.len() == 1 {
      return false;
    }
    for edge in &self.children {
      if edge.label.is_empty() || !edge.child.is_canonical(false) {
        return false;
      }
    }
    true
  }
}

impl<C, V, P> Node<C, V, P>
where
  C: ?Sized + ToOwned + Ord,
  P: SharedPointerKind,
{
  /// Binary-searches children for the edge whose label begins with `first`.
  ///
  /// `Ok(i)` indexes the matching edge; `Err(i)` is the insertion point that
  /// keeps `children` sorted by first component.
  #[inline]
  pub(crate) fn child_index(&self, first: &C) -> Result<usize, usize> {
    self
      .children
      .binary_search_by(|edge| Borrow::<C>::borrow(&edge.label[0]).cmp(first))
  }
}

/// Length of the longest common prefix between a stored `label` and the query
/// components remaining in `key`. Consumes the matched components from `key`.
pub(crate) fn match_prefix<C, I>(label: &[C::Owned], key: &mut core::iter::Peekable<I>) -> usize
where
  C: ?Sized + ToOwned + PartialEq,
  I: Iterator,
  I::Item: Borrow<C>,
{
  let mut shared = 0;
  for owned in label {
    match key.peek() {
      Some(item) if Borrow::<C>::borrow(owned) == item.borrow() => {
        shared += 1;
        key.next();
      }
      _ => break,
    }
  }
  shared
}

/// Length of the longest common prefix of two owned component slices, compared
/// by borrowing each element to `&C` (so only `C: PartialEq` is needed, not
/// `C::Owned: PartialEq`).
pub(crate) fn common_len<C>(a: &[C::Owned], b: &[C::Owned]) -> usize
where
  C: ?Sized + ToOwned + PartialEq,
{
  a.iter()
    .zip(b)
    .take_while(|(x, y)| Borrow::<C>::borrow(*x) == Borrow::<C>::borrow(*y))
    .count()
}

//! The shared persistent-radix algorithm core.
//!
//! Everything here is generic over the [`archery::SharedPointerKind`] `P` and is
//! `pub(crate)` only: `archery` is confined to this module and never appears in any
//! public signature. The two public faces ([`crate::unsync`] / [`crate::sync`])
//! each fix `P` (`RcK` / `ArcK`) and re-expose this surface bound-minimized.
//!
//! # Panic safety
//!
//! The mutators give the strong exception guarantee against a panicking user
//! `Clone` / `Ord` / `PartialEq`: each does its panic-capable work before any
//! irreversible structural change and adjusts `len` only at the infallible
//! take/unlink point, so an unwind leaves the trie and `len` consistent and never
//! drops a value being returned. Post-commit re-compression performs no user
//! `Clone`/`Ord` (it MOVES labels), so it cannot unwind on user code. Two failure
//! modes are out of scope (see the crate docs): panicking `Drop` (it aborts while
//! unwinding) and allocation failure (it aborts via the alloc-error handler, not an
//! unwind), so neither can corrupt the trie by returning mid-operation.

use std::{borrow::ToOwned, boxed::Box, vec, vec::Vec};

use core::borrow::Borrow;

use archery::{SharedPointer, SharedPointerKind};

/// An edge from a parent node to a child, carrying the path-compressed label.
///
/// The label lives in the parent (edge-in-parent), so splitting an edge never
/// rewrites the child subtree's stored values — it only re-labels the edge and
/// reparents the existing (shared) child.
pub(crate) struct Edge<P, C, V>
where
  C: ?Sized + ToOwned,
  P: SharedPointerKind,
{
  pub(crate) label: Box<[C::Owned]>,
  pub(crate) child: SharedPointer<Node<P, C, V>, P>,
}

// Structural `Clone` (rust-type-conventions §8 exception): `SharedPointer::make_mut`
// requires the pointee to be `Clone`, and a clone is taken only when a node is
// shared between versions (refcount > 1).
impl<P, C, V> Clone for Edge<P, C, V>
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
pub(crate) struct Node<P, C, V>
where
  C: ?Sized + ToOwned,
  P: SharedPointerKind,
{
  pub(crate) value: Option<V>,
  pub(crate) children: Vec<Edge<P, C, V>>,
}

impl<P, C, V> Clone for Node<P, C, V>
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

impl<P, C, V> Node<P, C, V>
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

impl<P, C, V> Node<P, C, V>
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

  /// Returns a reference to the value stored at exactly the components yielded by
  /// `key`, if any. Zero allocation: `key` is walked lazily.
  pub(crate) fn get<I>(&self, key: I) -> Option<&V>
  where
    I: Iterator,
    I::Item: Borrow<C>,
  {
    let mut node = self;
    let mut key = key.peekable();
    loop {
      let Some(first) = key.peek() else {
        return node.value.as_ref();
      };
      let i = node.child_index(first.borrow()).ok()?;
      let edge = &node.children[i];
      let shared = match_prefix::<C, _>(&edge.label, &mut key);
      if shared != edge.label.len() {
        return None;
      }
      node = &edge.child;
    }
  }

  /// Returns the value of the deepest stored key that is a prefix of `key`. When
  /// `inclusive`, an exact match counts as its own ancestor.
  pub(crate) fn ancestor<I>(&self, key: I, inclusive: bool) -> Option<&V>
  where
    I: Iterator,
    I::Item: Borrow<C>,
  {
    let mut node = self;
    let mut key = key.peekable();
    let mut deepest: Option<&V> = None;
    loop {
      let exhausted = key.peek().is_none();
      if (inclusive || !exhausted)
        && let Some(v) = node.value.as_ref()
      {
        deepest = Some(v);
      }
      let Some(first) = key.peek() else {
        return deepest;
      };
      let Ok(i) = node.child_index(first.borrow()) else {
        return deepest;
      };
      let edge = &node.children[i];
      let shared = match_prefix::<C, _>(&edge.label, &mut key);
      if shared != edge.label.len() {
        // diverged inside this edge: nothing deeper can match
        return deepest;
      }
      node = &edge.child;
    }
  }

  /// Collects references to the values of every stored key that is a prefix of
  /// `key`, inclusive of an exact match, in root-to-`key` order.
  pub(crate) fn ancestors<I>(&self, key: I) -> Vec<&V>
  where
    I: Iterator,
    I::Item: Borrow<C>,
  {
    let mut found = Vec::new();
    let mut node = self;
    let mut key = key.peekable();
    loop {
      if let Some(v) = node.value.as_ref() {
        found.push(v);
      }
      let Some(first) = key.peek() else { break };
      let Ok(i) = node.child_index(first.borrow()) else {
        break;
      };
      let edge = &node.children[i];
      let shared = match_prefix::<C, _>(&edge.label, &mut key);
      if shared != edge.label.len() {
        break;
      }
      node = &edge.child;
    }
    found
  }

  /// Iterates references to every value in this subtree, in key order.
  #[inline]
  pub(crate) fn value_iter(&self) -> ValueIter<'_, P, C, V> {
    ValueIter::from_stack(std::vec![self])
  }

  /// Iterates references to the values of every *strict* descendant of `key`.
  #[inline]
  pub(crate) fn descendant_iter<I>(&self, key: I) -> ValueIter<'_, P, C, V>
  where
    I: Iterator,
    I::Item: Borrow<C>,
  {
    ValueIter::from_stack(self.descendant_roots(key))
  }

  /// Returns the subtree roots whose union is exactly the *strict* descendants of
  /// `key` (the value at `key` itself is excluded).
  fn descendant_roots<I>(&self, key: I) -> Vec<&Node<P, C, V>>
  where
    I: Iterator,
    I::Item: Borrow<C>,
  {
    let mut node = self;
    let mut key = key.peekable();
    loop {
      let Some(first) = key.peek() else {
        // key ends exactly at `node`: descendants are everything strictly below.
        return node.children.iter().map(|edge| &*edge.child).collect();
      };
      let Ok(i) = node.child_index(first.borrow()) else {
        return Vec::new();
      };
      let edge = &node.children[i];
      let shared = match_prefix::<C, _>(&edge.label, &mut key);
      if shared == edge.label.len() {
        node = &edge.child;
      } else if key.peek().is_none() {
        // key ends mid-edge: the whole child subtree is strict descendants.
        return std::vec![&*edge.child];
      } else {
        return Vec::new();
      }
    }
  }
}

impl<P, C, V> Node<P, C, V>
where
  C: ?Sized + ToOwned + Ord,
  C::Owned: Clone,
  V: Clone,
  P: SharedPointerKind,
{
  /// Inserts `value` at `key` in the subtree rooted at `node_ptr`, copying on
  /// write. Returns the previous value if the exact key was already set.
  pub(crate) fn insert(
    node_ptr: &mut SharedPointer<Node<P, C, V>, P>,
    key: &[C::Owned],
    value: V,
  ) -> Option<V> {
    let node = SharedPointer::make_mut(node_ptr);

    let Some(first) = key.first() else {
      return node.value.replace(value);
    };

    let i = match node.child_index(first.borrow()) {
      Err(insert_at) => {
        // No edge begins with `first`: create a fresh leaf edge here.
        let leaf = Node {
          value: Some(value),
          children: Vec::new(),
        };
        let edge = Edge {
          label: key.to_vec().into_boxed_slice(),
          child: SharedPointer::new(leaf),
        };
        node.children.insert(insert_at, edge);
        return None;
      }
      Ok(i) => i,
    };

    let shared = common_len::<C>(&node.children[i].label, key);
    let label_len = node.children[i].label.len();

    if shared == label_len {
      // The whole edge label is consumed; descend into the child.
      return Node::insert(&mut node.children[i].child, &key[shared..], value);
    }

    // Split edge `i` at `shared`. Build the COMPLETE replacement `mid` subtree —
    // its label clones, the reused old child (an O(1) pointer clone), the new
    // leaf, and the sorted child order — while edge `i` is still installed. Every
    // fallible step (label `C::Owned::clone`, leaf allocation, the user `Ord` that
    // orders the two children) therefore runs before anything is detached, so an
    // unwind leaves the trie and `len` untouched. Only the final single move
    // splices `mid` in, dropping the old edge (whose child was already cloned, so
    // no subtree is lost).
    let (head, tail) = node.children[i].label.split_at(shared);
    let head: Box<[C::Owned]> = head.to_vec().into_boxed_slice();
    let tail: Box<[C::Owned]> = tail.to_vec().into_boxed_slice();
    let old_child_edge = Edge {
      label: tail,
      child: node.children[i].child.clone(),
    };

    let key_rest = &key[shared..];
    let mid_node = if key_rest.is_empty() {
      // The new key ends exactly at the split point: value lives on `mid`.
      Node {
        value: Some(value),
        children: vec![old_child_edge],
      }
    } else {
      // The new key diverges from the old child within this edge: two children in
      // sorted order. The ordering `Ord` is the last fallible step before splicing.
      let new_leaf = Edge {
        label: key_rest.to_vec().into_boxed_slice(),
        child: SharedPointer::new(Node {
          value: Some(value),
          children: Vec::new(),
        }),
      };
      if Borrow::<C>::borrow(&new_leaf.label[0]) < Borrow::<C>::borrow(&old_child_edge.label[0]) {
        Node {
          value: None,
          children: vec![new_leaf, old_child_edge],
        }
      } else {
        Node {
          value: None,
          children: vec![old_child_edge, new_leaf],
        }
      }
    };

    node.children[i] = Edge {
      label: head,
      child: SharedPointer::new(mid_node),
    };
    None
  }

  /// Removes the value at `key` from the subtree rooted at `node_ptr`, copying on
  /// write and re-compressing. Returns the removed value if present.
  pub(crate) fn remove(
    node_ptr: &mut SharedPointer<Node<P, C, V>, P>,
    key: &[C::Owned],
    len: &mut usize,
  ) -> Option<V> {
    let node = SharedPointer::make_mut(node_ptr);

    let Some(first) = key.first() else {
      // The value `take` is the single infallible commit point: decrement `len`
      // here, after the fallible make-mut/traversal above has already succeeded
      // and before any (fallible) re-compression on the way back up. An unwind
      // before this point never reaches it, so `len` stays accurate.
      let removed = node.value.take();
      if removed.is_some() {
        *len -= 1;
      }
      return removed;
    };

    let i = node.child_index(first.borrow()).ok()?;
    let shared = common_len::<C>(&node.children[i].label, key);
    if shared != node.children[i].label.len() {
      return None;
    }

    let removed = Node::remove(&mut node.children[i].child, &key[shared..], len);
    if removed.is_some() {
      normalize_child(node, i);
    }
    removed
  }

  /// Unlinks every strict descendant of `key` under `node_ptr`, copying on write
  /// and re-compressing. Returns the number of values removed and decrements
  /// `*len` by that amount.
  ///
  /// `len` is corrected atomically with the (infallible) unlink — counting the
  /// removed subtree and clearing/removing it happen together, after the fallible
  /// make-mut/traversal and before the (fallible) `normalize_child` on the way
  /// back up. So an unwind before the unlink leaves `len` untouched, and an
  /// unwind during re-compression leaves `len` already accurate with every
  /// surviving key resolvable. Callers still capture any values they need first
  /// (see [`crate::unsync::Radix::drain_descendants`]).
  pub(crate) fn unlink_descendants(
    node_ptr: &mut SharedPointer<Node<P, C, V>, P>,
    key: &[C::Owned],
    len: &mut usize,
  ) -> usize {
    let node = SharedPointer::make_mut(node_ptr);

    let Some(first) = key.first() else {
      // `key` ends here: every child subtree is a strict descendant — drop them.
      // Counting and clearing are infallible, so `len` is corrected atomically
      // with the unlink (after the fallible make-mut above).
      let removed: usize = node.children.iter().map(|edge| edge.child.count()).sum();
      *len -= removed;
      node.children.clear();
      return removed;
    };

    let Ok(i) = node.child_index(first.borrow()) else {
      return 0;
    };
    let shared = common_len::<C>(&node.children[i].label, key);
    let label_len = node.children[i].label.len();

    if shared == label_len {
      // Whole label consumed: recurse (which unlinks and corrects `len` deeper),
      // then re-canonicalize this node — the (fallible) normalize runs only after
      // the deeper unlink already adjusted `len`.
      let removed = Node::unlink_descendants(&mut node.children[i].child, &key[shared..], len);
      if removed > 0 {
        normalize_child(node, i);
      }
      removed
    } else if shared == key.len() {
      // `key` ends mid-edge: the whole child subtree is strict descendants.
      // Count and remove are infallible, so `len` is corrected with the unlink.
      let removed = node.children[i].child.count();
      *len -= removed;
      node.children.remove(i);
      removed
    } else {
      // `key` diverges from this edge — nothing matches, no change.
      0
    }
  }
}

/// The persistent-radix state shared by both public faces: a lazily-allocated
/// root plus an incrementally-tracked value count.
///
/// All trie operations live here, generic over `P`; [`crate::unsync::Radix`] and
/// [`crate::sync::Radix`] are thin wrappers that fix `P` and forward, so the COW
/// algorithm exists in exactly one place.
pub(crate) struct Root<P, C, V>
where
  C: ?Sized + ToOwned,
  P: SharedPointerKind,
{
  // `None` is the empty trie; the root node is allocated lazily on first insert
  // so that `new` is `const` (a refcounted allocation is not a `const` op).
  root: Option<SharedPointer<Node<P, C, V>, P>>,
  len: usize,
}

// O(1) clone — no `V: Clone` (the pointer is bumped, values are not touched).
impl<P, C, V> Clone for Root<P, C, V>
where
  C: ?Sized + ToOwned,
  P: SharedPointerKind,
{
  #[inline]
  fn clone(&self) -> Self {
    Self {
      root: self.root.clone(),
      len: self.len,
    }
  }
}

impl<P, C, V> Root<P, C, V>
where
  C: ?Sized + ToOwned,
  P: SharedPointerKind,
{
  /// Creates an empty trie. `const`: no allocation until the first insert.
  #[inline]
  pub(crate) const fn new() -> Self {
    Self { root: None, len: 0 }
  }

  /// Returns the number of values stored in the trie.
  #[inline]
  pub(crate) const fn len(&self) -> usize {
    self.len
  }

  /// Returns `true` if the trie holds no values.
  #[inline]
  pub(crate) const fn is_empty(&self) -> bool {
    self.len == 0
  }

  /// Removes every value, resetting the trie to empty.
  #[inline]
  pub(crate) fn clear(&mut self) {
    self.root = None;
    self.len = 0;
  }
}

impl<P, C, V> Root<P, C, V>
where
  C: ?Sized + ToOwned + Ord,
  P: SharedPointerKind,
{
  /// Returns a reference to the value stored at exactly `key`, if any.
  #[inline]
  pub(crate) fn get<I>(&self, key: I) -> Option<&V>
  where
    I: Iterator,
    I::Item: Borrow<C>,
  {
    self.root.as_ref()?.get(key)
  }

  /// Returns the value of the deepest stored prefix of `key`; `inclusive` decides
  /// whether an exact match is its own ancestor.
  #[inline]
  pub(crate) fn ancestor<I>(&self, key: I, inclusive: bool) -> Option<&V>
  where
    I: Iterator,
    I::Item: Borrow<C>,
  {
    self.root.as_ref()?.ancestor(key, inclusive)
  }

  /// Iterates references to every value in the trie, in key order.
  #[inline]
  pub(crate) fn values(&self) -> ValueIter<'_, P, C, V> {
    match self.root.as_ref() {
      Some(root) => root.value_iter(),
      None => ValueIter::empty(),
    }
  }

  /// Iterates references to the values of `key`'s ancestors (inclusive).
  #[inline]
  pub(crate) fn ancestors<'a, I>(&'a self, key: I) -> SliceIter<'a, V>
  where
    I: Iterator,
    I::Item: Borrow<C>,
  {
    match self.root.as_ref() {
      Some(root) => SliceIter::new(root.ancestors(key)),
      None => SliceIter::new(Vec::new()),
    }
  }

  /// Iterates references to the values of `key`'s strict descendants.
  #[inline]
  pub(crate) fn descendants<I>(&self, key: I) -> ValueIter<'_, P, C, V>
  where
    I: Iterator,
    I::Item: Borrow<C>,
  {
    match self.root.as_ref() {
      Some(root) => root.descendant_iter(key),
      None => ValueIter::empty(),
    }
  }
}

impl<P, C, V> Root<P, C, V>
where
  C: ?Sized + ToOwned + Ord,
  C::Owned: Clone,
  V: Clone,
  P: SharedPointerKind,
{
  /// Inserts `value` at `key`, returning the previous value if the key was set.
  pub(crate) fn insert(&mut self, components: &[C::Owned], value: V) -> Option<V> {
    let root = self
      .root
      .get_or_insert_with(|| SharedPointer::new(Node::new()));
    let old = Node::insert(root, components, value);
    if old.is_none() {
      self.len += 1;
    }
    old
  }

  /// Removes and returns the value at exactly `key`, if any.
  pub(crate) fn remove(&mut self, components: &[C::Owned]) -> Option<V> {
    // A read-only existence check first: a remove of an absent key must not
    // copy-on-write (and so must not disturb structural sharing). When the key
    // is present, every node on its root-to-value path genuinely changes, so the
    // eager copy-on-write in `Node::remove` is justified.
    self.get(components.iter().map(Borrow::<C>::borrow))?;
    let root = self.root.as_mut()?;
    // `Node::remove` decrements `len` at the value-`take` itself — after its
    // fallible make-mut/traversal succeeds — so a panic in a shared-node clone or
    // a user comparison on the way down leaves `len` and the trie consistent.
    Node::remove(root, components, &mut self.len)
  }

  /// Removes every *strict* descendant of `key` (the value at `key`, if any, is
  /// kept), returning the number of values removed. Never clones a `V`.
  pub(crate) fn remove_descendants(&mut self, components: &[C::Owned]) -> usize {
    // Read-only existence check: nothing to remove means no copy-on-write (so
    // structural sharing is preserved) and no `len` change. This traversal is
    // fallible (user comparisons) but mutates nothing, so a panic is harmless.
    if self
      .descendants(components.iter().map(Borrow::<C>::borrow))
      .next()
      .is_none()
    {
      return 0;
    }
    // `unlink_descendants` counts and unlinks the strict descendants, correcting
    // `len` atomically with the (infallible) unlink — and never cloning a `V`.
    match self.root.as_mut() {
      Some(root) => Node::unlink_descendants(root, components, &mut self.len),
      None => 0,
    }
  }

  /// Removes every *strict* descendant of `key` and returns their values (the
  /// value at `key`, if any, is kept). Clones values out before unlinking.
  pub(crate) fn drain_descendants(&mut self, components: &[C::Owned]) -> Vec<V> {
    // Phase 1 (read-only, fallible): clone every strict-descendant value out
    // FIRST, before unlinking anything. A `V::clone` panic here unwinds with the
    // trie and `len` completely untouched. This also doubles as the
    // nothing-to-drain check: an empty result means no copy-on-write (preserving
    // structural sharing).
    let out: Vec<V> = self
      .descendants(components.iter().map(Borrow::<C>::borrow))
      .cloned()
      .collect();
    if out.is_empty() {
      return out;
    }
    // Phase 2: the values are safely captured, so commit the structural change.
    // `unlink_descendants` corrects `len` atomically with the (infallible) unlink.
    if let Some(root) = self.root.as_mut() {
      Node::unlink_descendants(root, components, &mut self.len);
    }
    out
  }
}

#[cfg(test)]
impl<P, C, V> Root<P, C, V>
where
  C: ?Sized + ToOwned + Ord,
  P: SharedPointerKind,
{
  /// Test-only: number of direct edges from the root.
  pub(crate) fn root_child_count(&self) -> usize {
    self.root.as_ref().map_or(0, |r| r.children.len())
  }

  /// Test-only: whether the root holds a value.
  pub(crate) fn root_has_value(&self) -> bool {
    self.root.as_ref().is_some_and(|r| r.value.is_some())
  }

  /// Test-only: the true value count by walking the trie, to cross-check the
  /// incrementally-tracked `len`.
  pub(crate) fn count_values(&self) -> usize {
    self.root.as_ref().map_or(0, |r| r.count())
  }

  /// Test-only: whether the trie is in canonical path-compressed form.
  pub(crate) fn is_canonical(&self) -> bool {
    self.root.as_ref().is_none_or(|r| r.is_canonical(true))
  }

  /// Test-only: the child subtree pointer for the root edge starting with
  /// component `first`, for structural-sharing (`ptr_eq`) assertions.
  pub(crate) fn edge_child(&self, first: &C) -> Option<SharedPointer<Node<P, C, V>, P>> {
    let root = self.root.as_ref()?;
    let i = root.child_index(first).ok()?;
    Some(root.children[i].child.clone())
  }
}

/// Depth-first iterator over references to every value in a forest of subtrees,
/// in key order. Shared by the public `values` / `descendants` iterators.
pub(crate) struct ValueIter<'a, P, C, V>
where
  C: ?Sized + ToOwned,
  P: SharedPointerKind,
{
  stack: Vec<&'a Node<P, C, V>>,
}

impl<'a, P, C, V> ValueIter<'a, P, C, V>
where
  C: ?Sized + ToOwned,
  P: SharedPointerKind,
{
  #[inline]
  pub(crate) const fn empty() -> Self {
    Self { stack: Vec::new() }
  }

  #[inline]
  pub(crate) const fn from_stack(stack: Vec<&'a Node<P, C, V>>) -> Self {
    Self { stack }
  }
}

impl<'a, P, C, V> Iterator for ValueIter<'a, P, C, V>
where
  C: ?Sized + ToOwned,
  P: SharedPointerKind,
{
  type Item = &'a V;

  fn next(&mut self) -> Option<Self::Item> {
    while let Some(node) = self.stack.pop() {
      // push children in reverse so the smallest is visited first (key order)
      for edge in node.children.iter().rev() {
        self.stack.push(&edge.child);
      }
      if let Some(v) = node.value.as_ref() {
        return Some(v);
      }
    }
    None
  }
}

/// Iterator over a pre-collected list of value references (the ancestor chain).
pub(crate) struct SliceIter<'a, V> {
  values: Vec<&'a V>,
  pos: usize,
}

impl<'a, V> SliceIter<'a, V> {
  #[inline]
  pub(crate) const fn new(values: Vec<&'a V>) -> Self {
    Self { values, pos: 0 }
  }
}

impl<'a, V> Iterator for SliceIter<'a, V> {
  type Item = &'a V;

  #[inline]
  fn next(&mut self) -> Option<Self::Item> {
    let v = self.values.get(self.pos).copied();
    if v.is_some() {
      self.pos += 1;
    }
    v
  }
}

/// After a child edge has been mutated, restore canonical (path-compressed)
/// shape at index `i` of `node`: prune an emptied child, or collapse a
/// valueless single-child child by extending the edge label.
fn normalize_child<P, C, V>(node: &mut Node<P, C, V>, i: usize)
where
  C: ?Sized + ToOwned + Ord,
  C::Owned: Clone,
  V: Clone,
  P: SharedPointerKind,
{
  if node.children[i].child.value.is_some() {
    return;
  }
  match node.children[i].child.children.len() {
    0 => {
      // Dead end: drop the edge entirely.
      node.children.remove(i);
    }
    1 => {
      // Redundant node: splice its single grandchild edge into this edge by
      // MOVING both labels. The child was un-shared on the way down, so we own it
      // here — cloning the labels would be wasteful and, worse, a panic hazard for
      // any value still in flight up the call stack (a `remove`/`drain` return).
      //
      // This runs post-commit (the value is already taken/unlinked), so it must
      // not unwind on user code: it performs NO `C::Owned`/`V` clone and no `Ord`.
      // The lone allocation (the merged label) can only fail with OOM, which aborts
      // rather than unwinds (see the crate panic-safety docs), so the in-flight
      // return value is never dropped by an unwind here.
      let merged_len =
        node.children[i].label.len() + node.children[i].child.children[0].label.len();
      let mut merged: Vec<C::Owned> = Vec::with_capacity(merged_len);
      let grandchild_edge = {
        let child = SharedPointer::make_mut(&mut node.children[i].child);
        child.children.pop().expect("checked exactly one child")
      };
      merged.extend(core::mem::take(&mut node.children[i].label));
      merged.extend(grandchild_edge.label);
      node.children[i].label = merged.into_boxed_slice();
      node.children[i].child = grandchild_edge.child;
    }
    _ => {}
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

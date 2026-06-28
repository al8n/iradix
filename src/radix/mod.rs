use std::{borrow::ToOwned, boxed::Box, vec::Vec};

use core::borrow::Borrow;

use archery::{ArcK, RcK, SharedPointer, SharedPointerKind};

use crate::{
  RadixKey,
  node::{Edge, Node, common_len, match_prefix},
};

/// A [`Radix`] using [`Rc`](std::rc::Rc) pointers: cheap clones, no atomics,
/// but `!Send` / `!Sync`. Use this for a trie confined to one thread.
pub type LocalRadix<C, V> = Radix<C, V, RcK>;

/// A [`Radix`] using [`Arc`](std::sync::Arc) pointers: atomic refcounts, and
/// `Send + Sync` whenever `C::Owned` and `V` are. Use this to share snapshots
/// across threads (see [`ConcurrentRadix`](crate::ConcurrentRadix)).
pub type SyncRadix<C, V> = Radix<C, V, ArcK>;

/// A generic, persistent (copy-on-write) radix trie with structural sharing.
///
/// Keys decompose into [`Component`](RadixKey::Component)s via [`RadixKey`]; the
/// trie is parameterized over the component type `C`, the value type `V`, and the
/// reference-counting pointer kind `P` (see [`LocalRadix`] / [`SyncRadix`]).
///
/// Every mutation produces a logically new trie. A [`clone`](Clone::clone) is
/// O(1) and shares all structure with the original; a write copies only the path
/// from the root to the touched node, leaving every untouched subtree physically
/// shared with prior versions. This makes snapshots free and isolation automatic.
///
/// # Bounds
///
/// The struct itself requires only the structural `C: ?Sized + ToOwned` and
/// `P: SharedPointerKind`. Reads add `C: Ord`; persistent mutators add
/// `C: Ord, C::Owned: Clone, V: Clone`.
pub struct Radix<C, V, P = RcK>
where
  C: ?Sized + ToOwned,
  P: SharedPointerKind,
{
  // `None` is the empty trie; the root node is allocated lazily on first insert
  // so that `new` is `const` (a refcounted allocation is not a `const` op).
  root: Option<SharedPointer<Node<C, V, P>, P>>,
  len: usize,
}

// O(1) clone — no `V: Clone` (the pointer is bumped, values are not touched).
impl<C, V, P> Clone for Radix<C, V, P>
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

impl<C, V, P> Default for Radix<C, V, P>
where
  C: ?Sized + ToOwned,
  P: SharedPointerKind,
{
  #[inline]
  fn default() -> Self {
    Self::new()
  }
}

impl<C, V, P> Radix<C, V, P>
where
  C: ?Sized + ToOwned,
  P: SharedPointerKind,
{
  /// Creates an empty trie. `const`: no allocation happens until the first
  /// insert.
  #[inline]
  pub const fn new() -> Self {
    Self { root: None, len: 0 }
  }

  /// Returns the number of values stored in the trie.
  #[inline]
  pub const fn len(&self) -> usize {
    self.len
  }

  /// Returns `true` if the trie holds no values.
  #[inline]
  pub const fn is_empty(&self) -> bool {
    self.len == 0
  }

  /// Removes every value, resetting the trie to empty.
  #[inline]
  pub fn clear(&mut self) {
    self.root = None;
    self.len = 0;
  }
}

// ----- reads (C: Ord, V-bound-free) ---------------------------------------

impl<C, V, P> Radix<C, V, P>
where
  C: ?Sized + ToOwned + Ord,
  P: SharedPointerKind,
{
  /// Returns a reference to the value stored at exactly `key`, if any.
  ///
  /// Zero allocation: the key is walked lazily over its components.
  pub fn get<K>(&self, key: &K) -> Option<&V>
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    let mut node = &**self.root.as_ref()?;
    let mut key = key.components().peekable();
    loop {
      let Some(first) = key.peek() else {
        return node.value.as_ref();
      };
      let i = node.child_index(first.borrow()).ok()?;
      let edge = &node.children[i];
      let shared = match_prefix(&edge.label, &mut key);
      if shared != edge.label.len() {
        return None;
      }
      node = &edge.child;
    }
  }

  /// Returns `true` if a value is stored at exactly `key`.
  #[inline]
  pub fn contains<K>(&self, key: &K) -> bool
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    self.get(key).is_some()
  }

  /// Returns the value of the deepest stored key that is a prefix of `key`,
  /// **inclusive** of an exact match (longest-prefix match).
  pub fn get_ancestor<K>(&self, key: &K) -> Option<&V>
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    self.ancestor_value(key, true)
  }

  /// Returns the value of the deepest stored key that is a *strict* prefix of
  /// `key` (excludes an exact match).
  pub fn strict_ancestor<K>(&self, key: &K) -> Option<&V>
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    self.ancestor_value(key, false)
  }

  /// Returns `true` if any stored key is a prefix of `key` (inclusive).
  #[inline]
  pub fn has_ancestor<K>(&self, key: &K) -> bool
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    self.get_ancestor(key).is_some()
  }

  fn ancestor_value<K>(&self, key: &K, inclusive: bool) -> Option<&V>
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    let mut node = &**self.root.as_ref()?;
    let mut key = key.components().peekable();
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
      let shared = match_prefix(&edge.label, &mut key);
      if shared != edge.label.len() {
        // diverged inside this edge: nothing deeper can match
        return deepest;
      }
      node = &edge.child;
    }
  }

  /// Iterates references to every value in the trie, in key order.
  #[inline]
  pub fn values(&self) -> Values<'_, C, V, P> {
    let stack = match self.root.as_ref() {
      Some(root) => std::vec![&**root],
      None => Vec::new(),
    };
    Values { stack }
  }

  /// Iterates references to the values of every stored key that is a prefix of
  /// `key`, **inclusive** of an exact match (root-to-`key` path).
  pub fn ancestors<K>(&self, key: &K) -> Ancestors<'_, V>
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    let mut found = Vec::new();
    let Some(root) = self.root.as_ref() else {
      return Ancestors {
        values: found,
        pos: 0,
      };
    };
    let mut node = &**root;
    let mut key = key.components().peekable();
    loop {
      if let Some(v) = node.value.as_ref() {
        found.push(v);
      }
      let Some(first) = key.peek() else { break };
      let Ok(i) = node.child_index(first.borrow()) else {
        break;
      };
      let edge = &node.children[i];
      let shared = match_prefix(&edge.label, &mut key);
      if shared != edge.label.len() {
        break;
      }
      node = &edge.child;
    }
    Ancestors {
      values: found,
      pos: 0,
    }
  }

  /// Iterates references to the values of every *strict* descendant of `key`
  /// (stored keys that strictly extend `key`; the value at `key` is excluded).
  pub fn descendants<K>(&self, key: &K) -> Descendants<'_, C, V, P>
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    let Some(root) = self.root.as_ref() else {
      return Descendants { stack: Vec::new() };
    };
    let mut node = &**root;
    let mut key = key.components().peekable();
    loop {
      let Some(first) = key.peek() else {
        // key ends exactly at `node`: descendants are everything strictly below.
        let mut stack = Vec::new();
        for edge in &node.children {
          stack.push(&*edge.child);
        }
        return Descendants { stack };
      };
      let Ok(i) = node.child_index(first.borrow()) else {
        return Descendants { stack: Vec::new() };
      };
      let edge = &node.children[i];
      let shared = match_prefix(&edge.label, &mut key);
      if shared == edge.label.len() {
        node = &edge.child;
      } else if key.peek().is_none() {
        // key ends mid-edge: the whole child subtree is strict descendants.
        return Descendants {
          stack: std::vec![&*edge.child],
        };
      } else {
        return Descendants { stack: Vec::new() };
      }
    }
  }
}

// ----- mutators (C: Ord, C::Owned: Clone, V: Clone) -----------------------

impl<C, V, P> Radix<C, V, P>
where
  C: ?Sized + ToOwned + Ord,
  C::Owned: Clone,
  V: Clone,
  P: SharedPointerKind,
{
  /// Inserts `value` at `key`, returning the previous value if the key was set.
  pub fn insert<K>(&mut self, key: &K, value: V) -> Option<V>
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    let components: Vec<C::Owned> = key.components().map(|c| c.borrow().to_owned()).collect();
    let root = self
      .root
      .get_or_insert_with(|| SharedPointer::new(Node::new()));
    let old = insert_rec(root, &components, value);
    if old.is_none() {
      self.len += 1;
    }
    old
  }

  /// Removes and returns the value at exactly `key`, if any.
  pub fn remove<K>(&mut self, key: &K) -> Option<V>
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    // A read-only existence check first: a remove of an absent key must not
    // copy-on-write (and so must not disturb structural sharing). When the key
    // is present, every node on its root-to-value path genuinely changes, so the
    // eager copy-on-write in `remove_rec` is justified.
    self.get(key)?;
    let components: Vec<C::Owned> = key.components().map(|c| c.borrow().to_owned()).collect();
    // The guard above confirmed the key exists, so the root is allocated.
    let root = self.root.as_mut()?;
    let old = remove_rec(root, &components);
    if old.is_some() {
      self.len -= 1;
    }
    old
  }

  /// Removes every *strict* descendant of `key` (the value at `key`, if any, is
  /// kept). Returns the number of values removed.
  pub fn remove_descendants<K>(&mut self, key: &K) -> usize
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    self.drain_descendants(key).len()
  }

  /// Removes every *strict* descendant of `key` and returns their values (the
  /// value at `key`, if any, is kept).
  pub fn drain_descendants<K>(&mut self, key: &K) -> Vec<V>
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    // A read-only check first: if there is nothing to drain, do not
    // copy-on-write (preserving structural sharing).
    if self.descendants(key).next().is_none() {
      return Vec::new();
    }
    let components: Vec<C::Owned> = key.components().map(|c| c.borrow().to_owned()).collect();
    let mut out = Vec::new();
    // The guard above confirmed a descendant exists, so the root is allocated.
    if let Some(root) = self.root.as_mut() {
      drain_rec(root, &components, &mut out);
    }
    self.len -= out.len();
    out
  }

  /// Starts a transaction that batches several edits into a single root publish.
  #[inline]
  pub fn txn(&mut self) -> Txn<'_, C, V, P> {
    Txn { radix: self }
  }
}

// ----- transaction --------------------------------------------------------

/// A batch of edits applied to a [`Radix`] as one atomic root publish.
///
/// A `Txn` mutates a working copy held in the parent [`Radix`]; the changes are
/// visible to any snapshot taken from that `Radix` only after [`commit`](Txn::commit).
/// (Because a `Radix` owns its root directly, dropping a `Txn` keeps the edits —
/// the atomicity that matters is for *snapshots* taken via
/// [`ConcurrentRadix`](crate::ConcurrentRadix), which publishes once on commit.)
pub struct Txn<'a, C, V, P>
where
  C: ?Sized + ToOwned,
  P: SharedPointerKind,
{
  radix: &'a mut Radix<C, V, P>,
}

impl<C, V, P> Txn<'_, C, V, P>
where
  C: ?Sized + ToOwned + Ord,
  P: SharedPointerKind,
{
  /// Returns `true` if any stored key is a prefix of `key` (inclusive).
  #[inline]
  pub fn has_ancestor<K>(&self, key: &K) -> bool
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    self.radix.has_ancestor(key)
  }

  /// Returns the value of the deepest stored prefix of `key`, inclusive.
  #[inline]
  pub fn get_ancestor<K>(&self, key: &K) -> Option<&V>
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    self.radix.get_ancestor(key)
  }
}

impl<C, V, P> Txn<'_, C, V, P>
where
  C: ?Sized + ToOwned + Ord,
  C::Owned: Clone,
  V: Clone,
  P: SharedPointerKind,
{
  /// Inserts `value` at `key`, returning the previous value if set.
  #[inline]
  pub fn insert<K>(&mut self, key: &K, value: V) -> Option<V>
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    self.radix.insert(key, value)
  }

  /// Removes and returns the value at exactly `key`, if any.
  #[inline]
  pub fn remove<K>(&mut self, key: &K) -> Option<V>
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    self.radix.remove(key)
  }

  /// Removes every strict descendant of `key`, returning the count.
  #[inline]
  pub fn remove_descendants<K>(&mut self, key: &K) -> usize
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    self.radix.remove_descendants(key)
  }

  /// Commits the transaction. After this, the edits are the trie's state.
  #[inline]
  pub fn commit(self) {
    // The working copy already lives in `self.radix`; committing is the act of
    // ending the borrow. (`ConcurrentRadix::write` performs the single publish.)
  }
}

// ----- iterators ----------------------------------------------------------

/// Iterator over references to every value in a [`Radix`], in key order.
///
/// Created by [`Radix::values`].
pub struct Values<'a, C, V, P>
where
  C: ?Sized + ToOwned,
  P: SharedPointerKind,
{
  stack: Vec<&'a Node<C, V, P>>,
}

impl<'a, C, V, P> Iterator for Values<'a, C, V, P>
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

/// Iterator over references to the values of `key`'s ancestors (inclusive).
///
/// Created by [`Radix::ancestors`].
pub struct Ancestors<'a, V> {
  values: Vec<&'a V>,
  pos: usize,
}

impl<'a, V> Iterator for Ancestors<'a, V> {
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

/// Iterator over references to the values of `key`'s strict descendants.
///
/// Created by [`Radix::descendants`].
pub struct Descendants<'a, C, V, P>
where
  C: ?Sized + ToOwned,
  P: SharedPointerKind,
{
  stack: Vec<&'a Node<C, V, P>>,
}

impl<'a, C, V, P> Iterator for Descendants<'a, C, V, P>
where
  C: ?Sized + ToOwned,
  P: SharedPointerKind,
{
  type Item = &'a V;

  fn next(&mut self) -> Option<Self::Item> {
    while let Some(node) = self.stack.pop() {
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

// ----- recursive COW algorithms -------------------------------------------

/// Inserts `key`/`value` into the subtree rooted at `node_ptr`, copying on write.
/// Returns the previous value if the exact key was already set.
fn insert_rec<C, V, P>(
  node_ptr: &mut SharedPointer<Node<C, V, P>, P>,
  key: &[C::Owned],
  value: V,
) -> Option<V>
where
  C: ?Sized + ToOwned + Ord,
  C::Owned: Clone,
  V: Clone,
  P: SharedPointerKind,
{
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
    return insert_rec(&mut node.children[i].child, &key[shared..], value);
  }

  // Split edge `i` at `shared`. The old child subtree is reused untouched.
  let old = node.children.remove(i);
  let (head, tail) = old.label.split_at(shared);
  let head: Box<[C::Owned]> = head.to_vec().into_boxed_slice();
  let tail: Box<[C::Owned]> = tail.to_vec().into_boxed_slice();

  let mut mid = Node::<C, V, P>::new();
  // Reattach the original child under the remainder of its old label.
  let old_child_edge = Edge {
    label: tail,
    child: old.child,
  };

  let key_rest = &key[shared..];
  if let Some(rest_first) = key_rest.first() {
    // The new key diverges from the old child within this edge: two children.
    let new_leaf = Edge {
      label: key_rest.to_vec().into_boxed_slice(),
      child: SharedPointer::new(Node {
        value: Some(value),
        children: Vec::new(),
      }),
    };
    // Insert both children in sorted order.
    if rest_first.borrow() < old_child_edge.label[0].borrow() {
      mid.children.push(new_leaf);
      mid.children.push(old_child_edge);
    } else {
      mid.children.push(old_child_edge);
      mid.children.push(new_leaf);
    }
  } else {
    // The new key ends exactly at the split point: value lives on `mid`.
    mid.value = Some(value);
    mid.children.push(old_child_edge);
  }

  node.children.insert(
    i,
    Edge {
      label: head,
      child: SharedPointer::new(mid),
    },
  );
  None
}

/// Removes the value at `key` from the subtree rooted at `node_ptr`, copying on
/// write and re-compressing. Returns the removed value if present.
fn remove_rec<C, V, P>(
  node_ptr: &mut SharedPointer<Node<C, V, P>, P>,
  key: &[C::Owned],
) -> Option<V>
where
  C: ?Sized + ToOwned + Ord,
  C::Owned: Clone,
  V: Clone,
  P: SharedPointerKind,
{
  let node = SharedPointer::make_mut(node_ptr);

  let Some(first) = key.first() else {
    return node.value.take();
  };

  let i = node.child_index(first.borrow()).ok()?;
  let shared = common_len::<C>(&node.children[i].label, key);
  if shared != node.children[i].label.len() {
    return None;
  }

  let removed = remove_rec(&mut node.children[i].child, &key[shared..]);
  if removed.is_some() {
    normalize_child(node, i);
  }
  removed
}

/// After a child edge has been mutated, restore canonical (path-compressed)
/// shape at index `i` of `node`: prune an emptied child, or collapse a
/// valueless single-child child by extending the edge label.
fn normalize_child<C, V, P>(node: &mut Node<C, V, P>, i: usize)
where
  C: ?Sized + ToOwned + Ord,
  C::Owned: Clone,
  V: Clone,
  P: SharedPointerKind,
{
  let child = &node.children[i].child;
  if child.value.is_none() {
    match child.children.len() {
      0 => {
        // Dead end: drop the edge entirely.
        node.children.remove(i);
      }
      1 => {
        // Redundant node: splice its single grandchild edge into this edge.
        // Cloning the grandchild edge bumps the (shared) subtree pointer O(1)
        // and copies only the label, never any value.
        let grandchild = child.children[0].clone();
        let mut label = core::mem::take(&mut node.children[i].label).into_vec();
        label.extend_from_slice(&grandchild.label);
        node.children[i].label = label.into_boxed_slice();
        node.children[i].child = grandchild.child;
      }
      _ => {}
    }
  }
}

/// Removes every strict descendant of `key` under `node_ptr`, pushing removed
/// values into `out`.
fn drain_rec<C, V, P>(
  node_ptr: &mut SharedPointer<Node<C, V, P>, P>,
  key: &[C::Owned],
  out: &mut Vec<V>,
) where
  C: ?Sized + ToOwned + Ord,
  C::Owned: Clone,
  V: Clone,
  P: SharedPointerKind,
{
  let node = SharedPointer::make_mut(node_ptr);

  let Some(first) = key.first() else {
    // `key` ends here: every child subtree is a strict descendant — drop them.
    for edge in node.children.drain(..) {
      collect_subtree(edge.child, out);
    }
    return;
  };

  let Ok(i) = node.child_index(first.borrow()) else {
    return;
  };
  let shared = common_len::<C>(&node.children[i].label, key);
  let label_len = node.children[i].label.len();

  if shared == label_len {
    // Whole label consumed: recurse, then re-canonicalize this node.
    drain_rec(&mut node.children[i].child, &key[shared..], out);
    if !out.is_empty() {
      normalize_child(node, i);
    }
  } else if shared == key.len() {
    // `key` ends mid-edge: the whole child subtree is strict descendants.
    let edge = node.children.remove(i);
    collect_subtree(edge.child, out);
  }
  // else: `key` diverges from this edge — nothing matches, no change.
}

/// Drains every value in the subtree behind `child` into `out`, moving values
/// out without cloning when the subtree node is uniquely owned.
fn collect_subtree<C, V, P>(child: SharedPointer<Node<C, V, P>, P>, out: &mut Vec<V>)
where
  C: ?Sized + ToOwned + Ord,
  C::Owned: Clone,
  V: Clone,
  P: SharedPointerKind,
{
  match SharedPointer::try_unwrap(child) {
    Ok(node) => {
      // Uniquely owned: move the value and recurse into owned children.
      if let Some(v) = node.value {
        out.push(v);
      }
      for edge in node.children {
        collect_subtree(edge.child, out);
      }
    }
    Err(shared) => {
      // Still shared with another version: clone the values rather than disturb it.
      if let Some(v) = shared.value.as_ref() {
        out.push(v.clone());
      }
      for edge in &shared.children {
        collect_subtree(edge.child.clone(), out);
      }
    }
  }
}

#[cfg(test)]
impl<C, V, P> Radix<C, V, P>
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
  pub(crate) fn edge_child(&self, first: C::Owned) -> Option<SharedPointer<Node<C, V, P>, P>>
  where
    C::Owned: Clone,
  {
    let root = self.root.as_ref()?;
    let i = root.child_index(first.borrow()).ok()?;
    Some(root.children[i].child.clone())
  }
}

#[cfg(test)]
mod tests;

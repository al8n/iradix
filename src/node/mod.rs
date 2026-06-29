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

use std::{boxed::Box, vec, vec::Vec};

use core::{borrow::Borrow, cmp::Ordering, ops::Bound};

use archery::{SharedPointer, SharedPointerKind};

#[cfg(feature = "watch")]
use core::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

#[cfg(feature = "watch")]
use event_listener::{Event, EventListener};

/// A node's change channel for the `watch` feature: an [`Event`] plus a sticky
/// `notified` flag. The flag closes the lost-wakeup race — a watcher arms a listener
/// and then re-checks the flag, while the publish-time notify sets the flag *before*
/// notifying, so a notify landing between arm and re-check is still observed by the
/// re-check. The flag is sound precisely because firing happens at *publish*, not at
/// commit: a node is fired only once the published transition replaced it, so
/// "fired ⟺ superseded" — a still-current node is never fired, and a lost-CAS tree
/// (discarded without publishing) fires nothing.
///
/// Notification is **all-or-nothing per transition**: the publish-time diff
/// ([`Root::collect_changes`]) first *collects* every changed node's slot and only
/// then fires them, so a panicking user `Ord`/`PartialEq` during the (fallible)
/// collect walk aborts it with nothing fired for that transition — never a partial
/// fire that strands the rest. Supply non-panicking key comparisons, as the mutators
/// already require. The fire step calls [`Event::notify`], which wakes registered
/// async wakers; a panicking waker (a `Waker`-contract violation) can abort a fire
/// mid-loop and is **out of scope**, like a panicking `Drop`.
#[cfg(feature = "watch")]
pub(crate) struct WatchSlot {
  event: Event,
  notified: AtomicBool,
}

#[cfg(feature = "watch")]
impl WatchSlot {
  #[inline]
  pub(crate) const fn new() -> Self {
    Self {
      event: Event::new(),
      notified: AtomicBool::new(false),
    }
  }

  /// Arms a listener, then reads the sticky flag. Returns the listener and whether
  /// the slot was *already* fired (in which case the caller must not block).
  #[inline]
  pub(crate) fn listen(&self) -> (EventListener, bool) {
    let listener = self.event.listen();
    let already = self.notified.load(AtomicOrdering::Acquire);
    (listener, already)
  }

  /// Marks the slot fired (sticky) and wakes every current listener. The store
  /// precedes the notify so a listener that armed too late still sees the flag.
  #[inline]
  pub(crate) fn fire(&self) {
    self.notified.store(true, AtomicOrdering::Release);
    self.event.notify(usize::MAX);
  }
}

/// An edge from a parent node to a child, carrying the path-compressed label.
///
/// The label lives in the parent (edge-in-parent), so splitting an edge never
/// rewrites the child subtree's stored values — it only re-labels the edge and
/// reparents the existing (shared) child.
pub(crate) struct Edge<P, C, V>
where
  P: SharedPointerKind,
{
  pub(crate) label: Box<[C]>,
  pub(crate) child: SharedPointer<Node<P, C, V>, P>,
}

// `SharedPointer::make_mut` requires the pointee to be `Clone`; a clone is taken
// only when a node is shared between versions (refcount > 1), i.e. on a real copy-
// on-write of that path.
impl<P, C, V> Clone for Edge<P, C, V>
where
  C: Clone,
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
  P: SharedPointerKind,
{
  pub(crate) value: Option<V>,
  pub(crate) children: Vec<Edge<P, C, V>>,
  /// This version's change channel. A copy-on-write clone makes a *fresh* slot (a
  /// new version is a new channel); the old node keeps its own, fired when the
  /// publish-time structural diff sees it replaced.
  #[cfg(feature = "watch")]
  pub(crate) watch: WatchSlot,
}

impl<P, C, V> Clone for Node<P, C, V>
where
  C: Clone,
  V: Clone,
  P: SharedPointerKind,
{
  #[inline]
  fn clone(&self) -> Self {
    Self {
      value: self.value.clone(),
      children: self.children.clone(),
      // A COW clone is a new version of this node, so it gets a fresh change
      // channel; the original keeps its own, fired when a publish replaces it.
      #[cfg(feature = "watch")]
      watch: WatchSlot::new(),
    }
  }
}

impl<P, C, V> Node<P, C, V>
where
  P: SharedPointerKind,
{
  #[inline]
  pub(crate) const fn new() -> Self {
    Self {
      value: None,
      children: Vec::new(),
      // `Event::new()` is `const`, so the node constructor stays `const`.
      #[cfg(feature = "watch")]
      watch: WatchSlot::new(),
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
  C: Ord,
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

  /// Iterates references to the values of every *strict* descendant of `key`, in
  /// key order.
  #[inline]
  pub(crate) fn descendant_iter<I>(&self, key: I) -> ValueIter<'_, P, C, V>
  where
    I: Iterator,
    I::Item: Borrow<C>,
  {
    // `ValueIter` pops from the back, so the seed roots (collected in ascending
    // first-component order) are reversed to make the smallest subtree pop first.
    let mut roots = self.descendant_roots(key);
    roots.reverse();
    ValueIter::from_stack(roots)
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

  /// Returns the smallest key in this subtree (component lexicographic order) and
  /// its value. A node's own value precedes all its descendants, so the minimum is
  /// found by taking each node's value if present, else descending into its
  /// smallest (first) child.
  fn minimum(&self) -> Option<(Vec<C>, &V)>
  where
    C: Clone,
  {
    let mut node = self;
    let mut key: Vec<C> = Vec::new();
    loop {
      if let Some(v) = node.value.as_ref() {
        return Some((key, v));
      }
      let edge = node.children.first()?;
      extend_key::<C>(&mut key, &edge.label);
      node = &edge.child;
    }
  }

  /// Returns the largest key in this subtree (component lexicographic order) and
  /// its value. Every descendant outranks a node's own value, so the maximum lives
  /// in the largest (last) child whenever one exists, and is a node's own value
  /// only at a childless leaf.
  fn maximum(&self) -> Option<(Vec<C>, &V)>
  where
    C: Clone,
  {
    let mut node = self;
    let mut key: Vec<C> = Vec::new();
    loop {
      match node.children.last() {
        Some(edge) => {
          extend_key::<C>(&mut key, &edge.label);
          node = &edge.child;
        }
        None => return node.value.as_ref().map(|v| (key, v)),
      }
    }
  }

  /// Iterates references to every value in this subtree, in reverse key order.
  #[inline]
  fn rev_value_iter(&self) -> RevValueIter<'_, P, C, V> {
    RevValueIter::from_nodes(std::vec![self])
  }

  /// Iterates references to the values of every *strict* descendant of `key`, in
  /// reverse key order.
  #[inline]
  fn descendant_rev_iter<I>(&self, key: I) -> RevValueIter<'_, P, C, V>
  where
    I: Iterator,
    I::Item: Borrow<C>,
  {
    RevValueIter::from_nodes(self.descendant_roots(key))
  }

  /// Builds an ascending cursor over the entries whose key lies within
  /// `[lower, upper]` (each end honoring its [`Bound`] kind). The lower bound is
  /// resolved eagerly here (descending the trie and seeding only the subtrees at
  /// or past it); the upper bound is checked lazily as the cursor advances.
  fn range_iter(&self, lower: Bound<Vec<C>>, upper: Bound<Vec<C>>) -> RangeIter<'_, P, C, V>
  where
    C: Clone,
  {
    let mut stack = Vec::new();
    match &lower {
      Bound::Unbounded => stack.push(RangeFrame::Node {
        node: self,
        key: Vec::new(),
      }),
      Bound::Included(lb) => seed_lower(&mut stack, self, Vec::new(), lb, true),
      Bound::Excluded(lb) => seed_lower(&mut stack, self, Vec::new(), lb, false),
    }
    RangeIter {
      stack,
      upper,
      done: false,
    }
  }
}

impl<P, C, V> Node<P, C, V>
where
  C: Ord + Clone,
  V: Clone,
  P: SharedPointerKind,
{
  /// Inserts `value` at the components yielded by `key` in the subtree rooted at
  /// `node_ptr`, copying on write. Returns the previous value if the exact key was
  /// already set.
  ///
  /// `key` is consumed lazily (mirroring the read path): only the components
  /// actually STORED — the new leaf's or split-suffix label — are cloned, so no
  /// whole-key `Vec` is materialized.
  pub(crate) fn insert<I>(
    node_ptr: &mut SharedPointer<Node<P, C, V>, P>,
    key: &mut core::iter::Peekable<I>,
    value: V,
  ) -> Option<V>
  where
    I: Iterator,
    I::Item: Borrow<C>,
  {
    let node = SharedPointer::make_mut(node_ptr);

    if key.peek().is_none() {
      return node.value.replace(value);
    }

    let first = key.peek().expect("key has a next component").borrow();
    let i = match node.child_index(first) {
      Err(insert_at) => {
        // No edge begins with `first`: the new leaf's label is the WHOLE remaining
        // key. Consuming the iterator yields the peeked `first` too.
        let label: Box<[C]> = key
          .map(|c| c.borrow().clone())
          .collect::<Vec<C>>()
          .into_boxed_slice();
        let leaf = Node {
          value: Some(value),
          children: Vec::new(),
          #[cfg(feature = "watch")]
          watch: WatchSlot::new(),
        };
        let edge = Edge {
          label,
          child: SharedPointer::new(leaf),
        };
        node.children.insert(insert_at, edge);
        return None;
      }
      Ok(i) => i,
    };

    let shared = match_prefix::<C, _>(&node.children[i].label, key);
    let label_len = node.children[i].label.len();

    if shared == label_len {
      // The whole edge label is consumed; descend into the child with the same
      // (now-advanced) key iterator.
      return Node::insert(&mut node.children[i].child, key, value);
    }

    // Split edge `i` at `shared`. Build the COMPLETE replacement `mid` subtree —
    // its label clones, the reused old child (an O(1) pointer clone), the new
    // leaf, and the sorted child order — while edge `i` is still installed. Every
    // fallible step (collecting `key_rest`, the `head`/`tail` clones, the leaf
    // allocation, the user `Ord` that orders the two children) therefore runs
    // before anything is detached, so an unwind leaves the trie and `len`
    // untouched. Only the final single move splices `mid` in, dropping the old
    // edge (whose child was already cloned, so no subtree is lost).
    let key_rest: Vec<C> = key.map(|c| c.borrow().clone()).collect();
    let (head, tail) = node.children[i].label.split_at(shared);
    let head: Box<[C]> = head.to_vec().into_boxed_slice();
    let tail: Box<[C]> = tail.to_vec().into_boxed_slice();
    let old_child_edge = Edge {
      label: tail,
      child: node.children[i].child.clone(),
    };

    let mid_node = if key_rest.is_empty() {
      // The new key ends exactly at the split point: value lives on `mid`.
      Node {
        value: Some(value),
        children: vec![old_child_edge],
        #[cfg(feature = "watch")]
        watch: WatchSlot::new(),
      }
    } else {
      // The new key diverges from the old child within this edge: two children in
      // sorted order. The ordering `Ord` is the last fallible step before splicing.
      let new_leaf = Edge {
        label: key_rest.into_boxed_slice(),
        child: SharedPointer::new(Node {
          value: Some(value),
          children: Vec::new(),
          #[cfg(feature = "watch")]
          watch: WatchSlot::new(),
        }),
      };
      if Borrow::<C>::borrow(&new_leaf.label[0]) < Borrow::<C>::borrow(&old_child_edge.label[0]) {
        Node {
          value: None,
          children: vec![new_leaf, old_child_edge],
          #[cfg(feature = "watch")]
          watch: WatchSlot::new(),
        }
      } else {
        Node {
          value: None,
          children: vec![old_child_edge, new_leaf],
          #[cfg(feature = "watch")]
          watch: WatchSlot::new(),
        }
      }
    };

    node.children[i] = Edge {
      label: head,
      child: SharedPointer::new(mid_node),
    };
    None
  }

  /// Removes the value at the components yielded by `key` from the subtree rooted
  /// at `node_ptr`, copying on write and re-compressing. Returns the removed value
  /// if present.
  ///
  /// `key` is consumed lazily (mirroring [`insert`](Node::insert) and the read
  /// path): each matched edge label is walked through the same `Peekable`, so no
  /// whole-key `Vec` is materialized.
  pub(crate) fn remove<I>(
    node_ptr: &mut SharedPointer<Node<P, C, V>, P>,
    key: &mut core::iter::Peekable<I>,
    len: &mut usize,
  ) -> Option<V>
  where
    I: Iterator,
    I::Item: Borrow<C>,
  {
    let node = SharedPointer::make_mut(node_ptr);

    if key.peek().is_none() {
      // The value `take` is the single infallible commit point: decrement `len`
      // here, after the fallible make-mut/traversal above has already succeeded
      // and before any (fallible) re-compression on the way back up. An unwind
      // before this point never reaches it, so `len` stays accurate.
      let removed = node.value.take();
      if removed.is_some() {
        *len -= 1;
      }
      return removed;
    }

    let first = key.peek().expect("key has a next component").borrow();
    let i = node.child_index(first).ok()?;
    let shared = match_prefix::<C, _>(&node.children[i].label, key);
    if shared != node.children[i].label.len() {
      // The key diverges within this edge (or runs out mid-edge): no exact match
      // below, so nothing to remove.
      return None;
    }

    let removed = Node::remove(&mut node.children[i].child, key, len);
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
  pub(crate) fn unlink_descendants<I>(
    node_ptr: &mut SharedPointer<Node<P, C, V>, P>,
    key: &mut core::iter::Peekable<I>,
    len: &mut usize,
  ) -> usize
  where
    I: Iterator,
    I::Item: Borrow<C>,
  {
    let node = SharedPointer::make_mut(node_ptr);

    if key.peek().is_none() {
      // `key` ends here: every child subtree is a strict descendant — drop them.
      // Counting and clearing are infallible, so `len` is corrected atomically
      // with the unlink (after the fallible make-mut above).
      let removed: usize = node.children.iter().map(|edge| edge.child.count()).sum();
      *len -= removed;
      node.children.clear();
      return removed;
    }

    let first = key.peek().expect("key has a next component").borrow();
    let Ok(i) = node.child_index(first) else {
      return 0;
    };
    let shared = match_prefix::<C, _>(&node.children[i].label, key);
    let label_len = node.children[i].label.len();

    if shared == label_len {
      // Whole label consumed: recurse (which unlinks and corrects `len` deeper),
      // then re-canonicalize this node — the (fallible) normalize runs only after
      // the deeper unlink already adjusted `len`.
      let removed = Node::unlink_descendants(&mut node.children[i].child, key, len);
      if removed > 0 {
        normalize_child(node, i);
      }
      removed
    } else if key.peek().is_none() {
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

  /// Unlinks the value at `key` *and* every strict descendant (the whole subtree
  /// rooted at `key`), copying on write and re-compressing. Returns the number of
  /// values removed and decrements `*len` by that amount.
  ///
  /// This is the node-inclusive counterpart to [`unlink_descendants`]: it also
  /// drops the value stored exactly at `key`, and like it, drops the doomed subtree
  /// by unlinking its edge from the (copied) parent — never `make_mut`-ing the
  /// subtree, so no *removed* value is cloned. `key` must be non-empty (the
  /// empty-prefix whole-trie case is handled by [`Root`] dropping its root). The
  /// same panic-safety contract holds — `len` is corrected atomically with the
  /// (infallible) unlink, after the fallible make-mut/traversal and before the
  /// (fallible, but label-MOVING and therefore user-panic-free) `normalize_child`
  /// on the way up.
  ///
  /// [`unlink_descendants`]: Node::unlink_descendants
  pub(crate) fn unlink_prefix<I>(
    node_ptr: &mut SharedPointer<Node<P, C, V>, P>,
    key: &mut core::iter::Peekable<I>,
    len: &mut usize,
  ) -> usize
  where
    I: Iterator,
    I::Item: Borrow<C>,
  {
    // PRECONDITION: `key` is non-empty. The whole-trie (empty-prefix) case is
    // handled by the `Root` wrapper dropping its root pointer, so we never
    // `make_mut` a node we are about to clear — which would clone the very values
    // being deleted. Here `make_mut` only copies ANCESTORS on the path to `key`
    // (ordinary copy-on-write, shared by every mutator); the doomed subtree itself
    // is dropped by unlinking its edge, never cloned.
    let node = SharedPointer::make_mut(node_ptr);
    let first = key
      .peek()
      .expect("unlink_prefix requires a non-empty key")
      .borrow();

    let Ok(i) = node.child_index(first) else {
      return 0;
    };
    let shared = match_prefix::<C, _>(&node.children[i].label, key);
    let label_len = node.children[i].label.len();

    if key.peek().is_none() {
      // `key` ends at or within this edge: the whole child subtree is at or below
      // `key`. Unlink the edge from this already-copied parent — that drops the
      // value at `key` and every descendant via the pointer, with NO `make_mut` on
      // the doomed subtree, so none of the removed values is cloned. `count` is
      // inclusive (node + descendants), so `len` is corrected atomically with the
      // (infallible) removal. Checked before the descend case so a `key` ending
      // exactly at the child node boundary unlinks here rather than recursing into
      // (and copying) the target.
      let removed = node.children[i].child.count();
      *len -= removed;
      node.children.remove(i);
      removed
    } else if shared == label_len {
      // Whole label consumed and `key` continues: recurse, then re-canonicalize
      // this node (which may now have a pruned-empty or single-child child).
      let removed = Node::unlink_prefix(&mut node.children[i].child, key, len);
      if removed > 0 {
        normalize_child(node, i);
      }
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

  /// Returns `true` if the physical root node is absent (the `None`-root state the
  /// `watch` empty-position slot keys off). Distinct from [`is_empty`](Root::is_empty):
  /// the root node can exist with `len == 0` only transiently mid-mutation, but
  /// `watch_slot` returns `None` exactly when this is `true`.
  #[cfg(feature = "watch")]
  #[inline]
  pub(crate) const fn root_is_none(&self) -> bool {
    self.root.is_none()
  }

  /// Removes every value, resetting the trie to empty.
  #[inline]
  pub(crate) fn clear(&mut self) {
    self.root = None;
    self.len = 0;
  }

  /// Canonicalizes a *logically* empty trie (`len == 0`) to the *physically* empty
  /// `None`-root state, so "empty ⟺ root is None" holds. A net-empty mutation (e.g.
  /// insert-then-remove the same key from an empty base) can leave `root = Some(empty
  /// node)` with `len == 0`; without this, the `watch` diff would read that as a
  /// None -> Some transition and spuriously wake empty-position watchers. Called by
  /// the `watch` `commit` before computing the per-epoch empty slot.
  #[cfg(feature = "watch")]
  #[inline]
  pub(crate) fn canonicalize_empty(&mut self) {
    if self.len == 0 {
      self.root = None;
    }
  }
}

impl<P, C, V> Root<P, C, V>
where
  C: Ord,
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

  /// Returns the smallest key (component lexicographic order) and its value.
  #[inline]
  pub(crate) fn minimum(&self) -> Option<(Vec<C>, &V)>
  where
    C: Clone,
  {
    self.root.as_ref()?.minimum()
  }

  /// Returns the largest key (component lexicographic order) and its value.
  #[inline]
  pub(crate) fn maximum(&self) -> Option<(Vec<C>, &V)>
  where
    C: Clone,
  {
    self.root.as_ref()?.maximum()
  }

  /// Iterates references to every value in the trie, in reverse key order.
  #[inline]
  pub(crate) fn values_rev(&self) -> RevValueIter<'_, P, C, V> {
    match self.root.as_ref() {
      Some(root) => root.rev_value_iter(),
      None => RevValueIter::empty(),
    }
  }

  /// Iterates references to the values of `key`'s strict descendants, in reverse
  /// key order.
  #[inline]
  pub(crate) fn descendants_rev<I>(&self, key: I) -> RevValueIter<'_, P, C, V>
  where
    I: Iterator,
    I::Item: Borrow<C>,
  {
    match self.root.as_ref() {
      Some(root) => root.descendant_rev_iter(key),
      None => RevValueIter::empty(),
    }
  }

  /// Iterates `(key, value)` for every entry whose key lies within
  /// `[lower, upper]`, in ascending key order.
  #[inline]
  pub(crate) fn range(&self, lower: Bound<Vec<C>>, upper: Bound<Vec<C>>) -> RangeIter<'_, P, C, V>
  where
    C: Clone,
  {
    match self.root.as_ref() {
      Some(root) => root.range_iter(lower, upper),
      None => RangeIter::empty(),
    }
  }
}

impl<P, C, V> Root<P, C, V>
where
  C: Ord + Clone,
  V: Clone,
  P: SharedPointerKind,
{
  /// Inserts `value` at the components yielded by `key`, returning the previous
  /// value if the key was set. `key` is consumed lazily; only stored components are
  /// cloned (no whole-key `Vec`).
  pub(crate) fn insert<I>(&mut self, key: I, value: V) -> Option<V>
  where
    I: Iterator,
    I::Item: Borrow<C>,
  {
    let mut key = key.peekable();
    let root = self
      .root
      .get_or_insert_with(|| SharedPointer::new(Node::new()));
    let old = Node::insert(root, &mut key, value);
    if old.is_none() {
      self.len += 1;
    }
    old
  }

  /// Removes and returns the value at exactly the components yielded by
  /// `make_key`, if any.
  ///
  /// `make_key` is a re-iterable key source called once per pass: a read-only
  /// existence pass first, then (only when present) the mutate pass. Re-yielding a
  /// fresh iterator per pass keeps the no-copy-on-absent guarantee while avoiding a
  /// whole-key `Vec` — both passes walk the key lazily. Correctness relies on the
  /// [`RadixKey`](crate::RadixKey) determinism contract (each call yields the same
  /// components); the public wrappers pass `|| key.components()`.
  pub(crate) fn remove<F, I>(&mut self, make_key: F) -> Option<V>
  where
    F: Fn() -> I,
    I: Iterator,
    I::Item: Borrow<C>,
  {
    // A read-only existence check first: a remove of an absent key must not
    // copy-on-write (and so must not disturb structural sharing). When the key
    // is present, every node on its root-to-value path genuinely changes, so the
    // eager copy-on-write in `Node::remove` is justified.
    self.get(make_key())?;
    let root = self.root.as_mut()?;
    // `Node::remove` decrements `len` at the value-`take` itself — after its
    // fallible make-mut/traversal succeeds — so a panic in a shared-node clone or
    // a user comparison on the way down leaves `len` and the trie consistent.
    Node::remove(root, &mut make_key().peekable(), &mut self.len)
  }

  /// Removes every *strict* descendant of the components yielded by `make_key`
  /// (the value at the key, if any, is kept), returning the number of values
  /// removed. Never clones a `V`. Two-pass over `make_key` (existence, then
  /// unlink); see [`Root::remove`] for the contract.
  pub(crate) fn remove_descendants<F, I>(&mut self, make_key: F) -> usize
  where
    F: Fn() -> I,
    I: Iterator,
    I::Item: Borrow<C>,
  {
    // Read-only existence check: nothing to remove means no copy-on-write (so
    // structural sharing is preserved) and no `len` change. This traversal is
    // fallible (user comparisons) but mutates nothing, so a panic is harmless.
    if self.descendants(make_key()).next().is_none() {
      return 0;
    }
    // `unlink_descendants` counts and unlinks the strict descendants, correcting
    // `len` atomically with the (infallible) unlink — and never cloning a `V`.
    match self.root.as_mut() {
      Some(root) => Node::unlink_descendants(root, &mut make_key().peekable(), &mut self.len),
      None => 0,
    }
  }

  /// Removes every *strict* descendant of the components yielded by `make_key` and
  /// returns their values (the value at the key, if any, is kept). Clones values
  /// out before unlinking. Two-pass over `make_key` (capture, then unlink); see
  /// [`Root::remove`] for the contract.
  pub(crate) fn drain_descendants<F, I>(&mut self, make_key: F) -> Vec<V>
  where
    F: Fn() -> I,
    I: Iterator,
    I::Item: Borrow<C>,
  {
    // Phase 1 (read-only, fallible): clone every strict-descendant value out
    // FIRST, before unlinking anything. A `V::clone` panic here unwinds with the
    // trie and `len` completely untouched. This also doubles as the
    // nothing-to-drain check: an empty result means no copy-on-write (preserving
    // structural sharing).
    let out: Vec<V> = self.descendants(make_key()).cloned().collect();
    if out.is_empty() {
      return out;
    }
    // Phase 2: the values are safely captured, so commit the structural change.
    // `unlink_descendants` corrects `len` atomically with the (infallible) unlink.
    if let Some(root) = self.root.as_mut() {
      Node::unlink_descendants(root, &mut make_key().peekable(), &mut self.len);
    }
    out
  }

  /// Removes the value at the components yielded by `make_key` *and* every strict
  /// descendant (node-inclusive), returning the number of values removed. Clones no
  /// *removed* value — only the copy-on-write path to the key is duplicated, exactly
  /// like every other mutator. Two-pass over `make_key` (existence, then unlink);
  /// see [`Root::remove`] for the contract.
  pub(crate) fn delete_prefix<F, I>(&mut self, make_key: F) -> usize
  where
    F: Fn() -> I,
    I: Iterator,
    I::Item: Borrow<C>,
  {
    if make_key().next().is_none() {
      // Whole-trie delete: drop the root pointer outright. No `make_mut`, so not
      // even the root's own path is copied and no value is cloned.
      let removed = self.len;
      self.root = None;
      self.len = 0;
      return removed;
    }
    // Read-only existence check: if nothing is stored at or below `key`, there is
    // nothing to remove, so skip the copy-on-write entirely (preserving structural
    // sharing) and leave `len` alone. The traversal is fallible (user comparisons)
    // but mutates nothing, so a panic here is harmless.
    let nothing_here =
      self.get(make_key()).is_none() && self.descendants(make_key()).next().is_none();
    if nothing_here {
      return 0;
    }
    // `unlink_prefix` counts and unlinks the whole subtree at `key`, correcting
    // `len` atomically with the (infallible) unlink — unlinking the edge rather than
    // copying the doomed subtree, so no removed value is cloned.
    match self.root.as_mut() {
      Some(root) => Node::unlink_prefix(root, &mut make_key().peekable(), &mut self.len),
      None => 0,
    }
  }

  /// Removes the value at the components yielded by `make_key` *and* every strict
  /// descendant (node-inclusive) and returns their values in ascending key order
  /// (the value at the key itself, if any, first). Clones values out before
  /// unlinking. Two-pass over `make_key` (capture, then unlink); see
  /// [`Root::remove`] for the contract.
  pub(crate) fn drain_prefix<F, I>(&mut self, make_key: F) -> Vec<V>
  where
    F: Fn() -> I,
    I: Iterator,
    I::Item: Borrow<C>,
  {
    if make_key().next().is_none() {
      // Whole-trie drain: capture every value (the key is empty, so no key walk is
      // needed), then drop the root pointer outright — no `make_mut`, no re-clone.
      let out: Vec<V> = self.values().cloned().collect();
      self.root = None;
      self.len = 0;
      return out;
    }
    // Phase 1 (read-only, fallible): clone the value at `key` (which sorts before
    // all descendants) then every strict-descendant value, in ascending key order,
    // BEFORE unlinking anything. A `V::clone` panic here unwinds with the trie and
    // `len` completely untouched. The emptiness check also doubles as the
    // nothing-to-drain guard: an empty result means no copy-on-write.
    let mut out: Vec<V> = Vec::new();
    if let Some(v) = self.get(make_key()) {
      out.push(v.clone());
    }
    out.extend(self.descendants(make_key()).cloned());
    if out.is_empty() {
      return out;
    }
    // Phase 2: the values are safely captured, so commit the structural change.
    // `unlink_prefix` unlinks the doomed edge rather than copying the subtree, so it
    // never re-clones the values phase 1 already captured.
    if let Some(root) = self.root.as_mut() {
      Node::unlink_prefix(root, &mut make_key().peekable(), &mut self.len);
    }
    out
  }
}

#[cfg(test)]
impl<P, C, V> Root<P, C, V>
where
  C: Ord,
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
  P: SharedPointerKind,
{
  stack: Vec<&'a Node<P, C, V>>,
}

impl<'a, P, C, V> ValueIter<'a, P, C, V>
where
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
  C: Clone,
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
      // not unwind on user code: it performs NO `C`/`V` clone and no `Ord`.
      // The lone allocation (the merged label) can only fail with OOM, which aborts
      // rather than unwinds (see the crate panic-safety docs), so the in-flight
      // return value is never dropped by an unwind here.
      let merged_len =
        node.children[i].label.len() + node.children[i].child.children[0].label.len();
      let mut merged: Vec<C> = Vec::with_capacity(merged_len);
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
pub(crate) fn match_prefix<C, I>(label: &[C], key: &mut core::iter::Peekable<I>) -> usize
where
  C: PartialEq,
  I: Iterator,
  I::Item: Borrow<C>,
{
  let mut shared = 0;
  for c in label {
    match key.peek() {
      Some(item) if c == item.borrow() => {
        shared += 1;
        key.next();
      }
      _ => break,
    }
  }
  shared
}

/// Length of the longest common prefix of two component slices.
pub(crate) fn common_len<C>(a: &[C], b: &[C]) -> usize
where
  C: PartialEq,
{
  a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

/// Appends a clone of each of `labels` to `dst`.
fn extend_key<C>(dst: &mut Vec<C>, labels: &[C])
where
  C: Clone,
{
  dst.extend(labels.iter().cloned());
}

/// Clones a component slice into a fresh key vector.
fn dup_key<C>(key: &[C]) -> Vec<C>
where
  C: Clone,
{
  key.to_vec()
}

#[cfg(feature = "watch")]
impl<P, C, V> Node<P, C, V>
where
  C: Ord,
  P: SharedPointerKind,
{
  /// Returns the change slot of the deepest node reached by descending `key`: the
  /// node at `key`, or — when `key` is absent or ends mid-edge — the deepest
  /// existing node on its path. Because copy-on-write path-copies every ancestor of
  /// a change, listening on this slot fires on any change to `key` or anything
  /// beneath it.
  pub(crate) fn watch_slot<I>(&self, key: I) -> &WatchSlot
  where
    I: Iterator,
    I::Item: Borrow<C>,
  {
    let mut node = self;
    let mut key = key.peekable();
    loop {
      let Some(first) = key.peek() else {
        return &node.watch;
      };
      let Ok(i) = node.child_index(first.borrow()) else {
        return &node.watch;
      };
      let edge = &node.children[i];
      let shared = match_prefix::<C, _>(&edge.label, &mut key);
      if shared == edge.label.len() {
        node = &edge.child;
      } else {
        return &node.watch;
      }
    }
  }
}

/// Collects the change slot of every node in a subtree (a whole base subtree the
/// published version removed) into `out`. Pushes only; never fires (the caller fires
/// after the whole — fallible — walk, so notification is all-or-nothing).
///
/// Iterative (explicit `Vec` stack), so depth is heap-bounded, not call-stack-bounded
/// — a deep removed chain cannot overflow the stack during notification, matching the
/// crate's value/range iterators.
#[cfg(feature = "watch")]
fn collect_subtree<'a, P, C, V>(
  node: &'a SharedPointer<Node<P, C, V>, P>,
  out: &mut Vec<&'a WatchSlot>,
) where
  P: SharedPointerKind,
{
  let mut stack = vec![node];
  while let Some(n) = stack.pop() {
    out.push(&n.watch);
    for edge in &n.children {
      stack.push(&edge.child);
    }
  }
}

/// A pending position in the changed-node diff, driven by an explicit work stack so
/// the walk's depth is heap-bounded (no recursion on trie node-depth). Each variant
/// borrows `'a` from the *base* tree, exactly as the collected `&'a WatchSlot`s do.
#[cfg(feature = "watch")]
enum Work<'a, P, C, V>
where
  P: SharedPointerKind,
{
  /// A base node and the work node at the *same* key position, to diff.
  Pair(
    &'a SharedPointer<Node<P, C, V>, P>,
    &'a SharedPointer<Node<P, C, V>, P>,
  ),
  /// A base node that lands mid-edge in the work tree (merged away): `(base, suf, w)`
  /// where the work continues with `suf` to node `w`.
  MidEdge(
    &'a SharedPointer<Node<P, C, V>, P>,
    &'a [C],
    &'a SharedPointer<Node<P, C, V>, P>,
  ),
  /// A whole base subtree the published version removed.
  Subtree(&'a SharedPointer<Node<P, C, V>, P>),
}

/// Collects the change slot of every *base* node the published version replaced or
/// removed, found by a pointer-identity diff of `base` against `work` (the new tree).
/// `base` and `work` are the nodes at the *same* key position. Pointer-equal
/// subtrees are shared verbatim and pruned (never descended into); the walk visits
/// each replaced node and scans its direct children, so it scales with the changed
/// paths and their siblings — not the whole tree (so it is *not* `O(changed nodes)`
/// when a replaced node has high fanout). The
/// collected slots are BASE nodes, so they borrow `base`; the caller fires them only
/// after this (fallible) walk returns, keeping notification all-or-nothing.
///
/// Iterative (explicit `Work` stack): every position that would recurse instead
/// pushes a `Work` item, so a deep changed path is heap-bounded and cannot overflow
/// the call stack during notification.
#[cfg(feature = "watch")]
pub(crate) fn collect_changed<'a, P, C, V>(
  base: &'a SharedPointer<Node<P, C, V>, P>,
  work: &'a SharedPointer<Node<P, C, V>, P>,
  out: &mut Vec<&'a WatchSlot>,
) where
  C: Ord,
  P: SharedPointerKind,
{
  let mut stack = vec![Work::Pair(base, work)];
  drive(&mut stack, out);
}

/// Drains a seeded `Work` stack, collecting every changed base node's slot into
/// `out`. The single owner of the diff's control flow: each match arm pushes follow-up
/// `Work` rather than recursing, so the only growth is the heap `stack`.
#[cfg(feature = "watch")]
fn drive<'a, P, C, V>(stack: &mut Vec<Work<'a, P, C, V>>, out: &mut Vec<&'a WatchSlot>)
where
  C: Ord,
  P: SharedPointerKind,
{
  while let Some(item) = stack.pop() {
    match item {
      Work::Pair(base, work) => {
        if SharedPointer::ptr_eq(base, work) {
          continue;
        }
        out.push(&base.watch);
        for be in &base.children {
          if let Some(next) = locate(work, &be.label, &be.child) {
            stack.push(next);
          }
        }
      }
      Work::MidEdge(base, suf, w) => {
        out.push(&base.watch);
        for be in &base.children {
          if let Some(next) = locate_edge(suf, w, &be.label, &be.child) {
            stack.push(next);
          }
        }
      }
      Work::Subtree(node) => {
        out.push(&node.watch);
        for edge in &node.children {
          stack.push(Work::Subtree(&edge.child));
        }
      }
    }
  }
}

/// Computes where base `child` (reached from its parent by `label`) sits under work
/// node `w`, returning the follow-up [`Work`] (or `None` when the position is shared
/// verbatim, i.e. nothing to collect). Handles edge *splits* (`label` spans several
/// shorter work edges) and *merges* (`label` ends mid-edge inside a longer work edge
/// — the base node there was merged away but its subtree may live on past the split
/// point). The internal `loop` walks work edges within the hop; it never recurses.
#[cfg(feature = "watch")]
fn locate<'a, P, C, V>(
  w: &'a SharedPointer<Node<P, C, V>, P>,
  label: &'a [C],
  child: &'a SharedPointer<Node<P, C, V>, P>,
) -> Option<Work<'a, P, C, V>>
where
  C: Ord,
  P: SharedPointerKind,
{
  let mut cur = w;
  let mut rest = label;
  loop {
    if rest.is_empty() {
      // `child` sits exactly at `cur`.
      return (!SharedPointer::ptr_eq(child, cur)).then_some(Work::Pair(child, cur));
    }
    let Ok(i) = cur.child_index(&rest[0]) else {
      return Some(Work::Subtree(child));
    };
    let edge = &cur.children[i];
    let common = common_len(rest, &edge.label);
    if common == edge.label.len() {
      // Consumed the whole work edge; keep descending.
      rest = &rest[common..];
      cur = &edge.child;
    } else if common == rest.len() {
      // `rest` is a prefix of this longer work edge (a merge): `child` lands
      // mid-edge, so the node itself is gone; continue against the edge's suffix.
      return Some(Work::MidEdge(child, &edge.label[common..], &edge.child));
    } else {
      return Some(Work::Subtree(child));
    }
  }
}

/// Computes where base `child` (reached by `label`) sits against a virtual work edge
/// `suf` -> `w` — a position partway down a merged work edge — returning the follow-up
/// [`Work`]. Bounded by the edge-label lengths of a single hop; it never recurses.
#[cfg(feature = "watch")]
fn locate_edge<'a, P, C, V>(
  suf: &'a [C],
  w: &'a SharedPointer<Node<P, C, V>, P>,
  label: &'a [C],
  child: &'a SharedPointer<Node<P, C, V>, P>,
) -> Option<Work<'a, P, C, V>>
where
  C: Ord,
  P: SharedPointerKind,
{
  let common = common_len(label, suf);
  if common == suf.len() {
    // `suf` consumed; the rest of `label` descends from `w`.
    locate(w, &label[common..], child)
  } else if common == label.len() {
    // `label` is a prefix of `suf`: `child` lands still further mid-edge.
    Some(Work::MidEdge(child, &suf[common..], w))
  } else {
    Some(Work::Subtree(child))
  }
}

#[cfg(feature = "watch")]
impl<P, C, V> Root<P, C, V>
where
  C: Ord,
  P: SharedPointerKind,
{
  /// The change slot to listen on for `key`, or `None` only when the trie is empty
  /// (no root node exists; the caller supplies its own empty-position slot). See
  /// [`Node::watch_slot`].
  pub(crate) fn watch_slot<I>(&self, key: I) -> Option<&WatchSlot>
  where
    I: Iterator,
    I::Item: Borrow<C>,
  {
    self.root.as_ref().map(|root| root.watch_slot(key))
  }

  /// Collects the change slot of every node a newly published version replaced or
  /// removed (found by a pointer-identity diff) into `out`, WITHOUT firing — so the
  /// caller can fire all-or-nothing after this (fallible) walk returns. `self` is the
  /// newly published tree and `base` the version the producing transaction was opened
  /// from. Returns `true` iff the trie went from empty to non-empty, signalling the
  /// caller to fire its empty-position slot.
  ///
  /// The collected slots are BASE nodes, so they borrow `base` (alive for the call);
  /// the caller fires them after the walk. Call only after `self` is the live version
  /// (the public [`crate::sync::Radix::notify_changes_since`] enforces this);
  /// committing alone must not fire, so a lost-CAS tree is discarded without ever
  /// calling this. The walk runs user `C` comparisons (`child_index`/`common_len`),
  /// so a panicking `Ord`/`PartialEq` aborts it with `out` dropped — firing nothing
  /// for that transition, matching the crate's strong-exception-guarantee style.
  pub(crate) fn collect_changes<'a>(
    &'a self,
    base: &'a Self,
    out: &mut Vec<&'a WatchSlot>,
  ) -> bool {
    match (base.root.as_ref(), self.root.as_ref()) {
      (None, None) => false,
      (None, Some(_)) => true,
      (Some(b), None) => {
        collect_subtree(b, out);
        false
      }
      (Some(b), Some(w)) => {
        if !SharedPointer::ptr_eq(b, w) {
          collect_changed(b, w, out);
        }
        false
      }
    }
  }
}

/// A frame in [`RevValueIter`]'s explicit DFS stack: either a subtree still to be
/// expanded, or a value ready to be yielded once its descendants are exhausted.
enum RevFrame<'a, P, C, V>
where
  P: SharedPointerKind,
{
  Node(&'a Node<P, C, V>),
  Value(&'a V),
}

/// Depth-first iterator over references to every value in a forest of subtrees, in
/// *reverse* key order (mirror of [`ValueIter`]). A node's own value sorts before
/// all its descendants, so descending order visits the larger children first and
/// the node's own value last.
pub(crate) struct RevValueIter<'a, P, C, V>
where
  P: SharedPointerKind,
{
  stack: Vec<RevFrame<'a, P, C, V>>,
}

impl<'a, P, C, V> RevValueIter<'a, P, C, V>
where
  P: SharedPointerKind,
{
  #[inline]
  pub(crate) const fn empty() -> Self {
    Self { stack: Vec::new() }
  }

  /// Seeds the stack from a forest of subtree roots given in *ascending* key
  /// order; iteration then yields them in descending order (the last/largest root
  /// is processed first).
  #[inline]
  fn from_nodes(nodes: Vec<&'a Node<P, C, V>>) -> Self {
    Self {
      stack: nodes.into_iter().map(RevFrame::Node).collect(),
    }
  }
}

impl<'a, P, C, V> Iterator for RevValueIter<'a, P, C, V>
where
  P: SharedPointerKind,
{
  type Item = &'a V;

  fn next(&mut self) -> Option<Self::Item> {
    while let Some(frame) = self.stack.pop() {
      match frame {
        RevFrame::Node(node) => {
          // Push the value first (deepest), so it is yielded only after every
          // child; push children in ascending order so the largest lands on top
          // and is visited first (descending key order).
          if let Some(v) = node.value.as_ref() {
            self.stack.push(RevFrame::Value(v));
          }
          for edge in &node.children {
            self.stack.push(RevFrame::Node(&edge.child));
          }
        }
        RevFrame::Value(v) => return Some(v),
      }
    }
    None
  }
}

/// A frame in [`RangeIter`]'s explicit DFS stack. Each frame carries the
/// reconstructed key prefix from the root, so a yielded entry's full key is built
/// without a second traversal.
enum RangeFrame<'a, P, C, V>
where
  P: SharedPointerKind,
{
  Node {
    node: &'a Node<P, C, V>,
    key: Vec<C>,
  },
  Yield {
    key: Vec<C>,
    value: &'a V,
  },
}

/// Forward cursor over `(key, value)` entries within a range, in ascending key
/// order. Borrows the shared node graph (`&V`, no value clones); each yielded key
/// is reconstructed by concatenating the root-to-node edge labels.
pub(crate) struct RangeIter<'a, P, C, V>
where
  P: SharedPointerKind,
{
  stack: Vec<RangeFrame<'a, P, C, V>>,
  upper: Bound<Vec<C>>,
  done: bool,
}

impl<P, C, V> RangeIter<'_, P, C, V>
where
  P: SharedPointerKind,
{
  #[inline]
  pub(crate) const fn empty() -> Self {
    Self {
      stack: Vec::new(),
      upper: Bound::Unbounded,
      done: true,
    }
  }
}

impl<'a, P, C, V> Iterator for RangeIter<'a, P, C, V>
where
  C: Ord + Clone,
  P: SharedPointerKind,
{
  type Item = (Vec<C>, &'a V);

  fn next(&mut self) -> Option<Self::Item> {
    if self.done {
      return None;
    }
    while let Some(frame) = self.stack.pop() {
      match frame {
        RangeFrame::Node { node, key } => {
          // Push children in reverse so the smallest is visited first (ascending),
          // then the node's own value on top so it precedes its descendants.
          for edge in node.children.iter().rev() {
            let mut child_key = dup_key::<C>(&key);
            extend_key::<C>(&mut child_key, &edge.label);
            self.stack.push(RangeFrame::Node {
              node: &edge.child,
              key: child_key,
            });
          }
          if let Some(value) = node.value.as_ref() {
            self.stack.push(RangeFrame::Yield { key, value });
          }
        }
        RangeFrame::Yield { key, value } => {
          // Entries arrive in ascending order, so the first key past the upper
          // bound ends iteration: nothing remaining can be within range.
          let within = match &self.upper {
            Bound::Unbounded => true,
            Bound::Included(ub) => key.cmp(ub) != Ordering::Greater,
            Bound::Excluded(ub) => key.cmp(ub) == Ordering::Less,
          };
          if within {
            return Some((key, value));
          }
          self.done = true;
          self.stack.clear();
          return None;
        }
      }
    }
    self.done = true;
    None
  }
}

/// Seeds `stack` (for ascending iteration) with exactly the entries of `node`'s
/// subtree whose full key is `>= lower` (`included`) or `> lower` (otherwise),
/// where `lower == key ++ suffix`. `key` is the already-reconstructed prefix from
/// the root to `node`.
///
/// The frames are pushed so the normal ascending expansion yields them in order:
/// the matching child's seeded frames sit above (are popped before) the wholly
/// greater right siblings.
fn seed_lower<'a, P, C, V>(
  stack: &mut Vec<RangeFrame<'a, P, C, V>>,
  node: &'a Node<P, C, V>,
  key: Vec<C>,
  suffix: &[C],
  included: bool,
) where
  C: Ord + Clone,
  P: SharedPointerKind,
{
  let Some(first) = suffix.first() else {
    // `lower` is exactly this node's key. Its descendants all outrank it, so they
    // are included wholesale; the node's own value is included only when the bound
    // is inclusive.
    if included {
      stack.push(RangeFrame::Node { node, key });
    } else {
      for edge in node.children.iter().rev() {
        let mut child_key = dup_key::<C>(&key);
        extend_key::<C>(&mut child_key, &edge.label);
        stack.push(RangeFrame::Node {
          node: &edge.child,
          key: child_key,
        });
      }
    }
    return;
  };

  // `lower` extends strictly below this node, so the node's own value (a proper
  // prefix of `lower`) is below the bound and excluded. Children fall into three
  // groups by their first component relative to `first`.
  let idx = node.child_index(first.borrow());
  let gt_start = match idx {
    Ok(i) => i + 1,
    Err(i) => i,
  };
  // Children whose first component exceeds `first` are wholly above the bound, and
  // sort after the matching child's subtree — push them first (deepest), reversed
  // so the smallest of them is popped first.
  for edge in node.children[gt_start..].iter().rev() {
    let mut child_key = dup_key::<C>(&key);
    extend_key::<C>(&mut child_key, &edge.label);
    stack.push(RangeFrame::Node {
      node: &edge.child,
      key: child_key,
    });
  }
  // Children whose first component is below `first` are wholly below the bound and
  // excluded. The matching child (if any) is seeded last, so its frames sit on top.
  if let Ok(i) = idx {
    let edge = &node.children[i];
    let shared = common_len::<C>(&edge.label, suffix);
    let mut child_key = dup_key::<C>(&key);
    extend_key::<C>(&mut child_key, &edge.label);
    if shared == edge.label.len() {
      // Whole edge matched: continue the descent with the remaining suffix.
      seed_lower(stack, &edge.child, child_key, &suffix[shared..], included);
    } else if shared == suffix.len() {
      // `lower` runs out mid-edge: it is a proper prefix of every key in this
      // subtree, so all of them strictly exceed it — include the whole subtree.
      stack.push(RangeFrame::Node {
        node: &edge.child,
        key: child_key,
      });
    } else if Borrow::<C>::borrow(&suffix[shared]) < Borrow::<C>::borrow(&edge.label[shared]) {
      // The edge diverges above `lower`: the whole subtree exceeds it — include it.
      stack.push(RangeFrame::Node {
        node: &edge.child,
        key: child_key,
      });
    }
    // Otherwise the edge diverges below `lower`: the whole subtree is excluded.
  }
}

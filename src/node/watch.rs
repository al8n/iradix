use core::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

use event_listener::{Event, EventListener};

use super::*;

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
pub(crate) struct WatchSlot {
  event: Event,
  notified: AtomicBool,
}

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

impl<P, C, V> Root<P, C, V>
where
  P: SharedPointerKind,
{
  /// Returns `true` if the physical root node is absent (the `None`-root state the
  /// `watch` empty-position slot keys off). Distinct from [`is_empty`](Root::is_empty):
  /// the root node can exist with `len == 0` only transiently mid-mutation, but
  /// `watch_slot` returns `None` exactly when this is `true`.
  #[inline]
  pub(crate) const fn root_is_none(&self) -> bool {
    self.root.is_none()
  }

  /// Canonicalizes a *logically* empty trie (`len == 0`) to the *physically* empty
  /// `None`-root state, so "empty ⟺ root is None" holds. A net-empty mutation (e.g.
  /// insert-then-remove the same key from an empty base) can leave `root = Some(empty
  /// node)` with `len == 0`; without this, the `watch` diff would read that as a
  /// None -> Some transition and spuriously wake empty-position watchers. Called by
  /// the `watch` `commit` before computing the per-epoch empty slot.
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

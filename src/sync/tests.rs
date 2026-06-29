use super::*;
use archery::SharedPointer;
use proptest::prelude::*;
use std::{collections::BTreeMap, vec, vec::Vec};

type Trie = Radix<u8, u32>;

fn bytes(s: &[u8]) -> Vec<u8> {
  s.to_vec()
}

/// Builds a tree by inserting `(key, value)` pairs through one transaction.
fn build<const N: usize>(entries: [(&[u8], u32); N]) -> Trie {
  let mut t = Radix::new().txn();
  for (k, v) in entries {
    t.insert(k, v);
  }
  t.commit()
}

// One-shot edit helpers: each opens a `txn`, applies a single mutator, and commits
// the next immutable tree alongside what the mutator reported — so the persistence
// assertions (the source tree stays frozen) read the same as a single committed edit.
fn one_insert(t: &Trie, key: &[u8], value: u32) -> (Trie, Option<u32>) {
  let mut txn = t.txn();
  let prev = txn.insert(key, value);
  (txn.commit(), prev)
}

fn one_remove(t: &Trie, key: &[u8]) -> (Trie, Option<u32>) {
  let mut txn = t.txn();
  let prev = txn.remove(key);
  (txn.commit(), prev)
}

fn one_remove_descendants(t: &Trie, key: &[u8]) -> (Trie, usize) {
  let mut txn = t.txn();
  let n = txn.remove_descendants(key);
  (txn.commit(), n)
}

fn one_delete_prefix(t: &Trie, key: &[u8]) -> (Trie, usize) {
  let mut txn = t.txn();
  let n = txn.delete_prefix(key);
  (txn.commit(), n)
}

fn one_drain_descendants(t: &Trie, key: &[u8]) -> (Trie, Vec<u32>) {
  let mut txn = t.txn();
  let drained = txn.drain_descendants(key);
  (txn.commit(), drained)
}

fn one_drain_prefix(t: &Trie, key: &[u8]) -> (Trie, Vec<u32>) {
  let mut txn = t.txn();
  let drained = txn.drain_prefix(key);
  (txn.commit(), drained)
}

// ----- Send/Sync contract (auto-derived, none explicit) -------------------

const fn assert_send<T: Send>() {}
const fn assert_sync<T: Sync>() {}

#[test]
fn sync_radix_is_send_sync() {
  // `sync::Radix` and `sync::Txn` (ArcK) must be Send + Sync when their components
  // and values are. These are AUTO-derived (no explicit `unsafe impl Send/Sync`).
  assert_send::<Trie>();
  assert_sync::<Trie>();
  assert_send::<Radix<char, std::string::String>>();
  assert_sync::<Radix<char, std::string::String>>();
  assert_send::<Txn<u8, u32>>();
  assert_sync::<Txn<u8, u32>>();
}

// ----- a committed edit yields a NEW tree, leaving the source frozen (persistence) --

#[test]
fn new_is_empty_and_const() {
  // `new()` is `const` without `watch`; with `watch` it allocates the shared
  // empty-position channel, so it is a runtime value there.
  #[cfg(not(feature = "watch"))]
  const EMPTY: Trie = Radix::new();
  #[cfg(feature = "watch")]
  #[allow(non_snake_case)]
  let EMPTY: Trie = Radix::new();
  assert!(EMPTY.is_empty());

  let (t, prev) = one_insert(&EMPTY, b"abc", 1);
  assert_eq!(prev, None);
  let (t, prev) = one_insert(&t, b"abd", 2);
  assert_eq!(prev, None);
  assert_eq!(t.get(b"abc".as_slice()), Some(&1));
  assert_eq!(t.get(b"abd".as_slice()), Some(&2));
  assert_eq!(t.get(b"ab".as_slice()), None);
  assert_eq!(t.len(), 2);
}

#[test]
fn committed_insert_leaves_source_unchanged() {
  let base = build([(b"a", 1), (b"b", 2)]);
  let (next, prev) = one_insert(&base, b"a", 100);
  assert_eq!(prev, Some(1));
  // The source is frozen; only the committed tree sees the change.
  assert_eq!(base.get(b"a".as_slice()), Some(&1));
  assert_eq!(base.len(), 2);
  assert_eq!(next.get(b"a".as_slice()), Some(&100));
  assert_eq!(next.len(), 2);
}

#[test]
fn committed_remove_persists() {
  let base = build([(b"a", 1), (b"ab", 2), (b"b", 3)]);

  let (after_remove, removed) = one_remove(&base, b"a");
  assert_eq!(removed, Some(1));
  assert_eq!(base.get(b"a".as_slice()), Some(&1)); // source intact
  assert_eq!(after_remove.get(b"a".as_slice()), None);
  assert_eq!(after_remove.len(), 2);

  assert_eq!(base.len(), 3); // source never changed
}

#[test]
fn committed_prefix_ops_persist() {
  let base = build([(b"x", 1), (b"x/y", 2), (b"x/y/z", 3), (b"y", 9)]);

  let (kept, n) = one_remove_descendants(&base, b"x");
  assert_eq!(n, 2);
  assert_eq!(kept.get(b"x".as_slice()), Some(&1)); // strict: self kept
  assert_eq!(kept.len(), 2);
  assert!(kept.is_canonical());

  let (nuked, n) = one_delete_prefix(&base, b"x");
  assert_eq!(n, 3);
  assert_eq!(nuked.get(b"x".as_slice()), None); // node-inclusive: self gone
  assert_eq!(nuked.get(b"y".as_slice()), Some(&9));
  assert_eq!(nuked.len(), 1);
  assert!(nuked.is_canonical());

  // The base is untouched by either prefix op.
  assert_eq!(base.len(), 4);
  assert_eq!(base.get(b"x".as_slice()), Some(&1));
}

#[test]
fn committed_drain_returns_values_and_persists() {
  let base = build([(b"a", 1), (b"a/b", 2), (b"a/b/c", 3)]);

  let (after, mut drained) = one_drain_descendants(&base, b"a");
  drained.sort_unstable();
  assert_eq!(drained, vec![2, 3]);
  assert_eq!(after.get(b"a".as_slice()), Some(&1));
  assert_eq!(after.get(b"a/b".as_slice()), None);
  // base keeps everything (drain clones values out; source tree unchanged).
  assert_eq!(base.len(), 3);
  assert_eq!(base.get(b"a/b/c".as_slice()), Some(&3));

  let (after_prefix, drained) = one_drain_prefix(&base, b"a");
  assert_eq!(drained, vec![1, 2, 3]); // node-inclusive, ascending
  assert!(after_prefix.is_empty());
  assert_eq!(base.len(), 3);
}

#[test]
fn ancestors_inclusive_and_strict() {
  let t = build([(b"a", 1), (b"abc", 3)]);
  assert_eq!(t.get_ancestor(b"abc".as_slice()), Some(&3)); // inclusive
  assert_eq!(t.get_ancestor(b"abcd".as_slice()), Some(&3));
  assert_eq!(t.strict_ancestor(b"abc".as_slice()), Some(&1)); // exclusive
  assert!(t.has_ancestor(b"abc".as_slice()));
  assert!(!t.has_ancestor(b"x".as_slice()));
}

#[test]
fn iterators_over_arc_face() {
  let t = build([(b"a", 1), (b"ab", 2), (b"abc", 3), (b"b", 4)]);

  let mut all: Vec<u32> = t.values().copied().collect();
  all.sort_unstable();
  assert_eq!(all, vec![1, 2, 3, 4]);

  let mut anc: Vec<u32> = t.ancestors(b"abc".as_slice()).copied().collect();
  anc.sort_unstable();
  assert_eq!(anc, vec![1, 2, 3]);

  let mut desc: Vec<u32> = t.descendants(b"a".as_slice()).copied().collect();
  desc.sort_unstable();
  assert_eq!(desc, vec![2, 3]);
}

// ----- Txn: batching, read-your-writes, commit isolation ------------------

#[test]
fn txn_batches_then_commits() {
  let base: Trie = Radix::new();
  let mut txn = base.txn();
  assert_eq!(txn.insert(&bytes(b"a"), 1), None);
  assert_eq!(txn.insert(&bytes(b"ab"), 2), None);
  assert_eq!(txn.insert(&bytes(b"a"), 10), Some(1)); // replace within the txn
  assert_eq!(txn.len(), 2);
  let committed = txn.commit();

  assert_eq!(committed.len(), 2);
  assert_eq!(committed.get(b"a".as_slice()), Some(&10));
  assert_eq!(base.len(), 0); // the source tree never changed
}

#[test]
fn txn_read_your_writes() {
  let base = build([(b"a", 1)]);
  let mut txn = base.txn();
  // Reads observe edits made within the same txn.
  assert_eq!(txn.get(b"a".as_slice()), Some(&1));
  txn.insert(&bytes(b"ab"), 2);
  txn.insert(&bytes(b"b"), 3);
  assert_eq!(txn.get(b"ab".as_slice()), Some(&2));
  assert!(txn.contains(b"b".as_slice()));
  assert_eq!(txn.get_ancestor(b"abc".as_slice()), Some(&2));
  assert_eq!(txn.strict_ancestor(b"ab".as_slice()), Some(&1));
  assert!(txn.has_ancestor(b"abc".as_slice()));

  let mut anc: Vec<u32> = txn.ancestors(b"ab".as_slice()).copied().collect();
  anc.sort_unstable();
  assert_eq!(anc, vec![1, 2]);
  let mut all: Vec<u32> = txn.values().copied().collect();
  all.sort_unstable();
  assert_eq!(all, vec![1, 2, 3]);
  let mut desc: Vec<u32> = txn.descendants(b"a".as_slice()).copied().collect();
  desc.sort_unstable();
  assert_eq!(desc, vec![2]);

  // The base is unaffected until (and even after) commit.
  assert_eq!(base.len(), 1);
}

#[test]
fn txn_dropped_without_commit_discards_edits() {
  let base = build([(b"a", 1)]);
  {
    let mut txn = base.txn();
    txn.insert(&bytes(b"b"), 2);
    txn.clear();
    txn.insert(&bytes(b"c"), 3);
    // txn dropped here without commit.
  }
  assert_eq!(base.len(), 1);
  assert_eq!(base.get(b"a".as_slice()), Some(&1));
  assert_eq!(base.get(b"c".as_slice()), None);
}

#[test]
fn txn_clear_then_commit() {
  let base = build([(b"a", 1), (b"b", 2)]);
  let mut txn = base.txn();
  txn.clear();
  assert!(txn.is_empty());
  txn.insert(&bytes(b"z"), 9);
  let committed = txn.commit();
  assert_eq!(committed.len(), 1);
  assert_eq!(committed.get(b"z".as_slice()), Some(&9));
  assert_eq!(base.len(), 2); // source untouched
}

// ----- FromIterator --------------------------------------------------------

#[test]
fn from_iterator_builds_tree() {
  let t: Trie = [
    (bytes(b"a"), 1u32),
    (bytes(b"ab"), 2),
    (bytes(b"a"), 10), // later key wins
    (bytes(b"b"), 3),
  ]
  .into_iter()
  .collect();
  assert_eq!(t.len(), 3);
  assert_eq!(t.get(b"a".as_slice()), Some(&10));
  assert_eq!(t.get(b"ab".as_slice()), Some(&2));
  assert_eq!(t.get(b"b".as_slice()), Some(&3));
  assert!(t.is_canonical());
}

// ----- snapshot isolation & structural sharing (Arc) ----------------------

#[test]
fn clone_is_snapshot_isolated() {
  let snap = build([(b"k", 1)]);
  let (live, _) = one_insert(&snap, b"k", 2);
  let (live, _) = one_insert(&live, b"k2", 3);
  assert_eq!(snap.get(b"k".as_slice()), Some(&1));
  assert_eq!(snap.get(b"k2".as_slice()), None);
  assert_eq!(live.get(b"k".as_slice()), Some(&2));
}

#[test]
fn unchanged_subtree_is_shared() {
  let snap = build([(b"a/x", 1), (b"b/y", 2)]);
  let (live, _) = one_insert(&snap, b"a/z", 3); // touches only "a"

  let snap_b = snap.edge_child(&b'b').expect("b edge");
  let t_b = live.edge_child(&b'b').expect("b edge");
  assert!(
    SharedPointer::ptr_eq(&snap_b, &t_b),
    "the untouched b subtree must be shared"
  );
  let snap_a = snap.edge_child(&b'a').expect("a edge");
  let t_a = live.edge_child(&b'a').expect("a edge");
  assert!(!SharedPointer::ptr_eq(&snap_a, &t_a));
}

#[test]
fn txn_unchanged_subtree_is_shared_after_commit() {
  let snap = build([(b"a/x", 1), (b"b/y", 2)]);
  let mut txn = snap.txn();
  txn.insert(&bytes(b"a/z"), 3); // touches only "a"
  let live = txn.commit();

  let snap_b = snap.edge_child(&b'b').expect("b edge");
  let t_b = live.edge_child(&b'b').expect("b edge");
  assert!(
    SharedPointer::ptr_eq(&snap_b, &t_b),
    "an untouched subtree must stay shared across a txn commit"
  );
}

#[test]
fn drain_shared_subtree_clones_not_moves() {
  let snap = build([(b"a", 1), (b"a/b", 2), (b"a/b/c", 3)]);

  let (live, mut drained) = one_drain_descendants(&snap, b"a");
  drained.sort_unstable();
  assert_eq!(drained, vec![2, 3]);
  assert_eq!(live.get(b"a".as_slice()), Some(&1));
  assert_eq!(live.get(b"a/b".as_slice()), None);

  // The original still has everything (values were cloned, not stolen).
  assert_eq!(snap.get(b"a".as_slice()), Some(&1));
  assert_eq!(snap.get(b"a/b".as_slice()), Some(&2));
  assert_eq!(snap.get(b"a/b/c".as_slice()), Some(&3));
  assert_eq!(snap.len(), 3);
}

#[test]
fn remove_descendants_keeps_self() {
  let base = build([(b"a", 1), (b"ab", 2), (b"ac", 3), (b"b", 4)]);
  let (t, n) = one_remove_descendants(&base, b"a");
  assert_eq!(n, 2);
  assert_eq!(t.get(b"a".as_slice()), Some(&1));
  assert_eq!(t.get(b"b".as_slice()), Some(&4));
  assert_eq!(t.len(), 2);
  assert!(t.is_canonical());
}

#[test]
fn cross_thread_snapshot() {
  // A `sync::Radix` snapshot can be moved into another thread and read there.
  use std::thread;
  let snap = build([(b"a/b/c", 7)]);
  let h = thread::spawn(move || snap.get(b"a/b/c".as_slice()).copied());
  assert_eq!(h.join().unwrap(), Some(7));
}

// ----- lock-free shared holder via ArcSwap --------------------------------

// A long many-thread CAS stress test: skipped under miri (the arc-swap debt-list
// machinery across 11 threads × thousands of iterations is prohibitively slow
// there). The CAS protocol's correctness is also covered by `examples/sync.rs`.
#[test]
#[cfg_attr(miri, ignore)]
fn lock_free_arcswap_writers_all_land() {
  use arc_swap::ArcSwap;
  use std::{sync::Arc, thread};

  let holder: Arc<ArcSwap<Radix<u8, u32>>> = Arc::new(ArcSwap::from_pointee(Radix::new()));

  const WRITERS: u8 = 8;

  thread::scope(|s| {
    // Reader threads load wait-free snapshots while writers publish concurrently.
    for _ in 0..3 {
      let holder = Arc::clone(&holder);
      s.spawn(move || {
        for _ in 0..2000 {
          let snap = holder.load();
          // Every loaded snapshot is internally consistent regardless of races.
          assert_eq!(snap.len(), snap.count_values());
          assert!(snap.is_canonical());
        }
      });
    }

    // Each writer inserts its own distinct key with a CAS retry loop, so no write
    // is ever silently lost even when two writers race on the same prior version.
    for w in 0..WRITERS {
      let holder = Arc::clone(&holder);
      s.spawn(move || {
        loop {
          let current = holder.load_full();
          let mut txn = current.txn();
          txn.insert([w].as_slice(), u32::from(w));
          let next = Arc::new(txn.commit());
          let prev = holder.compare_and_swap(&current, next);
          if Arc::ptr_eq(&current, &prev) {
            break; // our publish won
          }
          // Lost the race: loop and retry against the latest published tree.
        }
      });
    }
  });

  // After every writer joins, all keys are present exactly once.
  let final_tree = holder.load_full();
  assert_eq!(final_tree.len(), usize::from(WRITERS));
  for w in 0..WRITERS {
    assert_eq!(final_tree.get([w].as_slice()), Some(&u32::from(w)));
  }
  assert!(final_tree.is_canonical());
}

// ----- proptest: the Arc path matches the reference model ------------------

prop_compose! {
  fn key_strategy()(v in prop::collection::vec(0u8..6, 0..6)) -> Vec<u8> { v }
}

#[derive(Clone, Debug)]
enum Op {
  Insert(Vec<u8>, u32),
  Remove(Vec<u8>),
  RemoveDescendants(Vec<u8>),
}

fn op_strategy() -> impl Strategy<Value = Op> {
  prop_oneof![
    (key_strategy(), 0u32..1000).prop_map(|(k, v)| Op::Insert(k, v)),
    key_strategy().prop_map(Op::Remove),
    key_strategy().prop_map(Op::RemoveDescendants),
  ]
}

fn model_remove_descendants(model: &mut BTreeMap<Vec<u8>, u32>, prefix: &[u8]) -> usize {
  let victims: Vec<Vec<u8>> = model
    .keys()
    .filter(|k| k.len() > prefix.len() && k.starts_with(prefix))
    .cloned()
    .collect();
  for k in &victims {
    model.remove(k);
  }
  victims.len()
}

// Proptests run the full case sweep natively but only a handful under miri:
// miri models the target address space (4 GiB on 32-bit targets), and the full
// sweep's cumulative allocations would exhaust it.
fn proptest_cfg() -> ProptestConfig {
  ProptestConfig {
    cases: if cfg!(miri) {
      8
    } else {
      ProptestConfig::default().cases
    },
    ..ProptestConfig::default()
  }
}

proptest! {
  #![proptest_config(proptest_cfg())]
  // The sequence of ops is applied through one transaction (read-your-writes),
  // matching the in-place reference model; this also exercises `Txn` end to end.
  #[test]
  fn model_matches_reference(ops in prop::collection::vec(op_strategy(), 0..if cfg!(miri) { 16 } else { 60 })) {
    let mut txn: Txn<u8, u32> = Radix::new().txn();
    let mut model: BTreeMap<Vec<u8>, u32> = BTreeMap::new();
    for op in ops {
      match op {
        Op::Insert(k, v) => {
          prop_assert_eq!(txn.insert(&k.clone(), v), model.insert(k, v));
        }
        Op::Remove(k) => {
          prop_assert_eq!(txn.remove(k.as_slice()), model.remove(&k));
        }
        Op::RemoveDescendants(k) => {
          prop_assert_eq!(
            txn.remove_descendants(k.as_slice()),
            model_remove_descendants(&mut model, &k)
          );
        }
      }
      prop_assert_eq!(txn.len(), model.len());
      prop_assert_eq!(txn.len(), txn.count_values());
      prop_assert!(txn.is_canonical());
      for (k, v) in &model {
        prop_assert_eq!(txn.get(k.as_slice()), Some(v));
      }
    }

    // Committing yields an immutable tree with the same contents.
    let trie = txn.commit();
    prop_assert_eq!(trie.len(), model.len());
    prop_assert_eq!(trie.len(), trie.count_values());
    prop_assert!(trie.is_canonical());
    for (k, v) in &model {
      prop_assert_eq!(trie.get(k.as_slice()), Some(v));
    }
  }
}

#[test]
fn committed_insert_preserves_each_prior_version() {
  // A chain of committed inserts: every intermediate tree stays frozen at its own
  // contents — persistence across the whole history, not just the last snapshot.
  let v0: Trie = Radix::new();
  let (v1, _) = one_insert(&v0, b"a", 1);
  let (v2, _) = one_insert(&v1, b"b", 2);
  let (v3, _) = one_insert(&v2, b"c", 3);

  assert_eq!(v0.len(), 0);
  assert_eq!(v1.len(), 1);
  assert_eq!(v2.len(), 2);
  assert_eq!(v3.len(), 3);
  assert_eq!(v1.get(b"b".as_slice()), None);
  assert_eq!(v2.get(b"c".as_slice()), None);
  assert_eq!(v3.get(b"a".as_slice()), Some(&1));
}

// ----- go-immutable-radix parity (Arc face) -------------------------------

use core::ops::Bound;

#[test]
fn min_max_empty_and_populated() {
  let empty: Trie = Radix::new();
  assert_eq!(empty.minimum(), None);
  assert_eq!(empty.maximum(), None);

  let t = build([(b"a", 1), (b"abc", 2), (b"abd", 3), (b"b", 4)]);
  assert_eq!(t.minimum(), Some((bytes(b"a"), &1)));
  assert_eq!(t.maximum(), Some((bytes(b"b"), &4)));
}

#[test]
fn delete_prefix_is_node_inclusive_and_vs_remove_descendants() {
  let base = build([(b"x", 1), (b"x/y", 2), (b"x/y/z", 3), (b"y", 9)]);

  let (keep, n) = one_remove_descendants(&base, b"x");
  assert_eq!(n, 2);
  assert_eq!(keep.get(b"x".as_slice()), Some(&1)); // kept
  assert_eq!(keep.len(), 2);
  assert!(keep.is_canonical());

  let (nuke, n) = one_delete_prefix(&base, b"x");
  assert_eq!(n, 3);
  assert_eq!(nuke.get(b"x".as_slice()), None); // removed
  assert_eq!(nuke.get(b"y".as_slice()), Some(&9));
  assert_eq!(nuke.len(), 1);
  assert!(nuke.is_canonical());
}

#[test]
fn drain_prefix_ascending_and_shared_clones() {
  let snap = build([(b"a", 1), (b"a/b", 2), (b"a/c", 3)]);
  // value at key first, then strict descendants ascending
  let (live, drained) = one_drain_prefix(&snap, b"a");
  assert_eq!(drained, vec![1, 2, 3]);
  assert!(live.is_empty());
  // original intact (values cloned, not stolen)
  assert_eq!(snap.get(b"a".as_slice()), Some(&1));
  assert_eq!(snap.get(b"a/b".as_slice()), Some(&2));
  assert_eq!(snap.get(b"a/c".as_slice()), Some(&3));
  assert_eq!(snap.len(), 3);
}

#[test]
fn reverse_iteration() {
  let t = build([(b"a", 1), (b"ab", 2), (b"abc", 3), (b"b", 4)]);
  let rev: Vec<u32> = t.values_rev().copied().collect();
  assert_eq!(rev, vec![4, 3, 2, 1]);
  let drev: Vec<u32> = t.descendants_rev(b"a".as_slice()).copied().collect();
  assert_eq!(drev, vec![3, 2]); // strict descendants ab, abc descending
}

fn rng(t: &Trie, lo: Bound<&[u8]>, hi: Bound<&[u8]>) -> Vec<(Vec<u8>, u32)> {
  t.range::<[u8], _>((lo, hi)).map(|(k, v)| (k, *v)).collect()
}

#[test]
fn range_bound_combinations_and_seek() {
  use core::ops::Bound::{Excluded, Included, Unbounded};
  let t = build([(b"a", 1), (b"b", 2), (b"c", 3), (b"d", 4)]);

  assert_eq!(rng(&t, Unbounded, Unbounded).len(), 4);
  assert_eq!(
    rng(&t, Included(b"b"), Excluded(b"d")),
    vec![(bytes(b"b"), 2), (bytes(b"c"), 3)]
  );
  assert_eq!(
    rng(&t, Excluded(b"b"), Included(b"c")),
    vec![(bytes(b"c"), 3)]
  );
  assert_eq!(rng(&t, Excluded(b"b"), Excluded(b"c")), vec![]);
  assert_eq!(
    rng(&t, Included(b"c"), Included(b"c")),
    vec![(bytes(b"c"), 3)]
  );
  // inverted bounds → empty, no panic
  assert_eq!(rng(&t, Included(b"d"), Included(b"a")), vec![]);

  let seek = |k: &[u8]| -> Vec<u32> { t.seek_lower_bound(k).map(|(_, v)| *v).collect() };
  assert_eq!(seek(b""), vec![1, 2, 3, 4]); // before all
  assert_eq!(seek(b"b"), vec![2, 3, 4]); // exact
  assert_eq!(seek(b"bb"), vec![3, 4]); // between
  assert_eq!(seek(b"z"), vec![]); // after all
}

fn model_range(
  model: &BTreeMap<Vec<u8>, u32>,
  lo: Bound<&[u8]>,
  hi: Bound<&[u8]>,
) -> Vec<(Vec<u8>, u32)> {
  model
    .iter()
    .filter(|(k, _)| {
      let k = k.as_slice();
      let lo_ok = match lo {
        Bound::Unbounded => true,
        Bound::Included(b) => k >= b,
        Bound::Excluded(b) => k > b,
      };
      let hi_ok = match hi {
        Bound::Unbounded => true,
        Bound::Included(b) => k <= b,
        Bound::Excluded(b) => k < b,
      };
      lo_ok && hi_ok
    })
    .map(|(k, v)| (k.clone(), *v))
    .collect()
}

fn model_delete_prefix(model: &mut BTreeMap<Vec<u8>, u32>, prefix: &[u8]) -> usize {
  let victims: Vec<Vec<u8>> = model
    .keys()
    .filter(|k| k.starts_with(prefix))
    .cloned()
    .collect();
  for k in &victims {
    model.remove(k);
  }
  victims.len()
}

proptest! {
  #![proptest_config(proptest_cfg())]
  #[test]
  fn ordered_ops_match_model(
    entries in prop::collection::vec((key_strategy(), 0u32..1000), 0..if cfg!(miri) { 12 } else { 40 }),
    lo_k in key_strategy(),
    hi_k in key_strategy(),
    del in key_strategy(),
  ) {
    // Build immutably from the entries, then exercise ordered reads and a
    // committed `drain_prefix` against the reference model.
    let entries_keyed: Vec<(Vec<u8>, u32)> = entries.clone();
    let trie: Trie = entries_keyed.into_iter().collect();
    let mut model: BTreeMap<Vec<u8>, u32> = BTreeMap::new();
    for (k, v) in entries {
      model.insert(k, v);
    }

    prop_assert_eq!(
      trie.minimum().map(|(k, v)| (k, *v)),
      model.iter().next().map(|(k, v)| (k.clone(), *v))
    );
    prop_assert_eq!(
      trie.maximum().map(|(k, v)| (k, *v)),
      model.iter().next_back().map(|(k, v)| (k.clone(), *v))
    );

    let model_vals: Vec<u32> = model.values().copied().collect();
    let rev: Vec<u32> = trie.values_rev().copied().collect();
    let mut want_rev = model_vals.clone();
    want_rev.reverse();
    prop_assert_eq!(rev, want_rev);

    let los = [
      Bound::Unbounded,
      Bound::Included(lo_k.as_slice()),
      Bound::Excluded(lo_k.as_slice()),
    ];
    let his = [
      Bound::Unbounded,
      Bound::Included(hi_k.as_slice()),
      Bound::Excluded(hi_k.as_slice()),
    ];
    for &lo in &los {
      for &hi in &his {
        let got: Vec<(Vec<u8>, u32)> =
          trie.range::<[u8], _>((lo, hi)).map(|(k, v)| (k, *v)).collect();
        prop_assert_eq!(got, model_range(&model, lo, hi));
      }
    }

    let seek: Vec<(Vec<u8>, u32)> =
      trie.seek_lower_bound(del.as_slice()).map(|(k, v)| (k, *v)).collect();
    prop_assert_eq!(seek, model_range(&model, Bound::Included(del.as_slice()), Bound::Unbounded));

    // delete_prefix removes the at-or-below set; drain_prefix returns it ascending.
    let (drained_trie, drained) = one_drain_prefix(&trie, del.as_slice());
    let want_drained: Vec<u32> = model
      .iter()
      .filter(|(k, _)| k.starts_with(&del))
      .map(|(_, v)| *v)
      .collect();
    prop_assert_eq!(&drained, &want_drained);
    let removed = model_delete_prefix(&mut model, &del);
    prop_assert_eq!(drained.len(), removed);
    prop_assert_eq!(drained_trie.len(), model.len());
    prop_assert_eq!(drained_trie.len(), drained_trie.count_values());
    prop_assert!(drained_trie.is_canonical());
    for (k, v) in &model {
      prop_assert_eq!(drained_trie.get(k.as_slice()), Some(v));
    }
    // The pre-drain tree is frozen at its own contents (persistence).
    prop_assert!(trie.is_canonical());
  }
}

// ----- panic-safety: a Clone that panics must not corrupt the working copy --
//
// The node core is unchanged, so its strong-exception guarantee holds; here it is
// exercised through `Txn` (where the mutators live). Every mutation builds its new
// pieces (cloning `C` labels / `V` values) BEFORE detaching existing structure, so
// an unwind out of a user `Clone` leaves the working copy structurally valid (every
// pre-existing key still resolves) and `len` accurate. Panicking `Drop` is out of
// scope (it aborts while unwinding), so these fuses target `Clone`, not `Drop`.

use core::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};

thread_local! {
  // Clones remaining before the next `KeyBomb::clone` panics; `None` = disarmed.
  static KEY_FUSE: Cell<Option<usize>> = const { Cell::new(None) };
  // As above, for `ValBomb::clone`.
  static VAL_FUSE: Cell<Option<usize>> = const { Cell::new(None) };
}

fn tick(fuse: &'static std::thread::LocalKey<Cell<Option<usize>>>) {
  fuse.with(|c| {
    if let Some(n) = c.get() {
      if n == 0 {
        c.set(None);
        panic!("armed clone fuse fired");
      }
      c.set(Some(n - 1));
    }
  });
}

/// A component `C` whose `Clone` panics on an armed invocation.
#[derive(PartialEq, Eq, PartialOrd, Ord, Debug)]
struct KeyBomb(u8);

impl Clone for KeyBomb {
  fn clone(&self) -> Self {
    tick(&KEY_FUSE);
    Self(self.0)
  }
}

/// A value whose `Clone` panics on an armed invocation.
#[derive(PartialEq, Eq, Debug)]
struct ValBomb(u32);

impl Clone for ValBomb {
  fn clone(&self) -> Self {
    tick(&VAL_FUSE);
    Self(self.0)
  }
}

fn key(bytes: &[u8]) -> Vec<KeyBomb> {
  bytes.iter().map(|&b| KeyBomb(b)).collect()
}

#[test]
fn split_panic_in_label_clone_keeps_txn_consistent() {
  // Edge "[1,2,3]". Inserting "[1,2,4]" splits it at "[1,2]", cloning the head/tail
  // labels. A panic during that clone must not have detached the original edge.
  let mut t: Txn<KeyBomb, u32> = Radix::<KeyBomb, u32>::new().txn();
  t.insert(&key(&[1, 2, 3]), 100);
  assert_eq!(t.len(), 1);

  // Insert is iterator-native: it clones only STORED components. The split first
  // collects the divergent suffix (`[4]`, one clone), then the head/tail label
  // clones. Arm to fire on the second clone — partway through the split's label
  // copying, before the infallible splice.
  KEY_FUSE.with(|c| c.set(Some(1)));
  let r = catch_unwind(AssertUnwindSafe(|| t.insert(&key(&[1, 2, 4]), 200)));
  KEY_FUSE.with(|c| c.set(None));
  assert!(r.is_err(), "the armed split clone must have panicked");

  // The pre-existing key still resolves and `len` is unchanged.
  assert_eq!(t.get(key(&[1, 2, 3]).as_slice()), Some(&100));
  assert_eq!(t.get(key(&[1, 2, 4]).as_slice()), None);
  assert_eq!(t.len(), 1);
  assert_eq!(t.count_values(), 1);
}

#[test]
fn drain_descendants_panic_in_value_clone_keeps_txn_intact() {
  // Draining clones every descendant value out FIRST; a panic there must leave the
  // working copy and `len` completely untouched (nothing unlinked yet).
  let mut t: Txn<KeyBomb, ValBomb> = Radix::<KeyBomb, ValBomb>::new().txn();
  t.insert(&key(&[1]), ValBomb(1));
  t.insert(&key(&[1, 2]), ValBomb(2));
  t.insert(&key(&[1, 2, 3]), ValBomb(3));
  assert_eq!(t.len(), 3);

  // Phase 1 clones the two descendant values; fire on the second clone.
  VAL_FUSE.with(|c| c.set(Some(1)));
  let r = catch_unwind(AssertUnwindSafe(|| {
    t.drain_descendants(key(&[1]).as_slice())
  }));
  VAL_FUSE.with(|c| c.set(None));
  assert!(r.is_err(), "the armed value clone must have panicked");

  // Nothing was unlinked: all three keys still resolve and `len` is unchanged.
  assert_eq!(t.get(key(&[1]).as_slice()), Some(&ValBomb(1)));
  assert_eq!(t.get(key(&[1, 2]).as_slice()), Some(&ValBomb(2)));
  assert_eq!(t.get(key(&[1, 2, 3]).as_slice()), Some(&ValBomb(3)));
  assert_eq!(t.len(), 3);
  assert_eq!(t.count_values(), 3);
}

#[test]
fn remove_descendants_never_clones_values_through_txn() {
  // `remove_descendants` is count-only: it must not clone any `V`. Arm the VALUE
  // fuse to fire on the very first value clone — if the count-only path ever clones
  // a value, this fails.
  let mut t: Txn<KeyBomb, ValBomb> = Radix::<KeyBomb, ValBomb>::new().txn();
  t.insert(&key(&[1]), ValBomb(1));
  t.insert(&key(&[1, 2]), ValBomb(2));
  t.insert(&key(&[1, 2, 3]), ValBomb(3));
  assert_eq!(t.len(), 3);

  VAL_FUSE.with(|c| c.set(Some(0)));
  let removed = catch_unwind(AssertUnwindSafe(|| {
    t.remove_descendants(key(&[1]).as_slice())
  }));
  VAL_FUSE.with(|c| c.set(None));
  assert_eq!(
    removed.ok(),
    Some(2),
    "remove_descendants must not clone any value"
  );
  assert_eq!(t.len(), 1);
  assert_eq!(t.count_values(), 1);
  assert_eq!(t.get(key(&[1]).as_slice()), Some(&ValBomb(1)));
}

#[test]
fn delete_prefix_on_shared_working_copy_never_clones_removed_values() {
  // `delete_prefix` is count-only and node-inclusive. Even when the txn shares the
  // subtree with the source tree — so mutation copies the path to the prefix — it
  // must not clone the VALUES being removed (it unlinks the doomed edge). The prefix
  // is a direct child of the (valueless) root, so no ancestor value is on the copied
  // path: the expected value-clone count is exactly zero.
  let source: Radix<KeyBomb, ValBomb> = {
    let mut t = Radix::<KeyBomb, ValBomb>::new().txn();
    t.insert(&key(&[1]), ValBomb(1));
    t.insert(&key(&[1, 2]), ValBomb(2));
    t.insert(&key(&[1, 3]), ValBomb(3));
    t.commit()
  };
  let mut t = source.txn(); // shares the whole subtree with `source`
  assert_eq!(t.len(), 3);

  VAL_FUSE.with(|c| c.set(Some(0))); // panic on ANY value clone
  let removed = catch_unwind(AssertUnwindSafe(|| t.delete_prefix(key(&[1]).as_slice())));
  VAL_FUSE.with(|c| c.set(None));

  assert_eq!(
    removed.ok(),
    Some(3),
    "delete_prefix must not clone any removed value, even on a shared working copy"
  );
  assert_eq!(t.len(), 0);
  assert_eq!(t.count_values(), 0);
  // The source tree is untouched (structural sharing preserved).
  assert_eq!(source.len(), 3);
  assert_eq!(source.get(key(&[1, 2]).as_slice()), Some(&ValBomb(2)));
}

#[test]
fn delete_prefix_may_clone_a_retained_ancestor_value() {
  // The copy-on-write boundary: delete_prefix clones no REMOVED value, but on a
  // working copy shared with the source it still duplicates the RETAINED ancestors
  // on the path to the prefix — including their values, exactly like every mutator.
  let source: Radix<KeyBomb, ValBomb> = {
    let mut t = Radix::<KeyBomb, ValBomb>::new().txn();
    t.insert(&key(&[1]), ValBomb(1)); // retained ancestor — has a value
    t.insert(&key(&[1, 2]), ValBomb(2)); // the deleted prefix
    t.commit()
  };
  let mut t = source.txn(); // shares the path with `source`
  assert_eq!(t.len(), 2);

  // No key is materialized (delete_prefix is iterator-native), so the only value
  // clone is the COW copy of the retained ancestor `[1]` on the path to the prefix.
  // Arm the VALUE fuse on that first value clone.
  VAL_FUSE.with(|c| c.set(Some(0)));
  let r = catch_unwind(AssertUnwindSafe(|| {
    t.delete_prefix(key(&[1, 2]).as_slice())
  }));
  VAL_FUSE.with(|c| c.set(None));

  assert!(
    r.is_err(),
    "copy-on-write of the retained ancestor's value panicked (the documented boundary)"
  );
  // Strong exception: the panic before any unlink leaves the working copy intact.
  assert_eq!(t.len(), 2);
  assert_eq!(t.count_values(), 2);
  assert_eq!(t.get(key(&[1]).as_slice()), Some(&ValBomb(1)));
  assert_eq!(t.get(key(&[1, 2]).as_slice()), Some(&ValBomb(2)));
  assert_eq!(source.len(), 2);
}

// ----- watch -------------------------------------------------------------------

#[cfg(all(feature = "watch", feature = "std"))]
mod watch {
  use super::*;
  use std::time::Duration;

  // The listener is armed before the writer commits+notifies, so a fired
  // notification is already delivered by the time we wait — this bound only guards
  // against a hang.
  const FIRED: Duration = Duration::from_secs(2);
  // A short bound for "must stay quiet" assertions.
  const QUIET: Duration = Duration::from_millis(50);

  #[test]
  fn watch_key_fires_on_change() {
    let r0 = build([(b"a", 1)]);
    let w = r0.watch(b"a".as_slice());
    let mut t = r0.txn();
    t.insert(b"a".as_slice(), 2);
    let r1 = t.commit();
    r1.notify_changes_since(&r0); // commit builds; publish-then-notify fires
    assert!(w.wait_timeout(FIRED));
  }

  #[test]
  fn watch_key_async_fires() {
    let r0 = build([(b"a", 1)]);
    let w = r0.watch(b"a".as_slice());
    let mut t = r0.txn();
    t.insert(b"a".as_slice(), 2);
    let r1 = t.commit();
    r1.notify_changes_since(&r0);
    pollster::block_on(w.changed());
  }

  #[test]
  fn noop_commit_does_not_fire() {
    // An empty no-op commit followed by a notify must stay quiet: nothing changed,
    // so the diff fires nothing.
    let r0 = build([(b"a", 1)]);
    let w = r0.watch(b"a".as_slice());
    let r1 = r0.txn().commit();
    r1.notify_changes_since(&r0);
    assert!(!w.wait_timeout(QUIET));
  }

  #[test]
  fn watch_prefix_fires_on_descendant() {
    let r0 = build([(b"a", 1), (b"b", 2)]);
    let w = r0.watch_prefix(b"a".as_slice());
    let mut t = r0.txn();
    t.insert(b"ax".as_slice(), 9);
    let r1 = t.commit();
    r1.notify_changes_since(&r0);
    assert!(w.wait_timeout(FIRED));
  }

  #[test]
  fn watch_prefix_quiet_on_unrelated_subtree() {
    let r0 = build([(b"a", 1), (b"b", 2)]);
    let w = r0.watch_prefix(b"a".as_slice());
    let mut t = r0.txn();
    t.insert(b"bz".as_slice(), 9);
    let r1 = t.commit();
    r1.notify_changes_since(&r0);
    assert!(!w.wait_timeout(QUIET));
  }

  #[test]
  fn split_above_watched_key_stays_quiet() {
    // Inserting "abd" splits the edge ABOVE "abc" but leaves the node at "abc"
    // physically shared, so a watch on "abc" must NOT fire (the diff prunes it).
    let r0 = build([(b"abc", 1)]);
    let w = r0.watch(b"abc".as_slice());
    let mut t = r0.txn();
    t.insert(b"abd".as_slice(), 2);
    let r1 = t.commit();
    r1.notify_changes_since(&r0);
    assert!(!w.wait_timeout(QUIET));
  }

  #[test]
  fn watch_after_commit_is_already_ready() {
    // Sticky-flag guard: after a change is committed AND published (notified),
    // arming a watch on the now-superseded base must resolve immediately (the node
    // was already fired), not hang.
    let r0 = build([(b"a", 1)]);
    let mut t = r0.txn();
    t.insert(b"a".as_slice(), 2);
    let r1 = t.commit();
    r1.notify_changes_since(&r0); // fires + stickies r0's "a" node
    let w = r0.watch(b"a".as_slice()); // armed AFTER the publish
    assert!(w.wait_timeout(FIRED));
  }

  #[test]
  fn merge_above_watched_key_stays_quiet() {
    // Removing "a" merges the "a" and "bc" edges into "abc" while REUSING the node
    // at "abc"; a watch on "abc" must NOT fire (only the merged-away "a" changed).
    let r0 = build([(b"a", 1), (b"abc", 3)]);
    let w = r0.watch(b"abc".as_slice());
    let mut t = r0.txn();
    assert_eq!(t.remove(b"a".as_slice()), Some(1));
    let r1 = t.commit();
    r1.notify_changes_since(&r0);
    assert!(!w.wait_timeout(QUIET));
  }

  #[test]
  fn watch_absent_key_fires_on_creation() {
    let r0 = build([(b"a", 1)]);
    let w = r0.watch(b"z".as_slice());
    let mut t = r0.txn();
    t.insert(b"z".as_slice(), 9);
    let r1 = t.commit();
    r1.notify_changes_since(&r0);
    assert!(w.wait_timeout(FIRED));
  }

  #[test]
  fn watch_empty_tree_fires_on_first_insert() {
    let r0: Trie = Radix::new();
    let w = r0.watch(b"a".as_slice());
    let mut t = r0.txn();
    t.insert(b"a".as_slice(), 1);
    let r1 = t.commit();
    r1.notify_changes_since(&r0);
    assert!(w.wait_timeout(FIRED));
  }

  #[test]
  fn lost_cas_does_not_poison() {
    // Under lock-free CAS publishing, two txns open from the same base. One wins and
    // notifies; the other is committed but DISCARDED (a lost CAS) and never notifies.
    // The losing commit must not poison the keys it touched: a watch on the loser's
    // key, armed on the base, must stay quiet because that tree was never published.
    let base = build([(b"a", 10), (b"b", 20)]);
    let w_b = base.watch(b"b".as_slice()); // armed before any commit

    // The winner: changes "a", publishes (notifies).
    let mut t1 = base.txn();
    t1.insert(b"a".as_slice(), 11);
    let r1 = t1.commit();
    r1.notify_changes_since(&base);

    // The loser: changes "b", commits, but its CAS lost — so it is dropped and
    // notify is NEVER called.
    let mut t2 = base.txn();
    t2.insert(b"b".as_slice(), 21);
    let _r2 = t2.commit(); // dropped without notify

    // "b" was only touched by the discarded tree, so its watch must stay quiet.
    assert!(
      !w_b.wait_timeout(QUIET),
      "a lost CAS must not poison the keys it touched"
    );
  }

  #[test]
  fn empty_noop_then_insert_fires() {
    // Per-epoch empty slot: an empty -> empty no-op commit carries the same empty
    // slot, so a watch armed on the ORIGINAL empty version still fires when a later
    // insert is published against the no-op commit.
    let r0: Trie = Radix::new();
    let k = b"k".as_slice();
    let w = r0.watch(k);

    let r1 = r0.txn().commit(); // empty -> empty no-op: r1.empty == r0.empty
    r1.notify_changes_since(&r0); // still empty -> still empty: fires nothing

    let mut t = r1.txn();
    t.insert(k, 1);
    let r2 = t.commit();
    r2.notify_changes_since(&r1); // empty -> non-empty: fires r1.empty (== r0.empty)

    assert!(w.wait_timeout(FIRED));
  }

  #[test]
  fn get_watch_reads_and_arms() {
    // `get_watch` returns the current value and a Watch armed on the same snapshot;
    // the Watch fires on the next published change.
    let r0 = build([(b"a", 1)]);
    let (value, w) = r0.get_watch(b"a".as_slice());
    assert_eq!(value, Some(&1));

    let mut t = r0.txn();
    t.insert(b"a".as_slice(), 2);
    let r1 = t.commit();
    r1.notify_changes_since(&r0);
    assert!(w.wait_timeout(FIRED));
  }

  #[test]
  #[cfg_attr(miri, ignore = "blocks a real thread on a wall-clock timeout")]
  fn watch_wakes_a_blocked_thread() {
    use std::sync::mpsc;
    let r0 = build([(b"k", 1)]);
    let w = r0.watch(b"k".as_slice());
    let (tx, rx) = mpsc::channel();
    let h = std::thread::spawn(move || {
      w.wait();
      tx.send(()).unwrap();
    });
    let mut t = r0.txn();
    t.insert(b"k".as_slice(), 2);
    let r1 = t.commit();
    r1.notify_changes_since(&r0);
    rx.recv_timeout(FIRED)
      .expect("watcher thread should have woken");
    h.join().unwrap();
  }

  #[test]
  fn notify_fires_all_changed() {
    // One publish-time notify must wake EVERY changed key. The diff collects all
    // changed slots and then fires them, so a single `notify_changes_since` covering
    // three changed keys wakes all three watches.
    let base = build([(b"a", 1), (b"b", 2), (b"c", 3)]);
    let wa = base.watch(b"a".as_slice());
    let wb = base.watch(b"b".as_slice());
    let wc = base.watch(b"c".as_slice());

    let mut t = base.txn();
    t.insert(b"a".as_slice(), 11);
    t.insert(b"b".as_slice(), 22);
    t.insert(b"c".as_slice(), 33);
    let r1 = t.commit();
    r1.notify_changes_since(&base);

    assert!(wa.wait_timeout(FIRED));
    assert!(wb.wait_timeout(FIRED));
    assert!(wc.wait_timeout(FIRED));
  }

  #[test]
  fn net_empty_noop_stays_quiet() {
    // A net-empty txn (insert-then-remove the same key from an empty base) commits
    // `len == 0`; canonicalizing logical-empty to physical-None keeps it a None ->
    // None transition, so it publishes nothing and wakes no empty watcher. A LATER
    // real insert must still wake a watch armed on the ORIGINAL empty version
    // (per-epoch carry). `wait_timeout` consumes a `Watch`, so arm two on `r0`.
    let r0: Trie = Radix::new();
    let k = b"k".as_slice();
    let w_quiet = r0.watch(k); // checks the net-empty no-op stays quiet
    let w_fire = r0.watch(k); // checks the later real insert still fires

    // Net-empty no-op: insert then remove k from the empty base.
    let mut t = r0.txn();
    t.insert(k, 1);
    assert_eq!(t.remove(k), Some(1));
    assert!(t.is_empty());
    let r1 = t.commit();
    r1.notify_changes_since(&r0); // None -> None: fires nothing
    assert!(
      !w_quiet.wait_timeout(QUIET),
      "a net-empty no-op publishes nothing and must stay quiet"
    );

    // A real insert against the (still-empty) r1 must wake the watch armed on r0.
    let mut t2 = r1.txn();
    t2.insert(k, 9);
    let r2 = t2.commit();
    r2.notify_changes_since(&r1); // empty -> non-empty: fires r1.empty (== r0.empty)
    assert!(
      w_fire.wait_timeout(FIRED),
      "a real insert must wake the watch armed on the original empty version"
    );
  }

  // A deep NODE chain: keys "a", "aa", "aaa", ... — each one component longer gives an
  // N-node chain. These tests isolate the publish-time NOTIFY walk's stack usage:
  //
  // - The chain is BUILT on a large-stack thread, because the crate's `insert` (and,
  //   separately, the chain's recursive `Drop`) recurse on node-depth — neither is
  //   what is under test here, so both are given headroom.
  // - The `notify_changes_since` under test runs on a deliberately SMALL stack, where
  //   a recursive collect walk of this depth would overflow but the iterative one
  //   (heap `Vec` stack, ~constant native stack) completes. This is the actual guard:
  //   the notify walk adds no node-depth recursion of its own.
  // - The deep trees are dropped on a large-stack thread, so the recursive `Drop`
  //   (a pre-existing structural property, out of scope here) cannot confound the
  //   small-stack notify assertion.
  //
  // N is far deeper than the small notify stack could survive recursively, yet small
  // enough that the O(N^2) build stays fast.
  const DEEP_N: usize = 6_000;
  // Ample headroom for the recursive `insert`/`Drop` on the chain.
  const BIG_STACK: usize = 64 * 1024 * 1024;
  // Small enough that a depth-`DEEP_N` recursive collect walk (frames well over 100
  // bytes each → far more than this) would overflow it; the iterative walk uses the
  // heap, so it is unaffected.
  const SMALL_STACK: usize = 128 * 1024;

  fn on_stack<T: Send + 'static>(stack: usize, f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
      .stack_size(stack)
      .spawn(f)
      .expect("spawn helper thread")
      .join()
      .expect("helper thread")
  }

  fn build_deep_chain(n: usize) -> Trie {
    on_stack(BIG_STACK, move || {
      let mut t = Radix::<u8, u32>::new().txn();
      let mut key: Vec<u8> = Vec::with_capacity(n);
      for i in 0..n {
        key.push(b'a');
        t.insert(key.as_slice(), i as u32);
      }
      t.commit()
    })
  }

  // Drops a deep tree on a large-stack thread, so its recursive `Drop` cannot
  // overflow the (small) test stack and confound the assertion under test.
  fn drop_deep(tree: Trie) {
    on_stack(BIG_STACK, move || drop(tree));
  }

  #[test]
  #[cfg_attr(miri, ignore = "deep chain is prohibitively slow under miri")]
  fn deep_clear_notifies_without_overflow() {
    // A deep Some -> None transition (clear the whole trie) fires the removed
    // subtree. `clear`/`commit` do not recurse on depth, so the only node-depth walk
    // is the NOTIFY collect — run it on a small stack to prove it is iterative.
    let base = build_deep_chain(DEEP_N);
    // The root node is the shallowest; an empty-key prefix watch covers the whole
    // trie and fires on the root's removal.
    let w = base.watch_prefix(b"".as_slice());

    let mut t = base.txn();
    t.clear();
    let r1 = t.commit();

    let base_for_notify = base.clone();
    // Notify on a SMALL stack: a recursive collect of this depth would overflow here.
    on_stack(SMALL_STACK, move || {
      r1.notify_changes_since(&base_for_notify);
      drop(r1); // r1 is shallow (empty), cheap to drop here
    });

    assert!(
      w.wait_timeout(FIRED),
      "clearing a deep trie must notify without overflowing"
    );
    drop_deep(base);
  }

  #[test]
  #[cfg_attr(miri, ignore = "deep chain is prohibitively slow under miri")]
  fn deep_changed_path_notifies() {
    // Changing the DEEPEST value forces the collect Pair walk all the way down the
    // chain; a watch on the deepest key must fire, proving the diff descends to full
    // depth without recursion-driven overflow.
    let base = build_deep_chain(DEEP_N);
    let deepest: Vec<u8> = vec![b'a'; DEEP_N];
    // Arming is iterative (`watch_slot` loops), so it is fine on the small/default stack.
    let w = base.watch(deepest.as_slice());

    // `insert` recurses on depth, so the deep mutation + commit run on a large stack.
    let r1 = {
      let base = base.clone();
      let deepest = deepest.clone();
      on_stack(BIG_STACK, move || {
        let mut t = base.txn();
        assert_eq!(t.insert(deepest.as_slice(), 7), Some((DEEP_N - 1) as u32));
        t.commit()
      })
    };

    let base_for_notify = base.clone();
    // Notify on a SMALL stack: the iterative Pair descent to the deepest node must
    // complete where a recursive one would overflow.
    let r1 = on_stack(SMALL_STACK, move || {
      r1.notify_changes_since(&base_for_notify);
      r1 // hand r1 back so it is dropped on a large stack, not here
    });

    assert!(
      w.wait_timeout(FIRED),
      "changing the deepest value must notify without overflowing"
    );
    drop_deep(base);
    drop_deep(r1);
  }
}

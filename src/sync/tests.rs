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

// ----- one-op `&self` mutators return a NEW tree (persistence) -------------

#[test]
fn new_is_empty_and_const() {
  const EMPTY: Trie = Radix::new();
  assert!(EMPTY.is_empty());

  let (t, prev) = EMPTY.insert(&bytes(b"abc"), 1);
  assert_eq!(prev, None);
  let (t, prev) = t.insert(&bytes(b"abd"), 2);
  assert_eq!(prev, None);
  assert_eq!(t.get(b"abc".as_slice()), Some(&1));
  assert_eq!(t.get(b"abd".as_slice()), Some(&2));
  assert_eq!(t.get(b"ab".as_slice()), None);
  assert_eq!(t.len(), 2);
}

#[test]
fn one_op_insert_leaves_original_unchanged() {
  let base = build([(b"a", 1), (b"b", 2)]);
  let (next, prev) = base.insert(&bytes(b"a"), 100);
  assert_eq!(prev, Some(1));
  // The original is frozen; only the returned tree sees the change.
  assert_eq!(base.get(b"a".as_slice()), Some(&1));
  assert_eq!(base.len(), 2);
  assert_eq!(next.get(b"a".as_slice()), Some(&100));
  assert_eq!(next.len(), 2);
}

#[test]
fn one_op_remove_and_clear_persist() {
  let base = build([(b"a", 1), (b"ab", 2), (b"b", 3)]);

  let (after_remove, removed) = base.remove(b"a".as_slice());
  assert_eq!(removed, Some(1));
  assert_eq!(base.get(b"a".as_slice()), Some(&1)); // original intact
  assert_eq!(after_remove.get(b"a".as_slice()), None);
  assert_eq!(after_remove.len(), 2);

  let empty = base.clear();
  assert!(empty.is_empty());
  assert_eq!(base.len(), 3); // clear() returns a NEW empty tree
}

#[test]
fn one_op_prefix_ops_persist() {
  let base = build([(b"x", 1), (b"x/y", 2), (b"x/y/z", 3), (b"y", 9)]);

  let (kept, n) = base.remove_descendants(b"x".as_slice());
  assert_eq!(n, 2);
  assert_eq!(kept.get(b"x".as_slice()), Some(&1)); // strict: self kept
  assert_eq!(kept.len(), 2);
  assert!(kept.is_canonical());

  let (nuked, n) = base.delete_prefix(b"x".as_slice());
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
fn one_op_drain_returns_values_and_persists() {
  let base = build([(b"a", 1), (b"a/b", 2), (b"a/b/c", 3)]);

  let (after, mut drained) = base.drain_descendants(b"a".as_slice());
  drained.sort_unstable();
  assert_eq!(drained, vec![2, 3]);
  assert_eq!(after.get(b"a".as_slice()), Some(&1));
  assert_eq!(after.get(b"a/b".as_slice()), None);
  // base keeps everything (drain clones values out; original tree unchanged).
  assert_eq!(base.len(), 3);
  assert_eq!(base.get(b"a/b/c".as_slice()), Some(&3));

  let (after_prefix, drained) = base.drain_prefix(b"a".as_slice());
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
  let (live, _) = snap.insert(&bytes(b"k"), 2);
  let (live, _) = live.insert(&bytes(b"k2"), 3);
  assert_eq!(snap.get(b"k".as_slice()), Some(&1));
  assert_eq!(snap.get(b"k2".as_slice()), None);
  assert_eq!(live.get(b"k".as_slice()), Some(&2));
}

#[test]
fn unchanged_subtree_is_shared() {
  let snap = build([(b"a/x", 1), (b"b/y", 2)]);
  let (live, _) = snap.insert(&bytes(b"a/z"), 3); // touches only "a"

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

  let (live, mut drained) = snap.drain_descendants(b"a".as_slice());
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
  let (t, n) = base.remove_descendants(b"a".as_slice());
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

proptest! {
  // The sequence of ops is applied through one transaction (read-your-writes),
  // matching the in-place reference model; this also exercises `Txn` end to end.
  #[test]
  fn model_matches_reference(ops in prop::collection::vec(op_strategy(), 0..60)) {
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
fn one_op_insert_preserves_each_prior_version() {
  // A chain of one-op inserts: every intermediate tree stays frozen at its own
  // contents — persistence across the whole history, not just the last snapshot.
  let v0: Trie = Radix::new();
  let (v1, _) = v0.insert(&bytes(b"a"), 1);
  let (v2, _) = v1.insert(&bytes(b"b"), 2);
  let (v3, _) = v2.insert(&bytes(b"c"), 3);

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

  let (keep, n) = base.remove_descendants(b"x".as_slice());
  assert_eq!(n, 2);
  assert_eq!(keep.get(b"x".as_slice()), Some(&1)); // kept
  assert_eq!(keep.len(), 2);
  assert!(keep.is_canonical());

  let (nuke, n) = base.delete_prefix(b"x".as_slice());
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
  let (live, drained) = snap.drain_prefix(b"a".as_slice());
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
  #[test]
  fn ordered_ops_match_model(
    entries in prop::collection::vec((key_strategy(), 0u32..1000), 0..40),
    lo_k in key_strategy(),
    hi_k in key_strategy(),
    del in key_strategy(),
  ) {
    // Build immutably from the entries, then exercise ordered reads and the
    // one-op `drain_prefix` against the reference model.
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
    let (drained_trie, drained) = trie.drain_prefix(del.as_slice());
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

  // Building the inserted key's components clones its 3 elements first; the very
  // next clone is the split's head label. Arm to fire there.
  KEY_FUSE.with(|c| c.set(Some(3)));
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

  // Building the 2-element key clones 2 KeyBombs (KEY_FUSE disarmed). Arm the VALUE
  // fuse on the first value clone: the COW copy of the retained ancestor `[1]`.
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

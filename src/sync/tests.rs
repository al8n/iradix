use super::*;
use archery::SharedPointer;
use proptest::prelude::*;
use std::{collections::BTreeMap, vec, vec::Vec};

type Trie = Radix<u8, u32>;

fn bytes(s: &[u8]) -> Vec<u8> {
  s.to_vec()
}

// ----- Send/Sync contract (auto-derived, none explicit) -------------------

const fn assert_send<T: Send>() {}
const fn assert_sync<T: Sync>() {}

#[test]
fn sync_radix_is_send_sync() {
  // `sync::Radix` (ArcK) must be Send + Sync when its components and values are.
  // These are AUTO-derived (the crate has no explicit `unsafe impl Send/Sync`).
  assert_send::<Trie>();
  assert_sync::<Trie>();
  assert_send::<Radix<char, std::string::String>>();
  assert_sync::<Radix<char, std::string::String>>();
}

// ----- basic round-trip over the Arc face ---------------------------------

#[test]
fn new_is_empty_and_const() {
  const EMPTY: Trie = Radix::new();
  assert!(EMPTY.is_empty());
  let mut t: Trie = Radix::new();
  assert_eq!(t.insert(&bytes(b"abc"), 1), None);
  assert_eq!(t.insert(&bytes(b"abd"), 2), None);
  assert_eq!(t.get(b"abc".as_slice()), Some(&1));
  assert_eq!(t.get(b"abd".as_slice()), Some(&2));
  assert_eq!(t.get(b"ab".as_slice()), None);
  assert_eq!(t.len(), 2);
}

#[test]
fn ancestors_inclusive_and_strict() {
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a"), 1);
  t.insert(&bytes(b"abc"), 3);
  assert_eq!(t.get_ancestor(b"abc".as_slice()), Some(&3)); // inclusive
  assert_eq!(t.get_ancestor(b"abcd".as_slice()), Some(&3));
  assert_eq!(t.strict_ancestor(b"abc".as_slice()), Some(&1)); // exclusive
  assert!(t.has_ancestor(b"abc".as_slice()));
  assert!(!t.has_ancestor(b"x".as_slice()));
}

#[test]
fn iterators_over_arc_face() {
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a"), 1);
  t.insert(&bytes(b"ab"), 2);
  t.insert(&bytes(b"abc"), 3);
  t.insert(&bytes(b"b"), 4);

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

// ----- snapshot isolation & structural sharing (Arc) ----------------------

#[test]
fn clone_is_snapshot_isolated() {
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"k"), 1);
  let snap = t.clone();
  t.insert(&bytes(b"k"), 2);
  t.insert(&bytes(b"k2"), 3);
  assert_eq!(snap.get(b"k".as_slice()), Some(&1));
  assert_eq!(snap.get(b"k2".as_slice()), None);
  assert_eq!(t.get(b"k".as_slice()), Some(&2));
}

#[test]
fn unchanged_subtree_is_shared() {
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a/x"), 1);
  t.insert(&bytes(b"b/y"), 2);
  let snap = t.clone();
  t.insert(&bytes(b"a/z"), 3); // touches only "a"

  let snap_b = snap.edge_child(&b'b').expect("b edge");
  let t_b = t.edge_child(&b'b').expect("b edge");
  assert!(
    SharedPointer::ptr_eq(&snap_b, &t_b),
    "the untouched b subtree must be shared"
  );
  let snap_a = snap.edge_child(&b'a').expect("a edge");
  let t_a = t.edge_child(&b'a').expect("a edge");
  assert!(!SharedPointer::ptr_eq(&snap_a, &t_a));
}

#[test]
fn drain_shared_subtree_clones_not_moves() {
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a"), 1);
  t.insert(&bytes(b"a/b"), 2);
  t.insert(&bytes(b"a/b/c"), 3);
  let snap = t.clone(); // shares the whole "a" subtree

  let mut drained = t.drain_descendants(b"a".as_slice());
  drained.sort_unstable();
  assert_eq!(drained, vec![2, 3]);
  assert_eq!(t.get(b"a".as_slice()), Some(&1));
  assert_eq!(t.get(b"a/b".as_slice()), None);

  // The snapshot still has everything (values were cloned, not stolen).
  assert_eq!(snap.get(b"a".as_slice()), Some(&1));
  assert_eq!(snap.get(b"a/b".as_slice()), Some(&2));
  assert_eq!(snap.get(b"a/b/c".as_slice()), Some(&3));
  assert_eq!(snap.len(), 3);
}

#[test]
fn remove_descendants_keeps_self() {
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a"), 1);
  t.insert(&bytes(b"ab"), 2);
  t.insert(&bytes(b"ac"), 3);
  t.insert(&bytes(b"b"), 4);
  assert_eq!(t.remove_descendants(b"a".as_slice()), 2);
  assert_eq!(t.get(b"a".as_slice()), Some(&1));
  assert_eq!(t.get(b"b".as_slice()), Some(&4));
  assert_eq!(t.len(), 2);
  assert!(t.is_canonical());
}

#[test]
fn cross_thread_snapshot() {
  // A `sync::Radix` snapshot can be moved into another thread and read there.
  use std::thread;
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a/b/c"), 7);
  let snap = t.clone();
  let h = thread::spawn(move || snap.get(b"a/b/c".as_slice()).copied());
  assert_eq!(h.join().unwrap(), Some(7));
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
  #[test]
  fn model_matches_reference(ops in prop::collection::vec(op_strategy(), 0..60)) {
    let mut trie: Trie = Radix::new();
    let mut model: BTreeMap<Vec<u8>, u32> = BTreeMap::new();
    for op in ops {
      match op {
        Op::Insert(k, v) => {
          prop_assert_eq!(trie.insert(&k.clone(), v), model.insert(k, v));
        }
        Op::Remove(k) => {
          prop_assert_eq!(trie.remove(k.as_slice()), model.remove(&k));
        }
        Op::RemoveDescendants(k) => {
          prop_assert_eq!(
            trie.remove_descendants(k.as_slice()),
            model_remove_descendants(&mut model, &k)
          );
        }
      }
      prop_assert_eq!(trie.len(), model.len());
      prop_assert_eq!(trie.len(), trie.count_values());
      prop_assert!(trie.is_canonical());
      // containment: inserting a parent keeps every stored child subsumed
      for (k, v) in &model {
        prop_assert_eq!(trie.get(k.as_slice()), Some(v));
      }
    }
  }
}

// ----- go-immutable-radix parity (Arc face) -------------------------------

use core::ops::Bound;

#[test]
fn min_max_empty_and_populated() {
  let empty: Trie = Radix::new();
  assert_eq!(empty.minimum(), None);
  assert_eq!(empty.maximum(), None);

  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a"), 1);
  t.insert(&bytes(b"abc"), 2);
  t.insert(&bytes(b"abd"), 3);
  t.insert(&bytes(b"b"), 4);
  assert_eq!(t.minimum(), Some((bytes(b"a"), &1)));
  assert_eq!(t.maximum(), Some((bytes(b"b"), &4)));
}

#[test]
fn delete_prefix_is_node_inclusive_and_vs_remove_descendants() {
  let mut keep: Trie = Radix::new();
  let mut nuke: Trie = Radix::new();
  for t in [&mut keep, &mut nuke] {
    t.insert(&bytes(b"x"), 1);
    t.insert(&bytes(b"x/y"), 2);
    t.insert(&bytes(b"x/y/z"), 3);
    t.insert(&bytes(b"y"), 9);
  }
  assert_eq!(keep.remove_descendants(b"x".as_slice()), 2);
  assert_eq!(keep.get(b"x".as_slice()), Some(&1)); // kept
  assert_eq!(keep.len(), 2);
  assert!(keep.is_canonical());

  assert_eq!(nuke.delete_prefix(b"x".as_slice()), 3);
  assert_eq!(nuke.get(b"x".as_slice()), None); // removed
  assert_eq!(nuke.get(b"y".as_slice()), Some(&9));
  assert_eq!(nuke.len(), 1);
  assert!(nuke.is_canonical());
}

#[test]
fn drain_prefix_ascending_and_shared_clones() {
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a"), 1);
  t.insert(&bytes(b"a/b"), 2);
  t.insert(&bytes(b"a/c"), 3);
  let snap = t.clone();
  // value at key first, then strict descendants ascending
  assert_eq!(t.drain_prefix(b"a".as_slice()), vec![1, 2, 3]);
  assert!(t.is_empty());
  // snapshot intact (values cloned, not stolen)
  assert_eq!(snap.get(b"a".as_slice()), Some(&1));
  assert_eq!(snap.get(b"a/b".as_slice()), Some(&2));
  assert_eq!(snap.get(b"a/c".as_slice()), Some(&3));
  assert_eq!(snap.len(), 3);
}

#[test]
fn reverse_iteration() {
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a"), 1);
  t.insert(&bytes(b"ab"), 2);
  t.insert(&bytes(b"abc"), 3);
  t.insert(&bytes(b"b"), 4);
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
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a"), 1);
  t.insert(&bytes(b"b"), 2);
  t.insert(&bytes(b"c"), 3);
  t.insert(&bytes(b"d"), 4);

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
    let mut trie: Trie = Radix::new();
    let mut model: BTreeMap<Vec<u8>, u32> = BTreeMap::new();
    for (k, v) in entries {
      trie.insert(&k.clone(), v);
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
    let drained = trie.drain_prefix(del.as_slice());
    let want_drained: Vec<u32> = model
      .iter()
      .filter(|(k, _)| k.starts_with(&del))
      .map(|(_, v)| *v)
      .collect();
    prop_assert_eq!(&drained, &want_drained);
    let removed = model_delete_prefix(&mut model, &del);
    prop_assert_eq!(drained.len(), removed);
    prop_assert_eq!(trie.len(), model.len());
    prop_assert_eq!(trie.len(), trie.count_values());
    prop_assert!(trie.is_canonical());
    for (k, v) in &model {
      prop_assert_eq!(trie.get(k.as_slice()), Some(v));
    }
  }
}

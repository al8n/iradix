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

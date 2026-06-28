use super::*;
use archery::SharedPointer;
use proptest::prelude::*;
use std::{collections::BTreeMap, vec, vec::Vec};

type Trie = LocalRadix<u8, u32>;

fn bytes(s: &[u8]) -> Vec<u8> {
  s.to_vec()
}

// ----- Send/Sync contract (auto-derived, none explicit) -------------------

const fn assert_send<T: Send>() {}
const fn assert_sync<T: Sync>() {}

#[test]
fn sync_radix_is_send_sync() {
  // SyncRadix (ArcK) must be Send + Sync when its components and values are.
  assert_send::<SyncRadix<u8, u32>>();
  assert_sync::<SyncRadix<u8, u32>>();
  // A non-Send value makes it non-Send (Arc<T>: Send requires T: Send), but
  // Send/Sync are auto-derived, never declared — there is nothing to assert
  // negatively here without an extra dependency. LocalRadix (RcK) is !Send.
}

// ----- unit tests: basic round-trip ---------------------------------------

#[test]
fn new_is_empty() {
  let t: Trie = Radix::new();
  assert!(t.is_empty());
  assert_eq!(t.len(), 0);
  assert_eq!(t.get(b"x".as_slice()), None);
}

#[test]
fn new_is_const() {
  // `new` must be usable in a const context (no allocation until first insert).
  const EMPTY: Trie = Radix::new();
  assert!(EMPTY.is_empty());
}

#[test]
fn empty_trie_reads_are_safe() {
  let t: Trie = Radix::new();
  assert_eq!(t.get(b"x".as_slice()), None);
  assert_eq!(t.get_ancestor(b"x".as_slice()), None);
  assert_eq!(t.strict_ancestor(b"x".as_slice()), None);
  assert!(!t.has_ancestor(b"x".as_slice()));
  assert_eq!(t.values().count(), 0);
  assert_eq!(t.ancestors(b"x".as_slice()).count(), 0);
  assert_eq!(t.descendants(b"x".as_slice()).count(), 0);
}

#[test]
fn remove_and_drain_on_empty_trie() {
  let mut t: Trie = Radix::new();
  assert_eq!(t.remove(b"x".as_slice()), None);
  assert_eq!(t.remove_descendants(b"x".as_slice()), 0);
  assert!(t.drain_descendants(b"x".as_slice()).is_empty());
  assert!(t.is_empty());
}

#[test]
fn insert_get_roundtrip() {
  let mut t: Trie = Radix::new();
  assert_eq!(t.insert(&bytes(b"abc"), 1), None);
  assert_eq!(t.insert(&bytes(b"abd"), 2), None);
  assert_eq!(t.insert(&bytes(b"ab"), 3), None);
  assert_eq!(t.get(b"abc".as_slice()), Some(&1));
  assert_eq!(t.get(b"abd".as_slice()), Some(&2));
  assert_eq!(t.get(b"ab".as_slice()), Some(&3));
  assert_eq!(t.get(b"a".as_slice()), None);
  assert_eq!(t.len(), 3);
}

#[test]
fn insert_overwrites_returns_old() {
  let mut t: Trie = Radix::new();
  assert_eq!(t.insert(&bytes(b"k"), 1), None);
  assert_eq!(t.insert(&bytes(b"k"), 2), Some(1));
  assert_eq!(t.get(b"k".as_slice()), Some(&2));
  assert_eq!(t.len(), 1);
}

#[test]
fn empty_key_insert_get() {
  let mut t: Trie = Radix::new();
  assert_eq!(t.insert(&bytes(b""), 7), None);
  assert_eq!(t.get(b"".as_slice()), Some(&7));
  assert!(t.contains(b"".as_slice()));
  assert_eq!(t.len(), 1);
}

// ----- unit tests: split -------------------------------------------------

#[test]
fn common_prefix_split() {
  // Insert "abc" then "abd": the shared "ab" edge must split.
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"abc"), 1);
  t.insert(&bytes(b"abd"), 2);
  assert_eq!(t.get(b"abc".as_slice()), Some(&1));
  assert_eq!(t.get(b"abd".as_slice()), Some(&2));
  // A non-leaf split node at "ab" holds no value.
  assert_eq!(t.get(b"ab".as_slice()), None);
}

#[test]
fn split_with_key_as_prefix_of_existing() {
  // Insert "abc", then "a": the edge "abc" splits at "a".
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"abc"), 1);
  t.insert(&bytes(b"a"), 2);
  assert_eq!(t.get(b"abc".as_slice()), Some(&1));
  assert_eq!(t.get(b"a".as_slice()), Some(&2));
  assert_eq!(t.get(b"ab".as_slice()), None);
}

// ----- unit tests: merge -------------------------------------------------

#[test]
fn single_child_merge_on_remove() {
  // "ab" + "abc": removing "ab" leaves a single child; the node must merge so
  // "abc" is still one compressed edge from the root.
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"ab"), 1);
  t.insert(&bytes(b"abc"), 2);
  assert_eq!(t.remove(b"ab".as_slice()), Some(1));
  assert_eq!(t.get(b"abc".as_slice()), Some(&2));
  assert_eq!(t.get(b"ab".as_slice()), None);
  assert_eq!(t.len(), 1);
  // After the merge there is exactly one edge from the root, labelled "abc".
  assert_eq!(t.root_child_count(), 1);
}

#[test]
fn root_is_never_merged() {
  // A single entry under the root: the root stays as a (valueless) root with one
  // child; it must not collapse into its child.
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"abc"), 1);
  assert_eq!(t.root_child_count(), 1);
  assert!(!t.root_has_value());
}

#[test]
fn remove_prunes_and_merges_chain() {
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a"), 1);
  t.insert(&bytes(b"ab"), 2);
  t.insert(&bytes(b"abc"), 3);
  assert_eq!(t.remove(b"ab".as_slice()), Some(2));
  // "a" and "abc" remain; the chain a -> ab -> abc collapses to a -> abc.
  assert_eq!(t.get(b"a".as_slice()), Some(&1));
  assert_eq!(t.get(b"abc".as_slice()), Some(&3));
  assert_eq!(t.len(), 2);
}

#[test]
fn remove_nonexistent_is_none() {
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"abc"), 1);
  assert_eq!(t.remove(b"abd".as_slice()), None);
  assert_eq!(t.remove(b"ab".as_slice()), None);
  assert_eq!(t.remove(b"abcd".as_slice()), None);
  assert_eq!(t.len(), 1);
}

// ----- ancestors ---------------------------------------------------------

#[test]
fn get_ancestor_is_inclusive() {
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a"), 1);
  t.insert(&bytes(b"abc"), 3);
  // exact match counts as its own ancestor
  assert_eq!(t.get_ancestor(b"abc".as_slice()), Some(&3));
  // longest covered prefix
  assert_eq!(t.get_ancestor(b"abcd".as_slice()), Some(&3));
  assert_eq!(t.get_ancestor(b"ab".as_slice()), Some(&1));
  assert_eq!(t.get_ancestor(b"a".as_slice()), Some(&1));
  assert_eq!(t.get_ancestor(b"x".as_slice()), None);
}

#[test]
fn strict_ancestor_excludes_exact() {
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a"), 1);
  t.insert(&bytes(b"abc"), 3);
  assert_eq!(t.strict_ancestor(b"abc".as_slice()), Some(&1));
  assert_eq!(t.strict_ancestor(b"abcd".as_slice()), Some(&3));
  assert_eq!(t.strict_ancestor(b"a".as_slice()), None);
}

#[test]
fn has_ancestor() {
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a"), 1);
  assert!(t.has_ancestor(b"abc".as_slice()));
  assert!(t.has_ancestor(b"a".as_slice()));
  assert!(!t.has_ancestor(b"x".as_slice()));
}

// ----- descendants / bulk -------------------------------------------------

#[test]
fn remove_descendants_keeps_self() {
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a"), 1);
  t.insert(&bytes(b"ab"), 2);
  t.insert(&bytes(b"ac"), 3);
  t.insert(&bytes(b"b"), 4);
  let removed = t.remove_descendants(b"a".as_slice());
  assert_eq!(removed, 2); // ab, ac
  assert_eq!(t.get(b"a".as_slice()), Some(&1)); // self kept
  assert_eq!(t.get(b"ab".as_slice()), None);
  assert_eq!(t.get(b"ac".as_slice()), None);
  assert_eq!(t.get(b"b".as_slice()), Some(&4));
  assert_eq!(t.len(), 2);
}

#[test]
fn drain_shared_subtree_clones_not_moves() {
  // When the drained subtree is shared with a snapshot, drain must clone values
  // out (not move), leaving the snapshot intact.
  let mut t: SyncRadix<u8, u32> = Radix::new();
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
fn drain_descendants_returns_values() {
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a"), 1);
  t.insert(&bytes(b"ab"), 2);
  t.insert(&bytes(b"abc"), 3);
  let mut drained = t.drain_descendants(b"a".as_slice());
  drained.sort_unstable();
  assert_eq!(drained, vec![2, 3]);
  assert_eq!(t.get(b"a".as_slice()), Some(&1));
  assert_eq!(t.len(), 1);
}

#[test]
fn descendants_of_nonexistent_prefix() {
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"abc"), 1);
  // "x" matches nothing
  assert_eq!(t.remove_descendants(b"x".as_slice()), 0);
  // "ab" is a prefix that ends mid-edge; "abc" is a strict descendant
  assert_eq!(t.remove_descendants(b"ab".as_slice()), 1);
  assert_eq!(t.get(b"abc".as_slice()), None);
}

#[test]
fn values_and_ancestors_descendants_iter() {
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
  assert_eq!(anc, vec![1, 2, 3]); // inclusive

  let mut desc: Vec<u32> = t.descendants(b"a".as_slice()).copied().collect();
  desc.sort_unstable();
  assert_eq!(desc, vec![2, 3]); // strict descendants
}

#[test]
fn clear_empties() {
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a"), 1);
  t.insert(&bytes(b"ab"), 2);
  t.clear();
  assert!(t.is_empty());
  assert_eq!(t.get(b"a".as_slice()), None);
}

// ----- str vs Vec<char> aliasing -----------------------------------------

#[test]
fn str_and_vec_char_keys_alias() {
  let mut t: LocalRadix<char, u32> = Radix::new();
  t.insert("héllo", 1);
  let v: Vec<char> = "héllo".chars().collect();
  assert_eq!(t.get(&v), Some(&1));
  // and the reverse
  let mut t2: LocalRadix<char, u32> = Radix::new();
  let key: Vec<char> = "ab".chars().collect();
  t2.insert(&key, 5);
  assert_eq!(t2.get("ab"), Some(&5));
}

// ----- snapshot isolation & structural sharing ----------------------------

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
  // Insert two disjoint subtrees, snapshot, then mutate only one. The untouched
  // subtree must remain physically shared (same SharedPointer) with the snapshot.
  let mut t: SyncRadix<u8, u32> = Radix::new();
  t.insert(&bytes(b"a/x"), 1);
  t.insert(&bytes(b"b/y"), 2);
  let snap = t.clone();
  t.insert(&bytes(b"a/z"), 3); // touches only the "a" subtree

  let snap_b = snap.edge_child(b'b').expect("b edge");
  let t_b = t.edge_child(b'b').expect("b edge");
  assert!(
    SharedPointer::ptr_eq(&snap_b, &t_b),
    "the untouched b subtree must be shared"
  );
  // the mutated "a" subtree must have diverged
  let snap_a = snap.edge_child(b'a').expect("a edge");
  let t_a = t.edge_child(b'a').expect("a edge");
  assert!(!SharedPointer::ptr_eq(&snap_a, &t_a));
}

#[test]
fn noop_remove_preserves_sharing() {
  // A remove of an absent key must not disturb structural sharing (no needless
  // copy-on-write of the path).
  let mut t: SyncRadix<u8, u32> = Radix::new();
  t.insert(&bytes(b"a/x"), 1);
  t.insert(&bytes(b"b/y"), 2);
  let snap = t.clone();

  assert_eq!(t.remove(b"a/zzz".as_slice()), None); // absent
  assert_eq!(t.remove(b"nope".as_slice()), None); // absent

  // Both subtrees must still be shared with the snapshot.
  for first in *b"ab" {
    let s = snap.edge_child(first).expect("edge");
    let l = t.edge_child(first).expect("edge");
    assert!(
      SharedPointer::ptr_eq(&s, &l),
      "no-op remove must not break sharing"
    );
  }
}

#[test]
fn noop_drain_preserves_sharing() {
  let mut t: SyncRadix<u8, u32> = Radix::new();
  t.insert(&bytes(b"a/x"), 1);
  t.insert(&bytes(b"b/y"), 2);
  let snap = t.clone();

  assert_eq!(t.remove_descendants(b"a/x".as_slice()), 0); // leaf: no descendants
  assert_eq!(t.remove_descendants(b"zzz".as_slice()), 0); // absent

  for first in *b"ab" {
    let s = snap.edge_child(first).expect("edge");
    let l = t.edge_child(first).expect("edge");
    assert!(
      SharedPointer::ptr_eq(&s, &l),
      "no-op drain must not break sharing"
    );
  }
}

#[test]
fn txn_one_publish_atomic() {
  let mut t: SyncRadix<u8, u32> = Radix::new();
  t.insert(&bytes(b"a"), 1);
  let snap = t.clone();
  let mut txn = t.txn();
  txn.insert(&bytes(b"a/b"), 2);
  txn.insert(&bytes(b"a/c"), 3);
  txn.remove_descendants(b"a".as_slice());
  txn.insert(&bytes(b"a"), 9);
  txn.commit();
  // snapshot taken before the txn is unaffected
  assert_eq!(snap.get(b"a".as_slice()), Some(&1));
  assert_eq!(snap.get(b"a/b".as_slice()), None);
  // committed state
  assert_eq!(t.get(b"a".as_slice()), Some(&9));
  assert_eq!(t.get(b"a/b".as_slice()), None);
  assert_eq!(t.len(), 1);
}

// ----- proptest model invariants -----------------------------------------

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

// Model: a BTreeMap with the same semantics.
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

fn model_get_ancestor<'a>(model: &'a BTreeMap<Vec<u8>, u32>, key: &[u8]) -> Option<&'a u32> {
  model
    .iter()
    .filter(|(k, _)| key.starts_with(k))
    .max_by_key(|(k, _)| k.len())
    .map(|(_, v)| v)
}

fn model_strict_ancestor<'a>(model: &'a BTreeMap<Vec<u8>, u32>, key: &[u8]) -> Option<&'a u32> {
  model
    .iter()
    .filter(|(k, _)| k.len() < key.len() && key.starts_with(k))
    .max_by_key(|(k, _)| k.len())
    .map(|(_, v)| v)
}

proptest! {
  #[test]
  fn model_matches_reference(ops in prop::collection::vec(op_strategy(), 0..60)) {
    let mut trie: Trie = Radix::new();
    let mut model: BTreeMap<Vec<u8>, u32> = BTreeMap::new();
    for op in ops {
      match op {
        Op::Insert(k, v) => {
          let trie_old = trie.insert(&k.clone(), v);
          let model_old = model.insert(k, v);
          prop_assert_eq!(trie_old, model_old);
        }
        Op::Remove(k) => {
          let trie_old = trie.remove(k.as_slice());
          let model_old = model.remove(&k);
          prop_assert_eq!(trie_old, model_old);
        }
        Op::RemoveDescendants(k) => {
          let n = trie.remove_descendants(k.as_slice());
          let m = model_remove_descendants(&mut model, &k);
          prop_assert_eq!(n, m);
        }
      }
      prop_assert_eq!(trie.len(), model.len());
      // the incrementally-tracked len matches a full structural walk
      prop_assert_eq!(trie.len(), trie.count_values());
      // the trie stays path-compressed after every operation
      prop_assert!(trie.is_canonical());
      // every model entry is retrievable; ancestors agree
      for (k, v) in &model {
        prop_assert_eq!(trie.get(k.as_slice()), Some(v));
      }
    }
    // exhaustive query agreement over the small key space
    for a in 0u8..6 {
      let key = [a, a];
      prop_assert_eq!(trie.get_ancestor(key.as_slice()), model_get_ancestor(&model, &key));
      prop_assert_eq!(trie.strict_ancestor(key.as_slice()), model_strict_ancestor(&model, &key));
    }
  }

  #[test]
  fn snapshot_isolation(ops in prop::collection::vec(op_strategy(), 0..40)) {
    let mut trie: Trie = Radix::new();
    for op in &ops {
      if let Op::Insert(k, v) = op { trie.insert(&k.clone(), *v); }
    }
    let snap = trie.clone();
    // Capture the snapshot's full contents before mutating.
    let before: Vec<(Vec<u8>, u32)> = {
      let mut m = Vec::new();
      for a in 0u8..6 {
        for b in 0u8..6 {
          for k in [vec![], vec![a], vec![a, b]] {
            if let Some(v) = snap.get(k.as_slice()) {
              m.push((k, *v));
            }
          }
        }
      }
      m.sort();
      m.dedup();
      m
    };
    // Mutate the live trie.
    for op in ops {
      match op {
        Op::Insert(k, v) => { trie.insert(&k, v); }
        Op::Remove(k) => { trie.remove(k.as_slice()); }
        Op::RemoveDescendants(k) => { trie.remove_descendants(k.as_slice()); }
      }
    }
    // The snapshot is unchanged.
    let after: Vec<(Vec<u8>, u32)> = {
      let mut m = Vec::new();
      for a in 0u8..6 {
        for b in 0u8..6 {
          for k in [vec![], vec![a], vec![a, b]] {
            if let Some(v) = snap.get(k.as_slice()) {
              m.push((k, *v));
            }
          }
        }
      }
      m.sort();
      m.dedup();
      m
    };
    prop_assert_eq!(before, after);
  }
}

use super::*;
use archery::SharedPointer;
use proptest::prelude::*;
use std::{collections::BTreeMap, vec, vec::Vec};

type Trie = Radix<u8, u32>;

fn bytes(s: &[u8]) -> Vec<u8> {
  s.to_vec()
}

// ----- !Send contract -----------------------------------------------------

#[test]
fn unsync_radix_is_not_send() {
  // `unsync::Radix` uses `Rc` internally, so it must be `!Send` / `!Sync`. There
  // is nothing to assert positively without a negative-trait dependency; the
  // contract is that the type is single-thread-confined (enforced by `Rc`). The
  // `Send + Sync` face lives in `crate::sync`.
  fn _confined<T>() {}
  _confined::<Trie>();
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
  let mut t: Radix<char, u32> = Radix::new();
  t.insert("héllo", 1);
  let v: Vec<char> = "héllo".chars().collect();
  assert_eq!(t.get(&v), Some(&1));
  // and the reverse
  let mut t2: Radix<char, u32> = Radix::new();
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
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a/x"), 1);
  t.insert(&bytes(b"b/y"), 2);
  let snap = t.clone();
  t.insert(&bytes(b"a/z"), 3); // touches only the "a" subtree

  let snap_b = snap.edge_child(&b'b').expect("b edge");
  let t_b = t.edge_child(&b'b').expect("b edge");
  assert!(
    SharedPointer::ptr_eq(&snap_b, &t_b),
    "the untouched b subtree must be shared"
  );
  // the mutated "a" subtree must have diverged
  let snap_a = snap.edge_child(&b'a').expect("a edge");
  let t_a = t.edge_child(&b'a').expect("a edge");
  assert!(!SharedPointer::ptr_eq(&snap_a, &t_a));
}

#[test]
fn noop_remove_preserves_sharing() {
  // A remove of an absent key must not disturb structural sharing (no needless
  // copy-on-write of the path).
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a/x"), 1);
  t.insert(&bytes(b"b/y"), 2);
  let snap = t.clone();

  assert_eq!(t.remove(b"a/zzz".as_slice()), None); // absent
  assert_eq!(t.remove(b"nope".as_slice()), None); // absent

  // Both subtrees must still be shared with the snapshot.
  for first in *b"ab" {
    let s = snap.edge_child(&first).expect("edge");
    let l = t.edge_child(&first).expect("edge");
    assert!(
      SharedPointer::ptr_eq(&s, &l),
      "no-op remove must not break sharing"
    );
  }
}

#[test]
fn noop_drain_preserves_sharing() {
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a/x"), 1);
  t.insert(&bytes(b"b/y"), 2);
  let snap = t.clone();

  assert_eq!(t.remove_descendants(b"a/x".as_slice()), 0); // leaf: no descendants
  assert_eq!(t.remove_descendants(b"zzz".as_slice()), 0); // absent

  for first in *b"ab" {
    let s = snap.edge_child(&first).expect("edge");
    let l = t.edge_child(&first).expect("edge");
    assert!(
      SharedPointer::ptr_eq(&s, &l),
      "no-op drain must not break sharing"
    );
  }
}

#[test]
fn batched_edits_are_isolated_from_snapshot() {
  // Single-threaded batching is just a sequence of direct `&mut` calls; a clone
  // taken before the batch is a fully isolated snapshot.
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a"), 1);
  let snap = t.clone();
  t.insert(&bytes(b"a/b"), 2);
  t.insert(&bytes(b"a/c"), 3);
  t.remove_descendants(b"a".as_slice());
  t.insert(&bytes(b"a"), 9);
  // snapshot taken before the batch is unaffected
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
      // every model entry is retrievable; containment holds (insert parent ⇒
      // children subsumed under the same prefix)
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

// ----- panic-safety: a Clone that panics must not corrupt the trie ---------
//
// Every mutation builds its new pieces (cloning `C::Owned` labels / `V` values)
// BEFORE detaching existing structure, so an unwind out of a user `Clone` leaves
// the trie structurally valid (every pre-existing key still resolves) and `len`
// accurate. These tests arm a `Clone` to panic on a chosen invocation, run the
// mutation under `catch_unwind`, and assert both invariants afterward.
//
// Panicking `Drop` is out of scope for the guarantee (see the crate docs): a
// destructor that panics while unwinding aborts, so there is no safe `DropBomb`
// regression to add — these fuses target `Clone` / `Ord`, not `Drop`.

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

/// A component (`C` = `C::Owned`) whose `Clone` panics on an armed invocation.
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
fn split_panic_in_label_clone_keeps_trie_consistent() {
  // Edge "[1,2,3]" from the root. Inserting "[1,2,4]" splits it at "[1,2]",
  // which clones the head/tail labels. A panic during that clone must not have
  // detached the original edge.
  let mut t: Radix<KeyBomb, u32> = Radix::new();
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
fn merge_moves_labels_and_returns_value() {
  // "[1,2]" + "[1,2,3]": removing "[1,2]" leaves a valueless single-child node
  // that merges its label with the grandchild's. The merge MOVES both labels (it
  // owns them — the child was un-shared on the way down), so it performs no
  // `C::Owned` clone and cannot panic: the removed value is always returned and
  // `len` stays accurate. Arming the label fuse on the very first clone proves the
  // merge clones nothing.
  let mut t: Radix<KeyBomb, u32> = Radix::new();
  t.insert(&key(&[1, 2]), 10);
  t.insert(&key(&[1, 2, 3]), 20);
  assert_eq!(t.len(), 2);

  // Arm past the 2 key-materialization clones: the next clone would be the merge's.
  KEY_FUSE.with(|c| c.set(Some(2)));
  let r = catch_unwind(AssertUnwindSafe(|| t.remove(key(&[1, 2]).as_slice())));
  KEY_FUSE.with(|c| c.set(None));

  assert!(r.is_ok(), "the merge must not clone, so it cannot panic");
  assert_eq!(r.unwrap(), Some(10), "the removed value is returned");
  assert_eq!(t.get(key(&[1, 2, 3]).as_slice()), Some(&20));
  assert_eq!(t.get(key(&[1, 2]).as_slice()), None);
  assert_eq!(t.len(), 1);
  assert_eq!(t.count_values(), 1);
  assert_eq!(t.root_child_count(), 1); // merged into a single edge
}

#[test]
fn drain_descendants_panic_in_value_clone_keeps_trie_intact() {
  // Draining clones every descendant value out FIRST; a panic there must leave
  // the trie and `len` completely untouched (nothing unlinked yet).
  let mut t: Radix<KeyBomb, ValBomb> = Radix::new();
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
fn remove_descendants_never_clones_values_and_survives_label_panic() {
  // `remove_descendants` is count-only: it must not clone any `V` (so a `V`
  // whose clone always panics is fine), and a `C::Owned` clone panic during
  // re-compression must still leave `len` accurate and survivors resolvable.
  let mut t: Radix<KeyBomb, ValBomb> = Radix::new();
  t.insert(&key(&[1]), ValBomb(1));
  t.insert(&key(&[1, 2]), ValBomb(2));
  t.insert(&key(&[1, 2, 3]), ValBomb(3));
  assert_eq!(t.len(), 3);

  // Arm the VALUE fuse to fire on the very first value clone: if the count-only
  // path ever clones a value, this test fails.
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
fn remove_descendants_merge_moves_labels_keeps_len() {
  // Shape: root -> [1](v) -> [2](valueless) -> { [3] -> {[30](v),[31](v)}, [4](v) }.
  // Removing the strict descendants of [1,2,3] empties [3] (pruned), dropping the
  // [2] node to a single child [4], which then merges ([2] ++ [4]). That merge
  // MOVES labels (no clone), so the removal cannot panic mid-recompression: `len`
  // is corrected and every survivor resolves. Arming the label fuse on the first
  // clone proves the merge clones nothing.
  let mut t: Radix<KeyBomb, u32> = Radix::new();
  t.insert(&key(&[1]), 1);
  t.insert(&key(&[1, 2, 3, 30]), 30);
  t.insert(&key(&[1, 2, 3, 31]), 31);
  t.insert(&key(&[1, 2, 4]), 4);
  assert_eq!(t.len(), 4);

  // Arm past the 3 key-materialization clones: the next clone would be the merge's.
  KEY_FUSE.with(|c| c.set(Some(3)));
  let r = catch_unwind(AssertUnwindSafe(|| {
    t.remove_descendants(key(&[1, 2, 3]).as_slice())
  }));
  KEY_FUSE.with(|c| c.set(None));

  assert_eq!(
    r.ok(),
    Some(2),
    "two strict descendants removed, no clone or panic"
  );
  assert_eq!(t.len(), 2);
  assert_eq!(t.count_values(), 2);
  assert_eq!(t.get(key(&[1]).as_slice()), Some(&1));
  assert_eq!(t.get(key(&[1, 2, 4]).as_slice()), Some(&4));
  assert_eq!(t.get(key(&[1, 2, 3, 30]).as_slice()), None);
}

#[test]
fn drain_descendants_merge_moves_labels_returns_values() {
  // Same shape as the remove_descendants merge case, but draining. The descendant
  // values are cloned out in phase 1; phase 2 then unlinks and re-compresses (the
  // [2] ++ [4] merge). That merge MOVES labels, so phase 2 performs no clone and
  // cannot panic — the drained values are never lost. The label fuse armed on the
  // first clone proves the post-unlink merge clones nothing.
  let mut t: Radix<KeyBomb, u32> = Radix::new();
  t.insert(&key(&[1]), 1);
  t.insert(&key(&[1, 2, 3, 30]), 30);
  t.insert(&key(&[1, 2, 3, 31]), 31);
  t.insert(&key(&[1, 2, 4]), 4);
  assert_eq!(t.len(), 4);

  // Arm past the 3 key-materialization clones: the next clone would be the merge's.
  KEY_FUSE.with(|c| c.set(Some(3)));
  let r = catch_unwind(AssertUnwindSafe(|| {
    t.drain_descendants(key(&[1, 2, 3]).as_slice())
  }));
  KEY_FUSE.with(|c| c.set(None));

  assert!(
    r.is_ok(),
    "the post-unlink merge must not clone, so it cannot panic"
  );
  let mut drained = r.unwrap();
  drained.sort_unstable();
  assert_eq!(drained, vec![30, 31], "both drained values are returned");
  assert_eq!(t.len(), 2);
  assert_eq!(t.count_values(), 2);
  assert_eq!(t.get(key(&[1]).as_slice()), Some(&1));
  assert_eq!(t.get(key(&[1, 2, 4]).as_slice()), Some(&4));
}

// ----- shared-snapshot panic safety (len order + split detach) -------------
//
// The fuses above fired during *post*-mutation re-compression. These cases force
// the failure point *earlier*: a SHARED trie makes `make_mut` clone each node on
// the path BEFORE the value is taken/unlinked, and a poisoned `Ord` makes an edge
// split fail at its ordering step. Both must leave `len` and the trie unchanged.

use core::cmp::Ordering;

/// A component whose `Ord` panics when either operand is poisoned; its `Clone`,
/// `PartialEq`, and `Eq` are ordinary. Poisoning only an inserted key's divergent
/// component makes a split's ordering comparison panic — after the child lookup
/// (which compares the shared first component) and the prefix check (which uses
/// `PartialEq`) have already run.
#[derive(Clone, Debug)]
struct OrdBomb {
  byte: u8,
  poison: bool,
}

impl PartialEq for OrdBomb {
  fn eq(&self, other: &Self) -> bool {
    self.byte == other.byte
  }
}

impl Eq for OrdBomb {}

impl PartialOrd for OrdBomb {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

impl Ord for OrdBomb {
  fn cmp(&self, other: &Self) -> Ordering {
    if self.poison || other.poison {
      panic!("armed Ord poison fired");
    }
    self.byte.cmp(&other.byte)
  }
}

fn ord_key(bytes: &[u8]) -> Vec<OrdBomb> {
  bytes
    .iter()
    .map(|&byte| OrdBomb {
      byte,
      poison: false,
    })
    .collect()
}

fn ord_key_poison_last(bytes: &[u8]) -> Vec<OrdBomb> {
  let last = bytes.len() - 1;
  bytes
    .iter()
    .enumerate()
    .map(|(i, &byte)| OrdBomb {
      byte,
      poison: i == last,
    })
    .collect()
}

#[test]
fn remove_panic_in_make_mut_before_take_keeps_len() {
  // A shared trie forces `remove` to copy-on-write: `make_mut` clones each node on
  // the path before the value is taken. A panic in that clone must leave `len` and
  // the trie exactly as they were — the take never ran.
  let mut t: Radix<KeyBomb, u32> = Radix::new();
  t.insert(&key(&[1, 2, 3]), 1);
  t.insert(&key(&[1, 2, 4]), 2);
  let snap = t.clone();
  assert_eq!(t.len(), 2);

  // Fire on the very first clone — inside `make_mut`, before any value is taken.
  // Arm past the 3 key-materialization clones so the fuse fires inside make_mut
  // (the copy-on-write of the shared path), before the value is taken.
  KEY_FUSE.with(|c| c.set(Some(3)));
  let r = catch_unwind(AssertUnwindSafe(|| t.remove(key(&[1, 2, 3]).as_slice())));
  KEY_FUSE.with(|c| c.set(None));
  assert!(r.is_err(), "the make_mut clone must have panicked");

  assert_eq!(t.len(), 2);
  assert_eq!(t.count_values(), 2);
  assert_eq!(t.get(key(&[1, 2, 3]).as_slice()), Some(&1));
  assert_eq!(t.get(key(&[1, 2, 4]).as_slice()), Some(&2));
  drop(snap);
}

#[test]
fn remove_descendants_panic_in_make_mut_before_unlink_keeps_len() {
  let mut t: Radix<KeyBomb, u32> = Radix::new();
  t.insert(&key(&[1]), 1);
  t.insert(&key(&[1, 2]), 2);
  t.insert(&key(&[1, 2, 3]), 3);
  let snap = t.clone();
  assert_eq!(t.len(), 3);

  // Arm past the 1 key-materialization clone so the fuse fires inside make_mut
  // (the copy-on-write of the shared path), before anything is unlinked.
  KEY_FUSE.with(|c| c.set(Some(1)));
  let r = catch_unwind(AssertUnwindSafe(|| {
    t.remove_descendants(key(&[1]).as_slice())
  }));
  KEY_FUSE.with(|c| c.set(None));
  assert!(r.is_err(), "the make_mut clone must have panicked");

  // Nothing was unlinked: `len` and every key are unchanged.
  assert_eq!(t.len(), 3);
  assert_eq!(t.count_values(), 3);
  assert_eq!(t.get(key(&[1]).as_slice()), Some(&1));
  assert_eq!(t.get(key(&[1, 2]).as_slice()), Some(&2));
  assert_eq!(t.get(key(&[1, 2, 3]).as_slice()), Some(&3));
  drop(snap);
}

#[test]
fn drain_descendants_panic_in_make_mut_before_unlink_keeps_len() {
  let mut t: Radix<KeyBomb, u32> = Radix::new();
  t.insert(&key(&[1]), 1);
  t.insert(&key(&[1, 2]), 2);
  t.insert(&key(&[1, 2, 3]), 3);
  let snap = t.clone();
  assert_eq!(t.len(), 3);

  // The values clone first (Phase 1, fine for `u32`); the panic is in Phase 2's
  // `make_mut`, before anything is unlinked.
  // Arm past the 1 key-materialization clone so the fuse fires inside make_mut
  // (the copy-on-write of the shared path), before anything is unlinked.
  KEY_FUSE.with(|c| c.set(Some(1)));
  let r = catch_unwind(AssertUnwindSafe(|| {
    t.drain_descendants(key(&[1]).as_slice())
  }));
  KEY_FUSE.with(|c| c.set(None));
  assert!(r.is_err(), "the make_mut clone must have panicked");

  assert_eq!(t.len(), 3);
  assert_eq!(t.count_values(), 3);
  assert_eq!(t.get(key(&[1, 2]).as_slice()), Some(&2));
  assert_eq!(t.get(key(&[1, 2, 3]).as_slice()), Some(&3));
  drop(snap);
}

#[test]
fn split_panic_in_ord_keeps_subtree_and_len() {
  // Edge "ab" from the root. Inserting "ac" splits at "a" and orders the new leaf
  // ("c") against the old child ("b"); poisoning "c" makes that ordering panic.
  // The original "ab" subtree and `len` must survive — the split detaches nothing
  // until the ordering has succeeded.
  let mut t: Radix<OrdBomb, u32> = Radix::new();
  t.insert(&ord_key(b"ab"), 1);
  let snap = t.clone();
  assert_eq!(t.len(), 1);

  let r = catch_unwind(AssertUnwindSafe(|| {
    t.insert(&ord_key_poison_last(b"ac"), 2)
  }));
  assert!(r.is_err(), "the split ordering Ord must have panicked");

  assert_eq!(t.get(ord_key(b"ab").as_slice()), Some(&1));
  assert_eq!(t.get(ord_key(b"ac").as_slice()), None);
  assert_eq!(t.len(), 1);
  assert_eq!(t.count_values(), 1);
  drop(snap);
}

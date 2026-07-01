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
  #[test]
  fn model_matches_reference(ops in prop::collection::vec(op_strategy(), 0..if cfg!(miri) { 16 } else { 60 })) {
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
// Every mutation builds its new pieces (cloning `C` labels / `V` values)
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
fn split_panic_in_label_clone_keeps_trie_consistent() {
  // Edge "[1,2,3]" from the root. Inserting "[1,2,4]" splits it at "[1,2]",
  // which clones the head/tail labels. A panic during that clone must not have
  // detached the original edge.
  let mut t: Radix<KeyBomb, u32> = Radix::new();
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
fn merge_moves_labels_and_returns_value() {
  // "[1,2]" + "[1,2,3]": removing "[1,2]" leaves a valueless single-child node
  // that merges its label with the grandchild's. The merge MOVES both labels (it
  // owns them — the child was un-shared on the way down), so it performs no
  // `C` clone and cannot panic: the removed value is always returned and
  // `len` stays accurate. Arming the label fuse on the very first clone proves the
  // merge clones nothing.
  let mut t: Radix<KeyBomb, u32> = Radix::new();
  t.insert(&key(&[1, 2]), 10);
  t.insert(&key(&[1, 2, 3]), 20);
  assert_eq!(t.len(), 2);

  // `remove` is iterator-native: it materializes no key (both the existence pass
  // and the unlink pass walk the key by reference). Arm on the very first clone —
  // which would be the merge's — to prove the merge clones nothing.
  KEY_FUSE.with(|c| c.set(Some(0)));
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
  // whose clone always panics is fine), and a `C` clone panic during
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
fn delete_prefix_on_shared_trie_never_clones_removed_values() {
  // `delete_prefix` is count-only and node-inclusive. Even on a SHARED trie — where
  // mutation copies the path to the prefix — it must not clone the VALUES being
  // removed (it unlinks the doomed edge rather than copying the subtree). So a value
  // whose clone always panics, stored at and below the deleted prefix, is fine. The
  // prefix is a direct child of the (valueless) root, so no ancestor value is on the
  // copied path either, making the expected value-clone count exactly zero.
  let mut t: Radix<KeyBomb, ValBomb> = Radix::new();
  t.insert(&key(&[1]), ValBomb(1));
  t.insert(&key(&[1, 2]), ValBomb(2));
  t.insert(&key(&[1, 3]), ValBomb(3));
  let snap = t.clone(); // share the whole subtree
  assert_eq!(t.len(), 3);

  VAL_FUSE.with(|c| c.set(Some(0))); // panic on ANY value clone
  let removed = catch_unwind(AssertUnwindSafe(|| t.delete_prefix(key(&[1]).as_slice())));
  VAL_FUSE.with(|c| c.set(None));

  assert_eq!(
    removed.ok(),
    Some(3),
    "delete_prefix must not clone any removed value, even on a shared trie"
  );
  assert_eq!(t.len(), 0);
  assert_eq!(t.count_values(), 0);
  // The snapshot is untouched (structural sharing preserved).
  assert_eq!(snap.len(), 3);
  assert_eq!(snap.get(key(&[1, 2]).as_slice()), Some(&ValBomb(2)));
}

#[test]
fn drain_prefix_on_shared_trie_returns_values_without_corrupting_snapshot() {
  // `drain_prefix` clones values out in phase 1 (its contract), then phase 2 unlinks
  // the doomed edge WITHOUT re-cloning (no `make_mut` on the subtree). On a shared
  // trie the snapshot must be left intact and the returned values correct.
  let mut t: Radix<u8, u32> = Radix::new();
  t.insert(&bytes(b"a"), 1);
  t.insert(&bytes(b"ab"), 2);
  t.insert(&bytes(b"ac"), 3);
  let snap = t.clone();

  let mut drained = t.drain_prefix(b"a".as_slice());
  drained.sort_unstable();
  assert_eq!(drained, vec![1, 2, 3], "node-inclusive: a, ab, ac");
  assert_eq!(t.len(), 0);
  // Snapshot intact — phase 2 unlinked from the live copy only.
  assert_eq!(snap.len(), 3);
  assert_eq!(snap.get(b"a".as_slice()), Some(&1));
  assert_eq!(snap.get(b"ac".as_slice()), Some(&3));
}

#[test]
fn delete_prefix_may_clone_a_retained_ancestor_value() {
  // The copy-on-write boundary: delete_prefix clones no REMOVED value, but on a
  // shared trie it still duplicates the RETAINED ancestors on the path to the
  // prefix — including their values, exactly like every mutator. This documents
  // that the no-removed-value guarantee does NOT extend to retained ancestors.
  let mut t: Radix<KeyBomb, ValBomb> = Radix::new();
  t.insert(&key(&[1]), ValBomb(1)); // retained ancestor — has a value
  t.insert(&key(&[1, 2]), ValBomb(2)); // the deleted prefix
  let snap = t.clone(); // share the path
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
  // Strong exception: the panic before any unlink leaves the trie and `len` intact.
  assert_eq!(t.len(), 2);
  assert_eq!(t.count_values(), 2);
  assert_eq!(t.get(key(&[1]).as_slice()), Some(&ValBomb(1)));
  assert_eq!(t.get(key(&[1, 2]).as_slice()), Some(&ValBomb(2)));
  assert_eq!(snap.len(), 2);
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

  // `remove_descendants` is iterator-native: it materializes no key. Arm on the
  // very first clone — which would be the merge's — to prove the merge clones
  // nothing.
  KEY_FUSE.with(|c| c.set(Some(0)));
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

  // `drain_descendants` is iterator-native: it materializes no key (phase 1 clones
  // only the `u32` values). Arm on the very first clone — which would be the
  // merge's — to prove the post-unlink merge clones nothing.
  KEY_FUSE.with(|c| c.set(Some(0)));
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

  // `remove` materializes no key, and the existence pass only reads, so the FIRST
  // `KeyBomb` clone is the copy-on-write `make_mut` of the shared path (cloning the
  // root edge's label). Arm on it — it fires before any value is taken.
  KEY_FUSE.with(|c| c.set(Some(0)));
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

  // `remove_descendants` materializes no key, and the existence pass only reads, so
  // the FIRST `KeyBomb` clone is the copy-on-write `make_mut` of the shared path.
  // Arm on it — it fires before anything is unlinked.
  KEY_FUSE.with(|c| c.set(Some(0)));
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

  // Phase 1 clones only the `u32` values (no key is materialized); the panic is in
  // Phase 2's `make_mut` (the copy-on-write of the shared path), before anything is
  // unlinked. The FIRST `KeyBomb` clone is that make_mut, so arm on it.
  KEY_FUSE.with(|c| c.set(Some(0)));
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

// ----- go-immutable-radix parity: min/max ---------------------------------

#[test]
fn min_max_on_empty() {
  let t: Trie = Radix::new();
  assert_eq!(t.minimum(), None);
  assert_eq!(t.maximum(), None);
}

#[test]
fn min_max_populated() {
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a"), 1);
  t.insert(&bytes(b"abc"), 2);
  t.insert(&bytes(b"abd"), 3);
  t.insert(&bytes(b"b"), 4);
  // "a" < "abc" < "abd" < "b" lexicographically.
  assert_eq!(t.minimum(), Some((bytes(b"a"), &1)));
  assert_eq!(t.maximum(), Some((bytes(b"b"), &4)));
}

#[test]
fn min_max_with_empty_key_and_single() {
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b""), 9);
  t.insert(&bytes(b"z"), 5);
  // the empty key sorts before every other key
  assert_eq!(t.minimum(), Some((bytes(b""), &9)));
  assert_eq!(t.maximum(), Some((bytes(b"z"), &5)));

  let mut one: Trie = Radix::new();
  one.insert(&bytes(b"solo"), 7);
  assert_eq!(one.minimum(), Some((bytes(b"solo"), &7)));
  assert_eq!(one.maximum(), Some((bytes(b"solo"), &7)));
}

// ----- delete_prefix / drain_prefix (node-inclusive) ----------------------

#[test]
fn delete_prefix_is_node_inclusive() {
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a"), 1);
  t.insert(&bytes(b"ab"), 2);
  t.insert(&bytes(b"ac"), 3);
  t.insert(&bytes(b"b"), 4);
  // removes "a" AND its strict descendants ("ab", "ac")
  assert_eq!(t.delete_prefix(b"a".as_slice()), 3);
  assert_eq!(t.get(b"a".as_slice()), None);
  assert_eq!(t.get(b"ab".as_slice()), None);
  assert_eq!(t.get(b"ac".as_slice()), None);
  assert_eq!(t.get(b"b".as_slice()), Some(&4));
  assert_eq!(t.len(), 1);
  assert!(t.is_canonical());
}

#[test]
fn delete_prefix_vs_remove_descendants() {
  // remove_descendants keeps the value at the key; delete_prefix removes it too.
  let mut keep: Trie = Radix::new();
  let mut nuke: Trie = Radix::new();
  for t in [&mut keep, &mut nuke] {
    t.insert(&bytes(b"x"), 1);
    t.insert(&bytes(b"x/y"), 2);
    t.insert(&bytes(b"x/y/z"), 3);
  }
  assert_eq!(keep.remove_descendants(b"x".as_slice()), 2); // x/y, x/y/z
  assert_eq!(keep.get(b"x".as_slice()), Some(&1)); // kept
  assert_eq!(keep.len(), 1);

  assert_eq!(nuke.delete_prefix(b"x".as_slice()), 3); // x, x/y, x/y/z
  assert_eq!(nuke.get(b"x".as_slice()), None); // removed
  assert!(nuke.is_empty());
}

#[test]
fn delete_prefix_mid_edge_absent_and_empty() {
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"abc"), 1);
  t.insert(&bytes(b"abd"), 2);
  // "ab" ends mid-edge (no node), yet is a prefix of both → removes both
  assert_eq!(t.delete_prefix(b"ab".as_slice()), 2);
  assert!(t.is_empty());

  // absent prefix is a no-op
  t.insert(&bytes(b"z"), 5);
  assert_eq!(t.delete_prefix(b"q".as_slice()), 0);
  assert_eq!(t.len(), 1);

  // the empty key prefix clears the whole trie
  t.insert(&bytes(b"a"), 1);
  t.insert(&bytes(b""), 0);
  assert_eq!(t.delete_prefix(b"".as_slice()), 3); // z, a, ""
  assert!(t.is_empty());
}

#[test]
fn delete_prefix_noop_preserves_sharing() {
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a/x"), 1);
  t.insert(&bytes(b"b/y"), 2);
  let snap = t.clone();
  assert_eq!(t.delete_prefix(b"zzz".as_slice()), 0); // absent
  for first in *b"ab" {
    let s = snap.edge_child(&first).expect("edge");
    let l = t.edge_child(&first).expect("edge");
    assert!(
      SharedPointer::ptr_eq(&s, &l),
      "a no-op delete_prefix must not break sharing"
    );
  }
}

#[test]
fn drain_prefix_returns_values_ascending() {
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a"), 1);
  t.insert(&bytes(b"ab"), 2);
  t.insert(&bytes(b"ac"), 3);
  t.insert(&bytes(b"b"), 4);
  // value at "a" first, then strict descendants in ascending key order
  assert_eq!(t.drain_prefix(b"a".as_slice()), vec![1, 2, 3]);
  assert_eq!(t.get(b"b".as_slice()), Some(&4));
  assert_eq!(t.len(), 1);
}

#[test]
fn drain_prefix_shared_subtree_clones_not_moves() {
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a"), 1);
  t.insert(&bytes(b"a/b"), 2);
  t.insert(&bytes(b"a/b/c"), 3);
  let snap = t.clone();

  assert_eq!(t.drain_prefix(b"a".as_slice()), vec![1, 2, 3]);
  assert!(t.is_empty());
  // snapshot keeps everything (values were cloned, not stolen)
  assert_eq!(snap.get(b"a".as_slice()), Some(&1));
  assert_eq!(snap.get(b"a/b".as_slice()), Some(&2));
  assert_eq!(snap.get(b"a/b/c".as_slice()), Some(&3));
  assert_eq!(snap.len(), 3);
}

// ----- reverse iteration --------------------------------------------------

#[test]
fn values_rev_is_reverse_of_values() {
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a"), 1);
  t.insert(&bytes(b"ab"), 2);
  t.insert(&bytes(b"abc"), 3);
  t.insert(&bytes(b"b"), 4);
  let fwd: Vec<u32> = t.values().copied().collect();
  assert_eq!(fwd, vec![1, 2, 3, 4]); // a, ab, abc, b
  let rev: Vec<u32> = t.values_rev().copied().collect();
  assert_eq!(rev, vec![4, 3, 2, 1]);
}

#[test]
fn descendants_rev_is_reverse_strict() {
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a"), 0); // excluded (strict)
  t.insert(&bytes(b"a/x"), 1);
  t.insert(&bytes(b"a/y"), 2);
  t.insert(&bytes(b"a/z"), 3);
  let fwd: Vec<u32> = t.descendants(b"a".as_slice()).copied().collect();
  assert_eq!(fwd, vec![1, 2, 3]);
  let rev: Vec<u32> = t.descendants_rev(b"a".as_slice()).copied().collect();
  assert_eq!(rev, vec![3, 2, 1]);
}

#[test]
fn rev_iters_empty() {
  let t: Trie = Radix::new();
  assert_eq!(t.values_rev().count(), 0);
  assert_eq!(t.descendants_rev(b"x".as_slice()).count(), 0);
}

// ----- range / seek_lower_bound -------------------------------------------

use core::ops::Bound;

fn rng(t: &Trie, lo: Bound<&[u8]>, hi: Bound<&[u8]>) -> Vec<(Vec<u8>, u32)> {
  t.range::<[u8], _>((lo, hi)).map(|(k, v)| (k, *v)).collect()
}

#[test]
fn range_all_bound_combinations() {
  use core::ops::Bound::{Excluded, Included, Unbounded};
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a"), 1);
  t.insert(&bytes(b"b"), 2);
  t.insert(&bytes(b"c"), 3);
  t.insert(&bytes(b"d"), 4);
  let all = vec![
    (bytes(b"a"), 1),
    (bytes(b"b"), 2),
    (bytes(b"c"), 3),
    (bytes(b"d"), 4),
  ];

  // Unbounded × Unbounded → everything.
  assert_eq!(rng(&t, Unbounded, Unbounded), all);
  // start bounds, open end.
  assert_eq!(
    rng(&t, Included(b"b"), Unbounded),
    vec![(bytes(b"b"), 2), (bytes(b"c"), 3), (bytes(b"d"), 4)]
  );
  assert_eq!(
    rng(&t, Excluded(b"b"), Unbounded),
    vec![(bytes(b"c"), 3), (bytes(b"d"), 4)]
  );
  // open start, end bounds.
  assert_eq!(
    rng(&t, Unbounded, Included(b"c")),
    vec![(bytes(b"a"), 1), (bytes(b"b"), 2), (bytes(b"c"), 3)]
  );
  assert_eq!(
    rng(&t, Unbounded, Excluded(b"c")),
    vec![(bytes(b"a"), 1), (bytes(b"b"), 2)]
  );
  // both ends, every kind combination.
  assert_eq!(
    rng(&t, Included(b"b"), Included(b"c")),
    vec![(bytes(b"b"), 2), (bytes(b"c"), 3)]
  );
  assert_eq!(
    rng(&t, Included(b"b"), Excluded(b"c")),
    vec![(bytes(b"b"), 2)]
  );
  assert_eq!(
    rng(&t, Excluded(b"b"), Included(b"c")),
    vec![(bytes(b"c"), 3)]
  );
  // nothing strictly between adjacent keys.
  assert_eq!(rng(&t, Excluded(b"b"), Excluded(b"c")), vec![]);
  // single element.
  assert_eq!(
    rng(&t, Included(b"b"), Included(b"b")),
    vec![(bytes(b"b"), 2)]
  );
  // empty (degenerate) ranges.
  assert_eq!(rng(&t, Included(b"c"), Excluded(b"c")), vec![]);
  assert_eq!(rng(&t, Excluded(b"c"), Included(b"c")), vec![]);
  // inverted bounds yield nothing rather than panicking.
  assert_eq!(rng(&t, Included(b"d"), Included(b"a")), vec![]);
  // spanning across the interior.
  assert_eq!(
    rng(&t, Excluded(b"a"), Excluded(b"d")),
    vec![(bytes(b"b"), 2), (bytes(b"c"), 3)]
  );
}

#[test]
fn range_prefix_spanning() {
  use core::ops::Bound::{Excluded, Included, Unbounded};
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a"), 1);
  t.insert(&bytes(b"ab"), 2);
  t.insert(&bytes(b"abc"), 3);
  t.insert(&bytes(b"b"), 4);
  // a proper prefix as a lower bound includes the whole subtree below it
  assert_eq!(
    rng(&t, Included(b"a"), Excluded(b"b")),
    vec![(bytes(b"a"), 1), (bytes(b"ab"), 2), (bytes(b"abc"), 3)]
  );
  // excluding the prefix key keeps its descendants
  assert_eq!(
    rng(&t, Excluded(b"a"), Unbounded),
    vec![(bytes(b"ab"), 2), (bytes(b"abc"), 3), (bytes(b"b"), 4)]
  );
  // a bound that diverges mid-edge below the edge includes the subtree
  assert_eq!(
    rng(&t, Included(b"aa"), Unbounded),
    vec![(bytes(b"ab"), 2), (bytes(b"abc"), 3), (bytes(b"b"), 4)]
  );
  // a bound that diverges mid-edge above the edge excludes the subtree
  assert_eq!(rng(&t, Included(b"ac"), Unbounded), vec![(bytes(b"b"), 4)]);
}

#[test]
fn seek_lower_bound_positions() {
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a"), 1);
  t.insert(&bytes(b"c"), 3);
  t.insert(&bytes(b"e"), 5);
  let seek = |k: &[u8]| -> Vec<u32> { t.seek_lower_bound(k).map(|(_, v)| *v).collect() };
  // lands exactly on a stored key
  assert_eq!(seek(b"c"), vec![3, 5]);
  // lands between two keys
  assert_eq!(seek(b"b"), vec![3, 5]);
  // before all keys
  assert_eq!(seek(b""), vec![1, 3, 5]);
  assert_eq!(seek(b"a"), vec![1, 3, 5]);
  // after all keys
  assert_eq!(seek(b"f"), vec![]);
  assert_eq!(seek(b"z"), vec![]);
  // on the last key
  assert_eq!(seek(b"e"), vec![5]);
}

#[test]
fn reverse_seek_lower_bound_positions() {
  // The descending mirror of `seek_lower_bound_positions` (go's
  // `SeekReverseLowerBound`): keys `<= search`, largest first.
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a"), 1);
  t.insert(&bytes(b"c"), 3);
  t.insert(&bytes(b"e"), 5);
  let rseek = |k: &[u8]| -> Vec<u32> { t.seek_reverse_lower_bound(k).map(|(_, v)| *v).collect() };
  assert_eq!(rseek(b"c"), vec![3, 1]); // exact stored key
  assert_eq!(rseek(b"d"), vec![3, 1]); // between two keys
  assert_eq!(rseek(b"e"), vec![5, 3, 1]); // last key
  assert_eq!(rseek(b"z"), vec![5, 3, 1]); // after all keys
  assert_eq!(rseek(b"a"), vec![1]); // first key
  assert_eq!(rseek(b""), vec![]); // the empty key is the smallest — nothing is below it
  assert_eq!(rseek(b"`"), vec![]); // below every key

  // Mixed-length keys exercise the mid-edge / shared-prefix seeding.
  let mut m: Trie = Radix::new();
  for (i, k) in [
    b"a1".as_slice(),
    b"abc",
    b"barbazboo",
    b"f",
    b"foo",
    b"found",
    b"zap",
    b"zip",
  ]
  .into_iter()
  .enumerate()
  {
    m.insert(k, i as u32);
  }
  let keys = |k: &[u8]| -> Vec<Vec<u8>> { m.seek_reverse_lower_bound(k).map(|(k, _)| k).collect() };
  // "f" is a prefix of "foo"/"found": keys <= "foo" excludes "found"/"zap"/"zip".
  assert_eq!(
    keys(b"foo"),
    vec![
      b"foo".to_vec(),
      b"f".to_vec(),
      b"barbazboo".to_vec(),
      b"abc".to_vec(),
      b"a1".to_vec()
    ]
  );
  // "fom" < "foo": both "foo" and "found" drop out; "f" (a proper prefix) stays.
  assert_eq!(
    keys(b"fom"),
    vec![
      b"f".to_vec(),
      b"barbazboo".to_vec(),
      b"abc".to_vec(),
      b"a1".to_vec()
    ]
  );
  // above everything: the full set, descending.
  assert_eq!(
    keys(b"zzz"),
    vec![
      b"zip".to_vec(),
      b"zap".to_vec(),
      b"found".to_vec(),
      b"foo".to_vec(),
      b"f".to_vec(),
      b"barbazboo".to_vec(),
      b"abc".to_vec(),
      b"a1".to_vec()
    ]
  );
}

#[test]
fn range_rev_descending_with_bounds() {
  let mut t: Trie = Radix::new();
  for (k, v) in [(b"a".as_slice(), 1u32), (b"b", 2), (b"c", 3), (b"d", 4)] {
    t.insert(k, v);
  }
  let rr = |r: (Bound<&[u8]>, Bound<&[u8]>)| -> Vec<(Vec<u8>, u32)> {
    t.range_rev::<[u8], _>(r).map(|(k, v)| (k, *v)).collect()
  };
  assert_eq!(
    rr((Bound::Unbounded, Bound::Unbounded)),
    vec![
      (b"d".to_vec(), 4),
      (b"c".to_vec(), 3),
      (b"b".to_vec(), 2),
      (b"a".to_vec(), 1)
    ]
  );
  assert_eq!(
    rr((
      Bound::Included(b"b".as_slice()),
      Bound::Included(b"c".as_slice())
    )),
    vec![(b"c".to_vec(), 3), (b"b".to_vec(), 2)]
  );
  assert_eq!(
    rr((
      Bound::Excluded(b"a".as_slice()),
      Bound::Excluded(b"d".as_slice())
    )),
    vec![(b"c".to_vec(), 3), (b"b".to_vec(), 2)]
  );
}

#[test]
fn descendants_inclusive_includes_prefix_key() {
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a"), 1);
  t.insert(&bytes(b"ab"), 2);
  t.insert(&bytes(b"abc"), 3);
  t.insert(&bytes(b"b"), 9);
  // strict excludes the value at the prefix; inclusive includes it.
  assert_eq!(
    t.descendants(b"a".as_slice()).copied().collect::<Vec<_>>(),
    vec![2, 3]
  );
  assert_eq!(
    t.descendants_inclusive(b"a".as_slice())
      .copied()
      .collect::<Vec<_>>(),
    vec![1, 2, 3]
  );
  // a prefix that is not itself a key: inclusive == strict (the two agree).
  let mut t2: Trie = Radix::new();
  t2.insert(&bytes(b"xy"), 1);
  t2.insert(&bytes(b"xz"), 2);
  assert_eq!(
    t2.descendants_inclusive(b"x".as_slice())
      .copied()
      .collect::<Vec<_>>(),
    vec![1, 2]
  );
  // absent prefix: empty.
  assert_eq!(
    t.descendants_inclusive(b"zzz".as_slice())
      .copied()
      .collect::<Vec<u32>>(),
    Vec::<u32>::new()
  );
}

#[test]
fn clone_fork_is_bidirectionally_isolated() {
  // go-immutable-radix's `TestClone`: two independent forks; edits to each are
  // invisible to the other and to the original (both directions).
  let mut base: Trie = Radix::new();
  base.insert(&bytes(b"shared"), 0);
  let mut a = base.clone();
  let mut b = base.clone();
  a.insert(&bytes(b"a-only"), 1);
  b.insert(&bytes(b"b-only"), 2);
  assert_eq!(a.get(b"a-only".as_slice()), Some(&1));
  assert_eq!(a.get(b"b-only".as_slice()), None);
  assert_eq!(b.get(b"b-only".as_slice()), Some(&2));
  assert_eq!(b.get(b"a-only".as_slice()), None);
  assert_eq!(base.len(), 1); // the original is untouched by either fork
  assert!(base.get(b"a-only".as_slice()).is_none());
  assert!(base.get(b"b-only".as_slice()).is_none());
}

#[test]
fn walk_prefix_and_path_yield_keys() {
  let mut t: Trie = Radix::new();
  t.insert(&bytes(b"a"), 1);
  t.insert(&bytes(b"a/b"), 2);
  t.insert(&bytes(b"a/b/c"), 3);
  t.insert(&bytes(b"b"), 9);
  let kv = |it: Range<'_, u8, u32>| -> Vec<(Vec<u8>, u32)> { it.map(|(k, v)| (k, *v)).collect() };
  // walk_prefix is node-inclusive (includes the "a" key); ascending (key, value).
  assert_eq!(
    kv(t.walk_prefix(b"a".as_slice())),
    vec![(bytes(b"a"), 1), (bytes(b"a/b"), 2), (bytes(b"a/b/c"), 3)]
  );
  // strict excludes the "a" key itself.
  assert_eq!(
    kv(t.walk_prefix_strict(b"a".as_slice())),
    vec![(bytes(b"a/b"), 2), (bytes(b"a/b/c"), 3)]
  );
  // descending form reverses the order.
  assert_eq!(
    t.walk_prefix_rev(b"a".as_slice())
      .map(|(k, v)| (k, *v))
      .collect::<Vec<_>>(),
    vec![(bytes(b"a/b/c"), 3), (bytes(b"a/b"), 2), (bytes(b"a"), 1)]
  );
  // walk_path: the stored keys that are prefixes of "a/b/c", root-to-key.
  assert_eq!(
    t.walk_path(b"a/b/c".as_slice())
      .map(|(k, v)| (k, *v))
      .collect::<Vec<_>>(),
    vec![(bytes(b"a"), 1), (bytes(b"a/b"), 2), (bytes(b"a/b/c"), 3)]
  );
  assert_eq!(
    t.walk_path_rev(b"a/b/c".as_slice())
      .map(|(k, v)| (k, *v))
      .collect::<Vec<_>>(),
    vec![(bytes(b"a/b/c"), 3), (bytes(b"a/b"), 2), (bytes(b"a"), 1)]
  );
}

// ----- proptest: ordered ops vs a BTreeMap oracle -------------------------

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
    let mut trie: Trie = Radix::new();
    let mut model: BTreeMap<Vec<u8>, u32> = BTreeMap::new();
    for (k, v) in entries {
      trie.insert(&k.clone(), v);
      model.insert(k, v);
    }

    // minimum / maximum.
    prop_assert_eq!(
      trie.minimum().map(|(k, v)| (k, *v)),
      model.iter().next().map(|(k, v)| (k.clone(), *v))
    );
    prop_assert_eq!(
      trie.maximum().map(|(k, v)| (k, *v)),
      model.iter().next_back().map(|(k, v)| (k.clone(), *v))
    );

    // forward / reverse value order.
    let fwd: Vec<u32> = trie.values().copied().collect();
    let model_vals: Vec<u32> = model.values().copied().collect();
    prop_assert_eq!(&fwd, &model_vals);
    let rev: Vec<u32> = trie.values_rev().copied().collect();
    let mut want_rev = model_vals.clone();
    want_rev.reverse();
    prop_assert_eq!(rev, want_rev);

    // range over every bound-kind combination on two random pivots.
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

    // seek_lower_bound == range(key..).
    let seek: Vec<(Vec<u8>, u32)> = trie.seek_lower_bound(del.as_slice()).map(|(k, v)| (k, *v)).collect();
    prop_assert_eq!(seek, model_range(&model, Bound::Included(del.as_slice()), Bound::Unbounded));

    // strict descendants forward / reverse.
    let d_fwd: Vec<u32> = trie.descendants(del.as_slice()).copied().collect();
    let d_want: Vec<u32> = model
      .iter()
      .filter(|(k, _)| k.len() > del.len() && k.starts_with(&del))
      .map(|(_, v)| *v)
      .collect();
    prop_assert_eq!(&d_fwd, &d_want);
    let d_rev: Vec<u32> = trie.descendants_rev(del.as_slice()).copied().collect();
    let mut d_rev_want = d_want.clone();
    d_rev_want.reverse();
    prop_assert_eq!(d_rev, d_rev_want);

    // range_rev == reverse of forward range, over every bound-kind combination.
    for &lo in &los {
      for &hi in &his {
        let got_rev: Vec<(Vec<u8>, u32)> = trie
          .range_rev::<[u8], _>((lo, hi))
          .map(|(k, v)| (k, *v))
          .collect();
        let mut want_rev = model_range(&model, lo, hi);
        want_rev.reverse();
        prop_assert_eq!(got_rev, want_rev);
      }
    }

    // seek_reverse_lower_bound(k) == reverse of range(..=k): keys <= k, descending.
    let rseek: Vec<(Vec<u8>, u32)> = trie
      .seek_reverse_lower_bound(del.as_slice())
      .map(|(k, v)| (k, *v))
      .collect();
    let mut rseek_want = model_range(&model, Bound::Unbounded, Bound::Included(del.as_slice()));
    rseek_want.reverse();
    prop_assert_eq!(rseek, rseek_want);

    // node-inclusive descendants == strict descendants plus the value at the key,
    // in ascending key order.
    let inc: Vec<u32> = trie
      .descendants_inclusive(del.as_slice())
      .copied()
      .collect();
    let inc_want: Vec<u32> = model
      .iter()
      .filter(|(k, _)| k.starts_with(&del))
      .map(|(_, v)| *v)
      .collect();
    prop_assert_eq!(inc, inc_want);

    // ancestors(k) == the stored keys that are prefixes of k (inclusive), ascending.
    let anc: Vec<u32> = trie.ancestors(del.as_slice()).copied().collect();
    let anc_want: Vec<u32> = model
      .iter()
      .filter(|(k, _)| del.starts_with(k.as_slice()))
      .map(|(_, v)| *v)
      .collect();
    prop_assert_eq!(anc, anc_want);

    // walk_prefix (inclusive) == model keys with del as a prefix, (key, value) asc.
    let wp: Vec<(Vec<u8>, u32)> = trie.walk_prefix(del.as_slice()).map(|(k, v)| (k, *v)).collect();
    let wp_want: Vec<(Vec<u8>, u32)> = model
      .iter()
      .filter(|(k, _)| k.starts_with(&del))
      .map(|(k, v)| (k.clone(), *v))
      .collect();
    prop_assert_eq!(&wp, &wp_want);
    let wpr: Vec<(Vec<u8>, u32)> = trie
      .walk_prefix_rev(del.as_slice())
      .map(|(k, v)| (k, *v))
      .collect();
    let mut wp_rev_want = wp_want.clone();
    wp_rev_want.reverse();
    prop_assert_eq!(wpr, wp_rev_want);

    // walk_prefix_strict == model keys strictly extending del.
    let wps: Vec<(Vec<u8>, u32)> = trie
      .walk_prefix_strict(del.as_slice())
      .map(|(k, v)| (k, *v))
      .collect();
    let wps_want: Vec<(Vec<u8>, u32)> = model
      .iter()
      .filter(|(k, _)| k.len() > del.len() && k.starts_with(&del))
      .map(|(k, v)| (k.clone(), *v))
      .collect();
    prop_assert_eq!(&wps, &wps_want);
    let wpsr: Vec<(Vec<u8>, u32)> = trie
      .walk_prefix_strict_rev(del.as_slice())
      .map(|(k, v)| (k, *v))
      .collect();
    let mut wps_rev_want = wps_want.clone();
    wps_rev_want.reverse();
    prop_assert_eq!(wpsr, wps_rev_want);

    // walk_path == model keys that are prefixes of del (inclusive), root-to-key.
    let wpath: Vec<(Vec<u8>, u32)> = trie.walk_path(del.as_slice()).map(|(k, v)| (k, *v)).collect();
    let wpath_want: Vec<(Vec<u8>, u32)> = model
      .iter()
      .filter(|(k, _)| del.starts_with(k.as_slice()))
      .map(|(k, v)| (k.clone(), *v))
      .collect();
    prop_assert_eq!(&wpath, &wpath_want);
    let wpathr: Vec<(Vec<u8>, u32)> = trie
      .walk_path_rev(del.as_slice())
      .map(|(k, v)| (k, *v))
      .collect();
    let mut wpath_rev_want = wpath_want.clone();
    wpath_rev_want.reverse();
    prop_assert_eq!(wpathr, wpath_rev_want);

    // drain_prefix returns the at-or-below values in ascending key order, and is
    // the value-returning twin of delete_prefix.
    let drained = trie.drain_prefix(del.as_slice());
    let mut model_after = model.clone();
    let n = model_delete_prefix(&mut model_after, &del);
    let want_drained: Vec<u32> = model
      .iter()
      .filter(|(k, _)| k.starts_with(&del))
      .map(|(_, v)| *v)
      .collect();
    prop_assert_eq!(&drained, &want_drained);
    prop_assert_eq!(drained.len(), n);

    // delete_prefix removes the same set (count + resulting trie).
    let removed = trie.delete_prefix(del.as_slice());
    // drain already removed them, so a second delete is a no-op.
    prop_assert_eq!(removed, 0);
    model = model_after;
    prop_assert_eq!(trie.len(), model.len());
    prop_assert_eq!(trie.len(), trie.count_values());
    prop_assert!(trie.is_canonical());
    for (k, v) in &model {
      prop_assert_eq!(trie.get(k.as_slice()), Some(v));
    }
  }
}

// ----- panic-safety: delete_prefix / drain_prefix --------------------------

#[test]
fn delete_prefix_panic_in_make_mut_before_unlink_keeps_len() {
  // A shared trie forces copy-on-write: `make_mut` clones each node on the path
  // (and thus its `KeyBomb` labels) before anything is unlinked. A panic there
  // must leave `len` and every key exactly as they were.
  let mut t: Radix<KeyBomb, u32> = Radix::new();
  t.insert(&key(&[1]), 1);
  t.insert(&key(&[1, 2]), 2);
  t.insert(&key(&[1, 2, 3]), 3);
  let snap = t.clone();
  assert_eq!(t.len(), 3);

  // `delete_prefix` materializes no key, and the existence pass only reads, so the
  // FIRST `KeyBomb` clone is the copy-on-write `make_mut` of the shared path. Arm on
  // it — it fires before anything is unlinked.
  KEY_FUSE.with(|c| c.set(Some(0)));
  let r = catch_unwind(AssertUnwindSafe(|| t.delete_prefix(key(&[1]).as_slice())));
  KEY_FUSE.with(|c| c.set(None));
  assert!(r.is_err(), "the make_mut clone must have panicked");

  assert_eq!(t.len(), 3);
  assert_eq!(t.count_values(), 3);
  assert_eq!(t.get(key(&[1]).as_slice()), Some(&1));
  assert_eq!(t.get(key(&[1, 2]).as_slice()), Some(&2));
  assert_eq!(t.get(key(&[1, 2, 3]).as_slice()), Some(&3));
  drop(snap);
}

#[test]
fn drain_prefix_panic_in_make_mut_before_unlink_keeps_len() {
  let mut t: Radix<KeyBomb, u32> = Radix::new();
  t.insert(&key(&[1]), 1);
  t.insert(&key(&[1, 2]), 2);
  t.insert(&key(&[1, 2, 3]), 3);
  let snap = t.clone();
  assert_eq!(t.len(), 3);

  // Phase 1 clones only the `u32` values (no key is materialized); the panic is in
  // Phase 2's make_mut (the copy-on-write of the shared path), before anything is
  // unlinked. The FIRST `KeyBomb` clone is that make_mut, so arm on it.
  KEY_FUSE.with(|c| c.set(Some(0)));
  let r = catch_unwind(AssertUnwindSafe(|| t.drain_prefix(key(&[1]).as_slice())));
  KEY_FUSE.with(|c| c.set(None));
  assert!(r.is_err(), "the make_mut clone must have panicked");

  assert_eq!(t.len(), 3);
  assert_eq!(t.count_values(), 3);
  assert_eq!(t.get(key(&[1]).as_slice()), Some(&1));
  assert_eq!(t.get(key(&[1, 2, 3]).as_slice()), Some(&3));
  drop(snap);
}

#[test]
fn drain_prefix_panic_in_value_clone_keeps_trie_intact() {
  // drain_prefix clones the value at the key and every descendant value out FIRST
  // (Phase 1); a panic there must leave the trie and `len` completely untouched.
  let mut t: Radix<KeyBomb, ValBomb> = Radix::new();
  t.insert(&key(&[1]), ValBomb(1));
  t.insert(&key(&[1, 2]), ValBomb(2));
  t.insert(&key(&[1, 2, 3]), ValBomb(3));
  assert_eq!(t.len(), 3);

  // Phase 1 clones value-at-key (1) then descendants (2, 3); fire on the 2nd.
  VAL_FUSE.with(|c| c.set(Some(1)));
  let r = catch_unwind(AssertUnwindSafe(|| t.drain_prefix(key(&[1]).as_slice())));
  VAL_FUSE.with(|c| c.set(None));
  assert!(r.is_err(), "the armed value clone must have panicked");

  assert_eq!(t.get(key(&[1]).as_slice()), Some(&ValBomb(1)));
  assert_eq!(t.get(key(&[1, 2]).as_slice()), Some(&ValBomb(2)));
  assert_eq!(t.get(key(&[1, 2, 3]).as_slice()), Some(&ValBomb(3)));
  assert_eq!(t.len(), 3);
  assert_eq!(t.count_values(), 3);
}

use std::collections::HashMap;

use bytes::Bytes;

use super::{node::copy_node, *};

fn copy_tree<T>(t: &Tree<T>) -> Tree<T> {
  Tree {
    root: copy_node(&t.root),
    size: t.size,
    kind: t.kind,
  }
}

fn test_radix_huge_txn_runner(mut r: Tree<usize>) {
  // Insert way more nodes than the cache can fit
  let mut txn = r.txn();
  let mut expect = Vec::new();
  for i in 0..DEFAULT_MODIFIED_CACHE * 100 {
    let gen = uuid::Uuid::new_v4();
    txn.insert(Bytes::copy_from_slice(gen.as_bytes()), i);
    expect.push(*gen.as_bytes());
  }
  let r = txn.commit();
  expect.sort();

  // Collect the output, should be sorted
  let mut out = Vec::new();
  r.root().walk(|k, _| {
    out.push((k.to_vec()));
    false
  });

  // Verify the match
  assert_eq!(
    out.len(),
    expect.len(),
    "kind({:?}): length mis-match",
    r.kind()
  );
  for i in 0..out.len() {
    assert_eq!(
      out[i],
      expect[i],
      "kind({:?}): mis-match at index {}",
      r.kind(),
      i
    );
  }
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_radix_huge_txn() {
  test_radix_huge_txn_runner(Tree::vec());
  test_radix_huge_txn_runner(Tree::btree());
}

fn test_radix_runner(mut t: Tree<()>) {
  let (mut min, mut max) = (None, None);
  let mut inp = HashMap::with_capacity(1000);
  for i in 0..1000 {
    let gen = uuid::Uuid::new_v4();
    inp.insert(gen.as_bytes().to_vec(), i);

    if let Some(val) = min {
      if gen < val {
        min = Some(gen);
      }
    } else {
      min = Some(gen);
    }

    if let Some(val) = max {
      if gen > val {
        max = Some(gen);
      }
    } else {
      max = Some(gen);
    }
  }
  let min = min.as_ref().unwrap().as_bytes().as_slice();
  let max = max.as_ref().unwrap().as_bytes().as_slice();

  let mut r = Tree::<usize>::new_with_kind(t.kind);
  let mut rcopy = copy_tree(&r);
  for (k, v) in inp.iter() {
    let (nr, _) = r.insert(Bytes::copy_from_slice(k), *v);

    r = nr;
    rcopy = copy_tree(&r);
  }

  assert_eq!(r.len(), inp.len(), "kind({:?}): bad length", r.kind());

  for (k, v) in inp.iter() {
    match r.get(k) {
      None => panic!("kind({:?}): missing key {:?}", r.kind(), k),
      Some(out) => assert_eq!(
        out,
        v,
        "kind({:?}): value mis-match. exp: {} got: {}",
        r.kind(),
        *v,
        *out
      ),
    }
  }

  // Check min and max
  let (out_min, _) = r.root().minimum().unwrap();
  assert_eq!(
    out_min, min,
    "kind({:?}): bad minimum: {:?} {:?}",
    t.kind, out_min, min
  );
  let (out_max, _) = r.root().maximum().unwrap();
  assert_eq!(
    out_max, max,
    "kind({:?}): bad maximum: {:?} {:?}",
    t.kind, out_max, max
  );

  // Copy the full tree before delete
  let orig = copy_tree(&r);
  let orig_copy = copy_tree(&orig);

  for (k, v) in inp.iter() {
    let (nr, out) = r.remove(k);
    r = nr;
    assert_eq!(
      *(out.unwrap()),
      *v,
      "kind({:?}): bad remove value",
      r.kind()
    );
  }

  assert_eq!(r.len(), 0, "kind({:?}): bad length", r.kind());
}

#[test]
fn test_radix() {
  test_radix_runner(Tree::vec());
  // test_radix_runner(Tree::btree());
}

fn test_len_txn_runner(mut r: Tree<()>) {
  const KEYS: &[&str] = &[
    "foo/bar/baz",
    "foo/baz/bar",
    "foo/zip/zap",
    "foobar",
    "nochange",
  ];

  let mut txn = r.txn();

  for k in KEYS {
    txn.insert(Bytes::copy_from_slice(k.as_bytes()), ());
  }

  r = txn.commit();
  assert_eq!(r.len(), KEYS.len(), "{:?}", r.kind());

  let mut txn = r.txn();
  for k in KEYS {
    txn.remove(k.as_bytes());
  }
  r = txn.commit();
  assert!(r.is_empty(), "{:?}", r.kind());
}

#[test]
fn test_len_txn() {
  test_len_txn_runner(Tree::vec());
  test_len_txn_runner(Tree::btree());
}

fn test_longest_prefix_runner(mut r: Tree<()>) {
  const KEYS: &[&str] = &["", "foo", "foobar", "foobarbaz", "foobarbazzip", "foozip"];

  for k in KEYS {
    let (nr, _) = r.insert(Bytes::copy_from_slice(k.as_bytes()), ());
    r = nr;
  }

  assert_eq!(
    r.len(),
    KEYS.len(),
    "kind: {:?} bad len {} {}",
    r.kind(),
    r.len(),
    KEYS.len()
  );

  const CASES: &[(&str, &str)] = &[
    ("a", ""),
    ("abc", ""),
    ("fo", ""),
    ("foo", "foo"),
    ("foob", "foo"),
    ("foobar", "foobar"),
    ("foobarba", "foobar"),
    ("foobarbaz", "foobarbaz"),
    ("foobarbazzi", "foobarbaz"),
    ("foobarbazzip", "foobarbazzip"),
    ("foozi", "foo"),
    ("foozip", "foozip"),
    ("foozipzap", "foozip"),
  ];

  let root = r.root();
  for (inp, out) in CASES {
    match root.longest_prefix(inp.as_bytes()) {
      None => panic!("kind({:?}): no match for input {inp}", r.kind()),
      Some((m, _)) => {
        assert_eq!(
          m,
          out.as_bytes(),
          "kind({:?}): bad match for input {inp}: exp {out:?}, got {m:?}",
          r.kind()
        );
      }
    }
  }
}

#[test]
fn test_longest_prefix() {
  test_longest_prefix_runner(Tree::vec());
  test_longest_prefix_runner(Tree::btree());
}

fn test_minimum_runner(mut r: Tree<usize>) {
  const KEYS: &[&str] = &[
    "foo/bar/baz",
    "foo/baz/bar",
    "foo/zip/zap",
    "foobar",
    "nochange",
  ];

  for (idx, k) in KEYS.iter().enumerate() {
    let (nr, _) = r.insert(Bytes::copy_from_slice(k.as_bytes()), idx);
    r = nr;
  }

  let (min, v) = r.root().minimum().unwrap();
  assert_eq!(min, KEYS[0].as_bytes());
  assert_eq!(*v, 0);
}

#[test]
fn test_minimum() {
  test_minimum_runner(Tree::vec());
  test_minimum_runner(Tree::btree());
}

fn test_maximum_runner(mut r: Tree<usize>) {
  const KEYS: &[&str] = &[
    "foo/bar/baz",
    "foo/baz/bar",
    "foo/zip/zap",
    "foobar",
    "nochange",
  ];

  for (idx, k) in KEYS.iter().enumerate() {
    let (nr, _) = r.insert(Bytes::copy_from_slice(k.as_bytes()), idx);
    r = nr;
  }

  let (max, v) = r.root().maximum().unwrap();
  assert_eq!(max, KEYS[4].as_bytes());
  assert_eq!(*v, 4);
}

#[test]
fn test_maximum() {
  test_maximum_runner(Tree::vec());
  test_maximum_runner(Tree::btree());
}

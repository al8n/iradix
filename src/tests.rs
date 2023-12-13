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

impl<V> Tree<V> {
  fn kind(&self) -> Kind {
    self.kind
  }

  fn new_with_kind(kind: Kind) -> Self {
    match kind {
      Kind::Vec => Self::vec(),
      Kind::BTree => Self::btree(),
    }
  }
}

fn test_radix_huge_txn_runner(r: Tree<usize>) {
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
    out.push(k.to_vec());
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

fn test_radix_runner(t: Tree<()>) {
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
  test_radix_runner(Tree::btree());
}

fn test_insert_update_feedback_runner(r: Tree<usize>) {
  let mut txn = r.txn();

  for i in 0..10 {
    let old = txn.insert(Bytes::from_static(b"helloworld"), i);
    if i == 0 {
      assert!(old.is_none());
    } else {
      assert_eq!(*old.unwrap(), i - 1);
    }
  }
}

#[test]
fn test_insert_update_feedback() {
  test_insert_update_feedback_runner(Tree::vec());
  test_insert_update_feedback_runner(Tree::btree());
}

fn test_remove_runner(mut r: Tree<bool>) {
  const KEYS: &[&str] = &["", "A", "AB"];

  for ss in KEYS.iter() {
    let (nr, _) = r.insert(Bytes::copy_from_slice(ss.as_bytes()), true);
    r = nr;
  }

  for ss in KEYS.iter() {
    let (nr, old) = r.remove(ss.as_bytes());
    r = nr;
    assert!(old.is_some());
  }
}

#[test]
fn test_remove() {
  test_remove_runner(Tree::vec());
  test_remove_runner(Tree::btree());
}

fn verify_tree<T>(r: &Tree<T>, expected: &[&str]) {
  let root = r.root();
  let mut out = Vec::new();
  root.walk(|k, _| {
    out.push(k.to_vec());
    print!("{},", core::str::from_utf8(k).unwrap());
    false
  });
  println!();

  assert_eq!(
    out.len(),
    expected.len(),
    "kind({:?}): bad length",
    r.kind()
  );
  for i in 0..out.len() {
    assert_eq!(
      out[i].as_slice(),
      expected[i].as_bytes(),
      "kind({:?}): bad value at index {}",
      r.kind(),
      i
    );
  }
}

fn test_remove_prefix_runner(r: Tree<bool>) {
  struct Exp {
    desc: &'static str,
    tree_nodes: &'static [&'static str],
    prefix: &'static str,
    expected_out: &'static [&'static str],
  }

  const CASES: &[Exp] = &[
    Exp {
      desc: "prefix not a node in tree",
      tree_nodes: &["", "test/test1", "test/test2", "test/test3", "R", "RA"],
      prefix: "test",
      expected_out: &["", "R", "RA"],
    },
    Exp {
      desc: "prefix is a node in tree",
      tree_nodes: &["", "test/test1", "test/test2", "test/test3", "R", "RA"],
      prefix: "test",
      expected_out: &["", "R", "RA"],
    },
    Exp {
      desc: "prefix matches a node in tree",
      tree_nodes: &[
        "",
        "test",
        "test/test1",
        "test/test2",
        "test/test3",
        "test/testAAA",
        "R",
        "RA",
      ],
      prefix: "test",
      expected_out: &["", "R", "RA"],
    },
    Exp {
      desc: "longer prefix, but prefix is not a node in tree",
      tree_nodes: &[
        "",
        "test/test1",
        "test/test2",
        "test/test3",
        "test/testAAA",
        "R",
        "RA",
      ],
      prefix: "test/test",
      expected_out: &["", "R", "RA"],
    },
    Exp {
      desc: "prefix only matches one node",
      tree_nodes: &["", "AB", "ABC", "AR", "R", "RA"],
      prefix: "AR",
      expected_out: &["", "AB", "ABC", "R", "RA"],
    },
  ];

  for test_case in CASES {
    println!("running test case: {}", test_case.desc);
    let mut r = Tree::<bool>::new_with_kind(r.kind());
    for ss in test_case.tree_nodes.iter() {
      let (nr, _) = r.insert(Bytes::copy_from_slice(ss.as_bytes()), true);
      r = nr;
    }

    let (got, want) = (r.len(), test_case.tree_nodes.len());
    assert_eq!(
      got,
      want,
      "kind({:?}): Unexpected tree length after insert, got {got} want {want}",
      r.kind(),
    );

    r = r.remove_prefix(test_case.prefix.as_bytes()).unwrap();

    // let (got, want) = (r.len(), test_case.expected_out.len());
    // assert_eq!(got, want, "kind({:?}): Unexpected tree length after remove_prefix, got {got} want {want}", r.kind(), );
    verify_tree(&r, test_case.expected_out);
    //Delete a non-existant node
    assert!(r.remove_prefix(b"CCCCC").is_none());
    // verify_tree(&r, test_case.expected_out);
  }
}

#[test]
fn test_remove_prefix() {
  test_remove_prefix_runner(Tree::vec());
  // test_remove_prefix_runner(Tree::btree());
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

fn test_walk_prefix_runner(mut r: Tree<()>) {
  const KEYS: &[&str] = &[
    "foobar",
    "foo/bar/baz",
    "foo/baz/bar",
    "foo/zip/zap",
    "zipzap",
  ];

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

  const CASES: &[(&str, &[&str])] = &[
    (
      "f",
      &["foobar", "foo/bar/baz", "foo/baz/bar", "foo/zip/zap"],
    ),
    (
      "foo",
      &["foobar", "foo/bar/baz", "foo/baz/bar", "foo/zip/zap"],
    ),
    ("foob", &["foobar"]),
    ("foo/", &["foo/bar/baz", "foo/baz/bar", "foo/zip/zap"]),
    ("foo/b", &["foo/bar/baz", "foo/baz/bar"]),
    ("foo/ba", &["foo/bar/baz", "foo/baz/bar"]),
    ("foo/bar", &["foo/bar/baz"]),
    ("foo/bar/baz", &["foo/bar/baz"]),
    ("foo/bar/bazoo", &[]),
    ("z", &["zipzap"]),
  ];

  let root = r.root();
  for (inp, out) in CASES {
    let mut got = Vec::new();
    root.walk_prefix(inp.as_bytes(), |k, _| {
      got.push(String::from_utf8(k.to_vec()).unwrap());
      false
    });
    got.sort();
    let mut out = out.to_vec();
    out.sort();

    assert_eq!(
      got,
      out,
      "kind({:?}): bad walk_prefix length for input {inp}: exp {out:?}, got {got:?}",
      r.kind(),
    );
  }
}

#[test]
fn test_walk_prefix() {
  test_walk_prefix_runner(Tree::vec());
  test_walk_prefix_runner(Tree::btree());
}

#[test]
fn test_walk_path() {
  todo!()
}

#[test]
fn test_iterate_prefix() {
  todo!()
}

fn test_merge_child_nil_edges_runner(mut r: Tree<usize>) {
  let (nr, _) = r.insert(Bytes::from_static(b"foobar"), 42);
  r = nr;

  let (nr, _) = r.insert(Bytes::from_static(b"foozip"), 43);
  r = nr;

  let (nr, _) = r.remove(b"foobar");
  r = nr;

  let root = r.root();
  let mut out = Vec::new();

  root.walk(|k, _| {
    out.push(String::from_utf8(k.to_vec()).unwrap());
    false
  });

  out.sort();
  assert_eq!(out, vec!["foozip"]);
}

#[test]
fn test_merge_child_nil_edges() {
  test_merge_child_nil_edges_runner(Tree::vec());
  test_merge_child_nil_edges_runner(Tree::btree());
}

fn test_merge_child_visibility_runner(mut r: Tree<usize>) {
  let (nr, _) = r.insert(Bytes::from_static(b"foobar"), 42);
  r = nr;

  let (nr, _) = r.insert(Bytes::from_static(b"foobaz"), 43);
  r = nr;

  let (nr, _) = r.insert(Bytes::from_static(b"foozip"), 10);
  r = nr;

  let (nr, _) = r.remove(b"foobar");
  r = nr;

  let txn1 = r.txn();
  let mut txn2 = r.txn();

  // Ensure we get the expected value foobar and foobaz
  assert_eq!(txn1.get(b"foobar"), Some(&42));
  assert_eq!(txn1.get(b"foobaz"), Some(&43));
  assert_eq!(txn2.get(b"foobar"), Some(&42));
  assert_eq!(txn2.get(b"foobaz"), Some(&43));

  // Delete of foozip will cause a merge child between the
  // "foo" and "ba" nodes.
  assert_eq!(*txn2.remove(b"foozip").unwrap(), 10);

  // Insert of "foobaz" will update the slice of the "fooba" node
  // in-place to point to the new "foobaz" node. This in-place update
  // will cause the visibility of the update to leak into txn1 (prior
  // to the fix).
  assert_eq!(*txn2.insert(Bytes::from_static(b"foobaz"), 44).unwrap(), 43);

  // Ensure we get the expected value foobar and foobaz
  assert_eq!(txn1.get(b"foobar"), Some(&42));
  assert_eq!(txn1.get(b"foobaz"), Some(&43));
  assert_eq!(txn2.get(b"foobar"), Some(&42));
  assert_eq!(txn2.get(b"foobaz"), Some(&44));

  // Commit txn2
  r = txn2.commit();

  // Ensure we get the expected value foobar and foobaz
  assert_eq!(txn1.get(b"foobar"), Some(&42));
  assert_eq!(txn1.get(b"foobaz"), Some(&43));
  assert_eq!(r.get(b"foobar"), Some(&42));
  assert_eq!(r.get(b"foobaz"), Some(&44));
}

#[test]
fn test_merge_child_visibility() {
  test_merge_child_visibility_runner(Tree::vec());
  test_merge_child_visibility_runner(Tree::btree());
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

#[test]
fn test_iterate_lower_bound() {
  todo!()
}

#[test]
fn test_iterate_lower_bound_fuzz() {
  todo!()
}

fn test_clond_runner(r: Tree<usize>) {
  let mut t1 = r.txn();
  t1.insert(Bytes::from_static(b"foo"), 7);
  let mut t2 = t1.clone();

  t1.insert(Bytes::from_static(b"bar"), 42);
  t2.insert(Bytes::from_static(b"baz"), 43);

  assert_eq!(t1.get(b"foo"), Some(&7), "bad foo in t1");
  assert_eq!(t2.get(b"foo"), Some(&7), "bad foo in t2");
  assert_eq!(t1.get(b"bar"), Some(&42), "bad bar in t1");
  assert_eq!(t2.get(b"bar"), None, "bad found in t2");
  assert_eq!(t1.get(b"baz"), None, "bad found in t1");
  assert_eq!(t2.get(b"baz"), Some(&43), "bad baz in t2");
}

#[test]
fn test_clone() {
  test_clond_runner(Tree::vec());
  test_clond_runner(Tree::btree());
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

use bytes::Bytes;

use super::*;

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
  assert_eq!(out.len(), expect.len(), "kind({:?}): length mis-match", r.kind());
  for i in 0..out.len() {
    assert_eq!(out[i], expect[i], "kind({:?}): mis-match at index {}", r.kind(), i);
  }
}

#[test]
fn test_radix_huge_txn() {
  test_radix_huge_txn_runner(Tree::vec());
  test_radix_huge_txn_runner(Tree::btree());
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

  assert_eq!(r.len(), KEYS.len(), "kind: {:?} bad len {} {}", r.kind(), r.len(), KEYS.len());

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
        assert_eq!(m, out.as_bytes(), "kind({:?}): bad match for input {inp}: exp {out:?}, got {m:?}", r.kind());
      },
    }
  }
}

#[test]
fn test_longest_prefix() {
  test_longest_prefix_runner(Tree::vec());
  test_longest_prefix_runner(Tree::btree());
}



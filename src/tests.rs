use bytes::Bytes;

use super::*;

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

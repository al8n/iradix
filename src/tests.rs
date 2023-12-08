use super::*;

#[test]
fn test_len_txn() {
  const KEYS: &[&str] = &[
    "foo/bar/baz",
    "foo/baz/bar",
    "foo/zip/zap",
    "foobar",
    "nochange",
  ];
  let mut r = Tree::new();
  let mut txn = r.txn();

  for k in KEYS {
    txn.insert(Bytes::copy_from_slice(k.as_bytes()), ());
  }

  r = txn.commit();
  assert_eq!(r.len(), KEYS.len());

  let mut txn = r.txn();
  for k in KEYS {
    txn.remove(k.as_bytes());
  }
  r = txn.commit();
  assert!(r.is_empty());
}

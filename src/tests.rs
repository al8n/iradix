use super::*;

#[test]
fn test_len_txn() {
  const KEYS: &[&str] = &[
    "foo/bar/baz",
    "foo/baz/bar",
    // "foo/zip/zap",
    // "foobar",
    // "nochange",
  ];
  let mut r = Tree::new();
  let mut txn = r.txn();

  for k in KEYS {
    txn.insert(Bytes::copy_from_slice(k.as_bytes()), ());
    println!("root: {:?}", txn.root);
    txn.root.print();
  }

  r = txn.commit();
  // assert_eq!(r.len(), KEYS.len());

  // r.root.print();

  // let mut txn = r.txn();
  // if txn.remove(KEYS[0].as_bytes()).is_some() {
  //   println!("{}", KEYS[0]);
  // }

  // for k in KEYS {
  //   if txn.remove(k.as_bytes()).is_some() {
  //     println!("{k}");
  //   }
  // }
  // r = txn.commit();
  // println!("{}", r.len());
  // assert!(r.is_empty());
}

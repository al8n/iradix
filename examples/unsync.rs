//! `unsync::Radix` — a single-threaded persistent radix trie.
//!
//! Covers exact lookup, the longest-covered-prefix queries that set a radix trie
//! apart from a plain map, ordered queries, prefix removal, and `O(1)` snapshots.
//!
//! Run with: `cargo run --example unsync`

use std::ops::Bound;

use iradix::unsync::Radix;

fn main() {
  insert_and_query();
  longest_covered_prefix();
  ordered_queries();
  snapshots_are_isolated();
  println!("\nunsync example: ok");
}

/// A `str` key is addressed by `char`, so the component type is `char`. (For
/// path-*segment* rather than per-character semantics, bring an owned segment
/// component — see `RadixKey` in the crate docs.)
fn insert_and_query() {
  let mut config: Radix<char, &str> = Radix::new();
  config.insert("app", "root");
  config.insert("app.db", "database");
  config.insert("app.db.pool", "connection pool");
  config.insert("app.cache", "cache");

  assert_eq!(config.get("app.db"), Some(&"database"));
  assert!(config.contains("app.cache"));
  assert_eq!(config.get("app.queue"), None);
  assert_eq!(config.len(), 4);

  println!("get(\"app.db\")       = {:?}", config.get("app.db"));
  println!("contains(\"app.cache\") = {}", config.contains("app.cache"));
}

/// The query a plain map cannot answer: the nearest stored prefix of a key.
fn longest_covered_prefix() {
  let mut config: Radix<char, &str> = Radix::new();
  config.insert("app", "root");
  config.insert("app.db", "database");
  config.insert("app.db.pool", "connection pool");

  // `get_ancestor` is inclusive: an exact key is its own ancestor.
  assert_eq!(
    config.get_ancestor("app.db.pool.size"),
    Some(&"connection pool")
  );
  assert_eq!(config.get_ancestor("app.db.pool"), Some(&"connection pool"));

  // `strict_ancestor` excludes the exact key (the next prefix up).
  assert_eq!(config.strict_ancestor("app.db.pool"), Some(&"database"));

  println!(
    "\nget_ancestor(\"app.db.pool.size\") = {:?}",
    config.get_ancestor("app.db.pool.size")
  );
  println!(
    "strict_ancestor(\"app.db.pool\")   = {:?}",
    config.strict_ancestor("app.db.pool")
  );
}

fn ordered_queries() {
  let mut t: Radix<char, u32> = Radix::new();
  for (k, v) in [("a", 1), ("ab", 2), ("b", 3), ("c", 4)] {
    t.insert(k, v);
  }

  // `minimum` / `maximum` reconstruct the key as a `Vec<C>` (here `Vec<char>`).
  let (min_key, min_val) = t.minimum().unwrap();
  let (max_key, max_val) = t.maximum().unwrap();
  assert_eq!((min_key.as_slice(), min_val), (['a'].as_slice(), &1));
  assert_eq!((max_key.as_slice(), max_val), (['c'].as_slice(), &4));

  // `range` yields reconstructed `(key, value)` pairs in ascending key order.
  // For an unsized key like `str`, bounds are given as `(Bound, Bound)` of
  // borrows (a range expression `a..b` would be `RangeBounds<&str>`, not `str`).
  let in_range: Vec<(String, u32)> = t
    .range::<str, _>((Bound::Included("ab"), Bound::Included("b")))
    .map(|(k, v)| (k.into_iter().collect(), *v))
    .collect();
  assert_eq!(in_range, vec![("ab".to_string(), 2), ("b".to_string(), 3)]);

  // `seek_lower_bound` is a forward cursor at the first key `>=` the argument.
  let from_b: Vec<u32> = t.seek_lower_bound("b").map(|(_, v)| *v).collect();
  assert_eq!(from_b, vec![3, 4]);

  println!("\nminimum() = {:?}", (min_key, min_val));
  println!("range(\"ab\"..=\"b\") = {in_range:?}");
  println!("seek_lower_bound(\"b\") values = {from_b:?}");
}

fn snapshots_are_isolated() {
  let mut t: Radix<char, u32> = Radix::new();
  t.insert("k", 1);

  let snapshot = t.clone(); // O(1): shares structure, copies no value.
  t.insert("k", 2); // copy-on-write — only the touched path is duplicated.

  assert_eq!(snapshot.get("k"), Some(&1)); // the snapshot is unaffected
  assert_eq!(t.get("k"), Some(&2));

  // `delete_prefix` removes a key and every descendant (node-inclusive).
  t.insert("k2", 3);
  assert_eq!(t.delete_prefix("k"), 2); // "k" and "k2"
  assert!(t.is_empty());

  println!(
    "\nsnapshot.get(\"k\") = {:?} (live trie moved on)",
    snapshot.get("k")
  );
}

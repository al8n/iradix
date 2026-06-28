//! `sync::Radix` — sharing snapshots across threads.
//!
//! `iradix` ships no built-in concurrent holder: `sync::Radix` is `Send + Sync`
//! and an `O(1)`-clone snapshot, so you publish new versions yourself. This shows
//! lock-free concurrent reads, snapshot isolation, and the simplest shared holder
//! (`Arc<Mutex<…>>`), where a reader takes a cheap snapshot under a brief lock and
//! then reads it without holding the lock.
//!
//! Run with: `cargo run --example sync`

use std::{
  sync::{Arc, Mutex},
  thread,
};

use iradix::sync::Radix;

fn main() {
  concurrent_reads();
  snapshot_isolation();
  shared_holder();
  println!("\nsync example: ok");
}

/// `sync::Radix` is `Send + Sync`, so an immutable trie is read from many threads
/// at once with no lock — reads borrow `&self`.
fn concurrent_reads() {
  let mut t: Radix<char, u64> = Radix::new();
  for (k, v) in [("a", 1), ("ab", 2), ("b", 3), ("c", 4)] {
    t.insert(k, v);
  }

  let t = &t; // a shared reference is `Copy`, so each thread gets its own.
  let total: u64 = thread::scope(|s| {
    let handles: Vec<_> = ["a", "ab", "b", "c"]
      .into_iter()
      .map(|key| s.spawn(move || *t.get(key).unwrap()))
      .collect();
    handles.into_iter().map(|h| h.join().unwrap()).sum()
  });

  assert_eq!(total, 1 + 2 + 3 + 4);
  println!("concurrent reads summed to {total}");
}

/// Every mutation yields a logically new trie; an earlier `O(1)` clone is frozen.
fn snapshot_isolation() {
  let mut live: Radix<char, u64> = Radix::new();
  live.insert("v", 1);

  let snapshot = live.clone(); // O(1): shares structure, copies no value.
  live.insert("v", 2);
  live.insert("w", 3);

  assert_eq!(snapshot.get("v"), Some(&1)); // frozen at clone time
  assert_eq!(snapshot.get("w"), None);
  assert_eq!(live.get("v"), Some(&2));
  println!("snapshot pinned v=1 while the live trie moved to v=2 (+w)");
}

/// The publish pattern with the simplest holder. A writer mutates under the lock;
/// a reader takes an `O(1)` clone under a brief lock, releases it, and reads the
/// private snapshot lock-free — isolated from any later write. (For lock-free
/// reads, swap `Mutex` for `arc_swap::ArcSwap<sync::Radix<…>>`.)
fn shared_holder() {
  let holder: Arc<Mutex<Radix<char, u64>>> = Arc::new(Mutex::new(Radix::new()));
  holder.lock().unwrap().insert("x", 10);

  // Brief lock, O(1) clone, then read the snapshot without holding the lock.
  let reader_snapshot = holder.lock().unwrap().clone();

  // A writer thread publishes a new version; the reader's snapshot is unaffected.
  let writer = Arc::clone(&holder);
  thread::spawn(move || writer.lock().unwrap().insert("y", 20))
    .join()
    .unwrap();

  assert_eq!(reader_snapshot.get("y"), None); // frozen before the write
  assert_eq!(holder.lock().unwrap().get("y"), Some(&20)); // holder advanced
  println!("holder published y=20; the earlier snapshot stayed frozen");
}

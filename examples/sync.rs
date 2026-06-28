//! `sync::Radix` — the immutable Tree + Txn model, and lock-free sharing.
//!
//! `sync::Radix` is an immutable, `O(1)`-clone snapshot (like go-immutable-radix's
//! `Tree`). It never mutates in place: a one-op `&self` method returns the next
//! tree, and a `Txn` (an owned working copy) batches edits before `commit`. `iradix`
//! ships no built-in concurrent holder; this shows the lock-free pattern you build
//! yourself with `arc_swap::ArcSwap<sync::Radix<…>>` — readers `load` a wait-free
//! snapshot, and a writer publishes its committed tree with a compare-and-swap
//! retry loop.
//!
//! Run with: `cargo run --example sync`

use std::{
  sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
  },
  thread,
};

use arc_swap::ArcSwap;
use iradix::sync::Radix;

fn main() {
  immutable_reads();
  one_op_insert();
  transaction_batch();
  lock_free_holder();
  println!("\nsync example: ok");
}

/// `sync::Radix` is `Send + Sync`, so one immutable tree is read from many threads
/// at once with no lock — reads borrow `&self`.
fn immutable_reads() {
  let t: Radix<char, u64> = [("a", 1u64), ("ab", 2), ("b", 3), ("c", 4)]
    .into_iter()
    .fold(Radix::new(), |t, (k, v)| {
      let (next, _) = t.insert(k, v);
      next
    });

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

/// A one-op `&self` mutator returns the *new* tree; the original is frozen.
fn one_op_insert() {
  let base: Radix<char, u64> = Radix::new();
  let (v1, prev) = base.insert("v", 1);
  assert_eq!(prev, None);

  let (v2, prev) = v1.insert("v", 2); // replaces, reporting the old value
  assert_eq!(prev, Some(1));

  assert_eq!(base.get("v"), None); // original: still empty
  assert_eq!(v1.get("v"), Some(&1)); // first version: frozen at 1
  assert_eq!(v2.get("v"), Some(&2)); // newest version
  println!("one-op insert produced v1=1 and v2=2, base still empty");
}

/// A `Txn` is an owned working copy: batch several edits, then `commit` into the
/// next immutable tree. The tree the txn was opened from never changes.
fn transaction_batch() {
  let base: Radix<char, u64> = Radix::new();

  let mut txn = base.txn();
  txn.insert("a", 1);
  txn.insert("ab", 2);
  txn.insert("b", 3);
  assert_eq!(txn.get("ab"), Some(&2)); // read-your-writes inside the txn
  let tree = txn.commit();

  assert_eq!(tree.len(), 3);
  assert_eq!(base.len(), 0); // the pre-txn tree is untouched
  println!("a transaction batched 3 inserts; base stayed empty");
}

/// The lock-free shared-holder pattern. Readers `load` a wait-free snapshot; a
/// writer opens a `Txn`, `commit`s it, then publishes with a compare-and-swap retry
/// loop so a write is never silently lost when two writers race.
fn lock_free_holder() {
  let holder: Arc<ArcSwap<Radix<u8, u64>>> = Arc::new(ArcSwap::from_pointee(Radix::new()));
  // Counts publishes that actually had to retry, proving the CAS loop is exercised.
  let retries = Arc::new(AtomicU64::new(0));

  const WRITERS: u8 = 4;

  thread::scope(|s| {
    // A reader thread loads wait-free snapshots while writers publish concurrently.
    let reader = {
      let holder = Arc::clone(&holder);
      s.spawn(move || {
        for _ in 0..1000 {
          // `load` is wait-free; the snapshot it returns is a frozen, consistent
          // tree regardless of any concurrent publish.
          let snap = holder.load();
          let _ = snap.len();
        }
      })
    };

    // Several writer threads each insert their own key with the CAS retry loop.
    let writers: Vec<_> = (0..WRITERS)
      .map(|w| {
        let holder = Arc::clone(&holder);
        let retries = Arc::clone(&retries);
        s.spawn(move || {
          loop {
            let current = holder.load_full(); // Arc<Radix<…>>
            let mut txn = current.txn();
            txn.insert([w].as_slice(), u64::from(w));
            let next = Arc::new(txn.commit());

            // Publish only if no other writer beat us; otherwise retry on the
            // latest. `compare_and_swap` returns the value that was there: the swap
            // happened iff it is pointer-equal to what we proposed replacing.
            let prev = holder.compare_and_swap(&current, next);
            if Arc::ptr_eq(&current, &prev) {
              break;
            }
            retries.fetch_add(1, Ordering::Relaxed);
          }
        })
      })
      .collect();

    reader.join().unwrap();
    for h in writers {
      h.join().unwrap();
    }
  });

  // After all writers join, every key must be present exactly once — no lost write.
  let final_tree = holder.load_full();
  assert_eq!(final_tree.len(), usize::from(WRITERS));
  for w in 0..WRITERS {
    assert_eq!(final_tree.get([w].as_slice()), Some(&u64::from(w)));
  }
  println!(
    "lock-free holder: {WRITERS} writers all landed ({} CAS retr{} along the way)",
    retries.load(Ordering::Relaxed),
    if retries.load(Ordering::Relaxed) == 1 {
      "y"
    } else {
      "ies"
    }
  );
}

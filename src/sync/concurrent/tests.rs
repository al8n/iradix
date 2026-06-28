use super::*;

type Conc = ConcurrentRadix<u8, u32>;

const fn assert_send<T: Send>() {}
const fn assert_sync<T: Sync>() {}

#[test]
fn concurrent_radix_is_send_sync() {
  assert_send::<Conc>();
  assert_sync::<Conc>();
}

#[test]
fn load_then_commit_is_isolated() {
  let holder: Conc = ConcurrentRadix::new();
  holder.commit_with(|txn| {
    txn.insert(b"a".as_slice(), 1);
  });
  let snap = holder.load();
  holder.commit_with(|txn| {
    txn.insert(b"a".as_slice(), 2);
    txn.insert(b"a/b".as_slice(), 3);
  });
  // snapshot taken before the second commit is unaffected
  assert_eq!(snap.get(b"a".as_slice()), Some(&1));
  assert_eq!(snap.get(b"a/b".as_slice()), None);
  // live state reflects the commit
  let live = holder.load();
  assert_eq!(live.get(b"a".as_slice()), Some(&2));
  assert_eq!(live.get(b"a/b".as_slice()), Some(&3));
}

#[test]
fn txn_commit_succeeds_uncontended() {
  let holder: Conc = ConcurrentRadix::new();
  let mut txn = holder.txn();
  txn.insert(b"k".as_slice(), 1);
  assert!(txn.commit().is_ok());
  assert_eq!(holder.load().get(b"k".as_slice()), Some(&1));
}

#[test]
fn concurrent_txn_loses_race_then_retries() {
  // Two transactions snapshot the same base. The first to commit wins; the
  // second sees a conflict because the published root changed underneath it.
  let holder: Conc = ConcurrentRadix::new();
  holder.commit_with(|txn| {
    txn.insert(b"a".as_slice(), 1);
  });

  let mut t1 = holder.txn();
  let mut t2 = holder.txn(); // same base as t1

  t1.insert(b"a".as_slice(), 10);
  assert!(t1.commit().is_ok(), "first commit wins");

  // t2 built on the now-stale base: its commit must conflict.
  t2.insert(b"b".as_slice(), 20);
  assert_eq!(t2.commit(), Err(Conflict));

  // The loser retries from a fresh snapshot and succeeds, observing t1's write.
  holder.commit_with(|txn| {
    assert_eq!(txn.get_ancestor(b"a".as_slice()), Some(&10));
    txn.insert(b"b".as_slice(), 20);
  });

  let snap = holder.load();
  assert_eq!(snap.get(b"a".as_slice()), Some(&10));
  assert_eq!(snap.get(b"b".as_slice()), Some(&20));
}

#[test]
fn commit_returns_closure_value() {
  let holder: Conc = ConcurrentRadix::new();
  let old = holder.commit_with(|txn| txn.insert(b"k".as_slice(), 1));
  assert_eq!(old, None);
  let old = holder.commit_with(|txn| txn.insert(b"k".as_slice(), 2));
  assert_eq!(old, Some(1));
}

#[test]
fn dropped_txn_publishes_nothing() {
  let holder: Conc = ConcurrentRadix::new();
  holder.commit_with(|txn| {
    txn.insert(b"a".as_slice(), 1);
  });

  {
    let mut txn = holder.txn();
    txn.insert(b"a".as_slice(), 2);
    txn.insert(b"a/b".as_slice(), 3);
    // dropped without commit
  }

  // Nothing changed: a dropped working copy is never published.
  let snap = holder.load();
  assert_eq!(snap.get(b"a".as_slice()), Some(&1));
  assert_eq!(snap.get(b"a/b".as_slice()), None);
  assert_eq!(snap.len(), 1);
}

#[test]
fn panic_mid_build_publishes_nothing() {
  // A panic while building a transaction drops the private working copy; the
  // published root is untouched and the holder stays usable.
  use std::panic::{AssertUnwindSafe, catch_unwind};

  let holder: Conc = ConcurrentRadix::new();
  holder.commit_with(|txn| {
    txn.insert(b"a".as_slice(), 1);
  });

  let r = catch_unwind(AssertUnwindSafe(|| {
    let mut txn = holder.txn();
    txn.insert(b"a".as_slice(), 2);
    txn.insert(b"a/b".as_slice(), 3);
    panic!("build blew up after partial edits");
  }));
  assert!(r.is_err(), "the build must have panicked");

  let snap = holder.load();
  assert_eq!(snap.get(b"a".as_slice()), Some(&1));
  assert_eq!(snap.get(b"a/b".as_slice()), None);
  assert_eq!(snap.len(), 1);

  // Still usable after the panicked build.
  holder.commit_with(|txn| {
    txn.insert(b"c".as_slice(), 9);
  });
  assert_eq!(holder.load().get(b"c".as_slice()), Some(&9));
}

#[test]
fn txn_forwards_ordered_ops() {
  let holder: Conc = ConcurrentRadix::new();
  let mut txn = holder.txn();
  txn.insert(b"a".as_slice(), 1);
  txn.insert(b"a/b".as_slice(), 2);
  txn.insert(b"a/c".as_slice(), 3);
  txn.insert(b"d".as_slice(), 4);

  // ordered reads on the working copy
  assert_eq!(txn.minimum(), Some((b"a".to_vec(), &1)));
  assert_eq!(txn.maximum(), Some((b"d".to_vec(), &4)));
  let rev: std::vec::Vec<u32> = txn.values_rev().copied().collect();
  assert_eq!(rev, std::vec![4, 3, 2, 1]);
  let seek: std::vec::Vec<u32> = txn
    .seek_lower_bound(b"a/b".as_slice())
    .map(|(_, v)| *v)
    .collect();
  assert_eq!(seek, std::vec![2, 3, 4]);
  let ranged: std::vec::Vec<u32> = txn
    .range::<[u8], _>((
      core::ops::Bound::Included(b"a".as_slice()),
      core::ops::Bound::Excluded(b"d".as_slice()),
    ))
    .map(|(_, v)| *v)
    .collect();
  assert_eq!(ranged, std::vec![1, 2, 3]);

  // node-inclusive prefix removal on the working copy
  assert_eq!(txn.drain_prefix(b"a".as_slice()), std::vec![1, 2, 3]);
  txn.insert(b"a".as_slice(), 5);
  assert_eq!(txn.delete_prefix(b"a".as_slice()), 1);
  txn.commit().unwrap();

  let snap = holder.load();
  assert_eq!(snap.get(b"a".as_slice()), None);
  assert_eq!(snap.get(b"d".as_slice()), Some(&4));
  assert_eq!(snap.len(), 1);
}

#[test]
fn from_radix_seeds_holder() {
  let mut seed: Radix<u8, u32> = Radix::new();
  seed.insert(&b"seed".to_vec(), 42);
  let holder: Conc = ConcurrentRadix::from_radix(seed);
  assert_eq!(holder.load().get(b"seed".as_slice()), Some(&42));
}

#[cfg(feature = "std")]
#[test]
fn parallel_writers_via_retry_serialize() {
  use std::sync::Arc;
  use std::thread;

  let holder: Arc<Conc> = Arc::new(ConcurrentRadix::new());
  let threads: Vec<_> = (0u8..8)
    .map(|t| {
      let holder = Arc::clone(&holder);
      thread::spawn(move || {
        for i in 0u8..32 {
          // commit_with retries on conflict until the publish wins, so every
          // write lands despite the CAS contention.
          holder.commit_with(|txn| {
            txn.insert([t, i].as_slice(), u32::from(t) * 100 + u32::from(i));
          });
        }
      })
    })
    .collect();
  for h in threads {
    h.join().unwrap();
  }
  let snap = holder.load();
  assert_eq!(snap.len(), 8 * 32);
  for t in 0u8..8 {
    for i in 0u8..32 {
      assert_eq!(
        snap.get([t, i].as_slice()),
        Some(&(u32::from(t) * 100 + u32::from(i)))
      );
    }
  }
}

// A successful commit on the alloc (spin) backend must drop the replaced trie
// OUTSIDE the write lock: a stored value's (non-panicking) `Drop` may re-enter
// the holder via `load`/`txn`, which would deadlock on the non-reentrant spin lock
// if the drop ran while the lock was still held. Gated to the alloc backend (the
// std backend is lock-free) and off miri (real threads + timing).
#[cfg(all(not(feature = "std"), feature = "alloc", not(miri)))]
mod drop_outside_lock {
  use super::*;
  use core::{cell::Cell, ptr};
  use std::{
    sync::{Arc, mpsc},
    thread,
    time::Duration,
  };

  thread_local! {
    static HOLDER: Cell<*const ConcurrentRadix<u8, Reenter>> = const { Cell::new(ptr::null()) };
  }

  // On drop, re-enters the holder with a wait-free `load`. If the holder dropped
  // this value while holding the write lock, the `load` deadlocks.
  #[derive(Clone, Debug, PartialEq)]
  struct Reenter(u32);

  impl Drop for Reenter {
    fn drop(&mut self) {
      let holder = HOLDER.with(Cell::get);
      if !holder.is_null() {
        // SAFETY: set to a live `Arc`'s pointer for the duration of the commit
        // below, and cleared before that `Arc` is dropped.
        let holder = unsafe { &*holder };
        let _ = holder.load();
      }
    }
  }

  #[test]
  fn commit_drops_replaced_tree_without_deadlock() {
    let holder: Arc<ConcurrentRadix<u8, Reenter>> = Arc::new(ConcurrentRadix::new());
    {
      let mut txn = holder.txn();
      txn.insert(b"k".as_slice(), Reenter(1));
      txn.commit().unwrap();
    }

    let (tx, rx) = mpsc::channel();
    let worker = holder.clone();
    let handle = thread::spawn(move || {
      HOLDER.with(|c| c.set(Arc::as_ptr(&worker)));
      // Overwriting "k" makes the first tree the sole owner of `Reenter(1)`, so
      // committing the second tree drops it — re-entering the holder.
      let mut txn = worker.txn();
      txn.insert(b"k".as_slice(), Reenter(2));
      txn.commit().unwrap();
      HOLDER.with(|c| c.set(ptr::null()));
      let _ = tx.send(());
    });

    rx.recv_timeout(Duration::from_secs(5))
      .expect("commit deadlocked: the replaced tree was dropped under the write lock");
    handle.join().unwrap();
    assert_eq!(holder.load().get(b"k".as_slice()), Some(&Reenter(2)));
  }
}

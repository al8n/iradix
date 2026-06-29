//! `watch` — block or await until a key or prefix changes in a *published* version.
//!
//! Run with: `cargo run --example watch --features watch`
//!
//! The discipline is **commit → publish → notify**. `commit` builds the next tree
//! but fires nothing; the writer then tries to install it with a compare-and-swap,
//! and only the *winner* notifies (relative to the version it was opened from).
//! `Radix::publish_to` folds the CAS and the notify into one call, so a tree that
//! loses the race never wakes a watcher. A `Watch` is armed against one immutable
//! snapshot — use `get_watch` to read a value and arm against the same snapshot in
//! one call; the sticky flag returns an already-resolved `Watch` if that node was
//! already replaced. To track a key over time, loop: reload, re-arm, wait.

use std::{sync::Arc, thread};

use arc_swap::ArcSwap;
use iradix::sync::Radix;

/// Open a txn on the current live version, apply `edit`, then publish with a CAS
/// retry loop. The winning publish notifies relative to the version it was opened
/// from (via `publish_to`); a lost CAS retries against the new live version and
/// notifies nothing. This is the lock-free, watch-safe writer.
fn publish(
  holder: &ArcSwap<Radix<u8, u32>>,
  mut edit: impl FnMut(&mut iradix::sync::Txn<u8, u32>),
) {
  loop {
    let base = holder.load_full();
    let mut txn = base.txn();
    edit(&mut txn);
    let next = Arc::new(txn.commit()); // commit builds the tree; fires nothing
    let mut won = false;
    next.publish_to(&base, || {
      // Install `next` iff the live version is still `base`. `compare_and_swap`
      // returns the value that *was* there; pointer-equality with `base` means ours
      // landed. `publish_to` notifies only on this `true`.
      let prev = holder.compare_and_swap(&base, Arc::clone(&next));
      won = Arc::ptr_eq(&base, &prev);
      won
    });
    if won {
      break;
    }
    // Lost the race: loop and retry against the latest published tree.
  }
}

fn main() {
  // An initial tree, published behind an `ArcSwap` so a reader and a writer share
  // it lock-free (the same shape go-memdb's watch builds on).
  let initial = {
    let mut t = Radix::<u8, u32>::new().txn();
    t.insert(b"config/timeout".as_slice(), 30);
    t.insert(b"config/retries".as_slice(), 3);
    t.commit()
  };
  let shared = Arc::new(ArcSwap::from_pointee(initial));

  // ---- Blocking watch on a single key (read + arm on one snapshot) -----------
  {
    let current = shared.load_full();
    // `get_watch` reads the value and arms the watch against the SAME immutable
    // snapshot; the sticky flag returns an already-resolved watch if a publish
    // already replaced this node.
    let (before, w) = current.get_watch(b"config/timeout".as_slice());
    println!("timeout before = {before:?}");

    let writer = {
      let shared = Arc::clone(&shared);
      thread::spawn(move || {
        // commit -> publish (CAS) -> notify, all inside `publish`.
        publish(&shared, |t| {
          t.insert(b"config/timeout".as_slice(), 60);
        });
      })
    };

    w.block_wait(); // wakes once the writer PUBLISHES a change to config/timeout
    writer.join().unwrap();
    let after = shared.load().get(b"config/timeout".as_slice()).copied();
    println!("timeout changed -> {after:?}");
    assert_eq!(after, Some(60));
  }

  // ---- Prefix watch: any change anywhere under a subtree --------------------
  {
    let current = shared.load_full();
    let w = current.watch_prefix(b"config".as_slice());
    let writer = {
      let shared = Arc::clone(&shared);
      thread::spawn(move || {
        publish(&shared, |t| {
          t.insert(b"config/backoff".as_slice(), 100); // a *new* key under config/
        });
      })
    };
    w.block_wait(); // any published change under "config" wakes the prefix watch
    writer.join().unwrap();
    println!("a change occurred somewhere under config/");
  }

  // ---- A lost CAS must NOT wake a watcher on an unrelated key ---------------
  {
    // Arm a watch on "config/retries", then deliberately drive a writer whose
    // commit targets a *different* key but whose publish is made to LOSE: we sneak
    // an interloping publish in first, then attempt a stale CAS by hand. The losing
    // tree must notify nothing, so the retries watch stays asleep.
    let base = shared.load_full();
    let w_retries = base.watch(b"config/retries".as_slice());

    // The loser: commit a change to "config/backoff" against `base`...
    let loser = Arc::new({
      let mut t = base.txn();
      t.insert(b"config/backoff".as_slice(), 999);
      t.commit()
    });
    // ...but an interloper publishes first, so `base` is no longer live.
    publish(&shared, |t| {
      t.insert(b"config/timeout".as_slice(), 61);
    });
    // Now the loser's CAS against the stale `base` fails — and `publish_to` fires
    // nothing on the loss.
    loser.publish_to(&base, || {
      let prev = shared.compare_and_swap(&base, Arc::clone(&loser));
      Arc::ptr_eq(&base, &prev) // false: `base` was already superseded
    });
    // The retries watch never fired (nothing touched retries, and the loser was
    // discarded without notifying). Confirm it is still pending with a tiny wait.
    let still_pending = !w_retries.block_wait_timeout(std::time::Duration::from_millis(50));
    assert!(
      still_pending,
      "a lost CAS must not wake an unrelated watcher"
    );
    println!("lost-CAS publish woke nobody (as it must)");
  }

  // ---- Async: the same watch awaited instead of blocked --------------------
  {
    let current = shared.load_full();
    let (_, w) = current.get_watch(b"config/retries".as_slice());
    publish(&shared, |t| {
      t.insert(b"config/retries".as_slice(), 5);
    });
    pollster::block_on(w.changed()); // resolves once the change is published
    println!("retries changed (observed via .changed().await)");
    assert_eq!(
      shared.load().get(b"config/retries".as_slice()).copied(),
      Some(5)
    );
  }

  // ---- Track a key across versions: the reload-and-re-arm loop -------------
  {
    // A `Watch` is single-use and bound to the version it was armed on. To follow a
    // key over several publishes, reload the holder, re-arm, then wait again.
    let shared_writer = Arc::clone(&shared);
    let writer = thread::spawn(move || {
      for v in 10..13 {
        publish(&shared_writer, |t| {
          t.insert(b"counter".as_slice(), v);
        });
        thread::yield_now();
      }
    });

    let mut last = None;
    while last != Some(12) {
      let snap = shared.load_full(); // reload the live version
      let (value, w) = snap.get_watch(b"counter".as_slice()); // read + arm
      last = value.copied();
      if last == Some(12) {
        break;
      }
      w.block_wait(); // block until the next publish touches "counter"
    }
    writer.join().unwrap();
    println!("counter reached {:?} via reload-and-re-arm", last);
  }
}

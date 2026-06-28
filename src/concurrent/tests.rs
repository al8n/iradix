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
fn snapshot_then_write_is_isolated() {
  let holder: Conc = ConcurrentRadix::new();
  holder.write(|txn| {
    txn.insert(b"a".as_slice(), 1);
  });
  let snap = holder.snapshot();
  holder.write(|txn| {
    txn.insert(b"a".as_slice(), 2);
    txn.insert(b"a/b".as_slice(), 3);
  });
  assert_eq!(snap.get(b"a".as_slice()), Some(&1));
  assert_eq!(snap.get(b"a/b".as_slice()), None);
  let live = holder.snapshot();
  assert_eq!(live.get(b"a".as_slice()), Some(&2));
  assert_eq!(live.get(b"a/b".as_slice()), Some(&3));
}

#[test]
fn write_returns_closure_value() {
  let holder: Conc = ConcurrentRadix::new();
  let old = holder.write(|txn| txn.insert(b"k".as_slice(), 1));
  assert_eq!(old, None);
  let old = holder.write(|txn| txn.insert(b"k".as_slice(), 2));
  assert_eq!(old, Some(1));
}

#[test]
fn concurrent_get_ancestor() {
  let holder: Conc = ConcurrentRadix::new();
  holder.write(|txn| {
    txn.insert(b"a".as_slice(), 1);
    txn.insert(b"a/b/c".as_slice(), 2);
  });
  assert_eq!(holder.get_ancestor(b"a/b/c/d".as_slice()), Some(2));
  assert_eq!(holder.get_ancestor(b"a/x".as_slice()), Some(1));
  assert!(holder.has_ancestor(b"a/anything".as_slice()));
  assert!(!holder.has_ancestor(b"z".as_slice()));
}

#[cfg(feature = "std")]
#[test]
fn parallel_writers_serialize() {
  use std::sync::Arc;
  use std::thread;

  let holder: Arc<Conc> = Arc::new(ConcurrentRadix::new());
  let threads: Vec<_> = (0u8..8)
    .map(|t| {
      let holder = Arc::clone(&holder);
      thread::spawn(move || {
        for i in 0u8..32 {
          holder.write(|txn| {
            txn.insert([t, i].as_slice(), u32::from(t) * 100 + u32::from(i));
          });
        }
      })
    })
    .collect();
  for h in threads {
    h.join().unwrap();
  }
  let snap = holder.snapshot();
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

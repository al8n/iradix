//! `watch_timeout` — await a published change with a bound, runtime-agnostically.
//!
//! Run with: `cargo run --example watch_timeout --features tokio`
//! (the `tokio` feature re-exports `iradix::TokioRuntime`; no direct agnostic-lite
//! dependency is needed).
//!
//! `Watch::changed_timeout::<R>(d)` resolves `Ok(())` when the watched key changes
//! in a *published* version, or `Err(Elapsed)` once `d` elapses. The runtime is the
//! type parameter `R` — here `TokioRuntime`; swap it for `SmolRuntime`,
//! `WasmRuntime`, `EmbassyRuntime`, … without touching the call. The crate itself
//! pulls in no runtime. To track a key over time, loop: reload, re-arm, await.

use std::{sync::Arc, time::Duration};

use arc_swap::ArcSwap;
use iradix::{TokioRuntime, sync::Radix};

#[tokio::main]
async fn main() {
  let holder = Arc::new(ArcSwap::from_pointee({
    let mut t = Radix::<u8, u32>::new().txn();
    t.insert(b"config/timeout".as_slice(), 30);
    t.commit()
  }));

  // Read the live value and arm a watch on the same snapshot.
  let base = holder.load_full();
  let (before, watch) = base.get_watch(b"config/timeout".as_slice());
  println!("timeout before = {before:?}");

  // A writer publishes a change shortly: commit -> CAS -> notify (only the winner).
  let writer = {
    let holder = Arc::clone(&holder);
    let base = Arc::clone(&base);
    tokio::spawn(async move {
      tokio::time::sleep(Duration::from_millis(50)).await;
      let next = Arc::new({
        let mut t = base.txn();
        t.insert(b"config/timeout".as_slice(), 60);
        t.commit()
      });
      next.publish_to(&base, || {
        let prev = holder.compare_and_swap(&base, Arc::clone(&next));
        Arc::ptr_eq(&base, &prev)
      });
    })
  };

  // Await the change for up to 2s — runtime-agnostic, driven by agnostic-lite.
  match watch
    .changed_timeout::<TokioRuntime>(Duration::from_secs(2))
    .await
  {
    Ok(()) => {
      let after = holder.load().get(b"config/timeout".as_slice()).copied();
      println!("changed in time -> {after:?}");
      assert_eq!(after, Some(60));
    }
    Err(_elapsed) => panic!("the writer published well within the budget"),
  }
  writer.await.unwrap();

  // A watch on a key nothing touches is *expected* to time out.
  let snap = holder.load_full();
  let idle = snap.watch(b"missing".as_slice());
  match idle
    .changed_timeout::<TokioRuntime>(Duration::from_millis(50))
    .await
  {
    Ok(()) => unreachable!("nothing changed \"missing\""),
    Err(_elapsed) => println!("(expected) timed out: no change to \"missing\""),
  }
}

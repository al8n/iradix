use super::*;

use core::{
  future::Future,
  pin::Pin,
  task::{Context, Poll},
};

#[cfg(any(feature = "std", feature = "future"))]
use core::time::Duration;

#[cfg(feature = "future")]
use std::boxed::Box;

#[cfg(feature = "future")]
use agnostic_lite::{RuntimeLite, time::Elapsed};

impl<C, V> Txn<C, V> {
  /// Consumes the transaction and returns the next immutable [`Radix`] holding all
  /// committed edits.
  ///
  /// Committing builds the next tree only; it fires **no** `watch` events. Publishing
  /// follows **commit → publish → notify**: after the returned tree wins publication
  /// (e.g. a successful compare-and-swap), call [`Radix::notify_changes_since`]
  /// against the version this transaction was opened from — or fold the publish and
  /// notify together with [`Radix::publish_to`]. A committed-then-discarded tree (a
  /// lost CAS) must NOT notify.
  #[cfg(feature = "watch")]
  #[inline]
  #[must_use = "returns the new tree holding the committed edits"]
  pub fn commit(self) -> Radix<C, V> {
    let mut working = self.working;
    // Canonicalize a *logically* empty working copy to physical `None`-root FIRST, so
    // "empty ⟺ root is None" holds: a net-empty txn (insert-then-remove from an empty
    // base) commits `len == 0` but with a physical `root = Some(empty node)`, which
    // the publish-time diff would otherwise misread as a None -> Some transition and
    // use to spuriously wake empty-position watchers though nothing was published.
    working.canonicalize_empty();
    // Carry an empty-position slot *per empty-epoch* so the empty-then-insert no-op
    // case still wakes watchers (see `Radix::notify_changes_since`): a run of
    // consecutive empty versions shares one slot, and a fresh slot opens only when
    // the trie newly becomes empty. The slot is dormant for non-empty versions.
    let empty = if working.root_is_none() {
      if self.base.root_is_none() {
        // None -> None: same empty epoch, carry the base's slot so an arm on the
        // base and an arm on this commit fire together when a later insert lands.
        self.empty.clone()
      } else {
        // Some -> None: a new empty epoch begins; nothing armed on the prior
        // (non-empty) version's empty slot, so start fresh.
        std::sync::Arc::new(WatchSlot::new())
      }
    } else {
      // Non-empty: the empty slot is dormant; carry it so the epoch is preserved if
      // a later commit empties the trie back out without changing it in between.
      self.empty.clone()
    };
    Radix {
      inner: working,
      empty,
    }
  }
}

/// A pending observation of a key or prefix, returned by [`Radix::watch`] /
/// [`Radix::watch_prefix`] / [`Radix::get_watch`].
///
/// A `Watch` is **edge-triggered against the exact snapshot it was armed on**: it
/// resolves *once*, for the next change to the watched key (or anything in its
/// subtree) that is **published** to that version — that is, a later
/// [`commit`](Txn::commit) whose tree is then made live and calls
/// [`notify_changes_since`](Radix::notify_changes_since) (committing alone fires
/// nothing). It is single-use, and bound to that one version: never reuse a `Watch`
/// after it fires, and never arm against a snapshot different from the one you read.
///
/// A `Watch` is armed against one immutable snapshot: it registers a listener on
/// that snapshot's node slot and re-checks the node's sticky flag, so a change
/// published to that snapshot is never missed — and if the node was already replaced
/// by an earlier publish, the `Watch` is created already-resolved. (The listener plus
/// the sticky-flag re-check is what closes the race; the snapshot is immutable, so
/// whether you read a value before or after arming does not matter.) Use
/// [`get_watch`](Radix::get_watch) to read a value and arm against the *same*
/// snapshot in one call.
///
/// It may **over-notify**: because there is one change channel per node and any
/// ancestor of a change is path-copied, a change to a *descendant* (or a sibling
/// merged onto the same node) can wake a key watcher whose own value did not change
/// — re-read to confirm. It never under-notifies, given non-panicking key
/// comparisons and async wakers (a panic in either during notification is out of
/// scope, like a panicking `Drop`).
///
/// To track a key across versions, loop: read-and-arm via
/// [`get_watch`](Radix::get_watch), wait, then **reload the holder and re-arm**
/// against the new live version before waiting again.
///
/// ```no_run
/// # #[cfg(all(feature = "watch", feature = "std"))] {
/// use std::sync::Arc;
/// use arc_swap::ArcSwap;
/// use iradix::sync::Radix;
///
/// let holder: Arc<ArcSwap<Radix<u8, u32>>> = Arc::new(ArcSwap::from_pointee(Radix::new()));
/// loop {
///   let snap = holder.load_full();              // reload the live version
///   let (value, watch) = snap.get_watch(b"k".as_slice()); // read + arm on one snapshot
///   # let _ = value;
///   watch.block_wait();                         // block until the next published change
///   # break;
/// }
/// # }
/// ```
pub struct Watch(Option<EventListener>);

/// The future returned by [`Watch::changed`].
///
/// Resolves (to `()`) when the watched key or prefix changes in a published
/// version — immediately if it already has. Runtime-agnostic and `no_std + alloc`:
/// drive it with any async executor.
#[must_use = "futures do nothing unless awaited"]
pub struct Changed(Option<EventListener>);

impl Future for Changed {
  type Output = ();

  #[inline]
  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    // `EventListener` is `Unpin`, so the inner listener can be polled through a
    // plain `&mut` without structural pinning.
    match &mut self.get_mut().0 {
      Some(listener) => Pin::new(listener).poll(cx),
      None => Poll::Ready(()),
    }
  }
}

impl Watch {
  /// Blocks the current thread until the watched key or prefix changes (returns at
  /// once if it already has). The blocking counterpart to `changed`.
  #[cfg(feature = "std")]
  #[cfg_attr(docsrs, doc(cfg(feature = "std")))]
  #[inline]
  pub fn block_wait(self) {
    if let Some(listener) = self.0 {
      listener.wait();
    }
  }

  /// Blocks until the watched key or prefix changes, or `timeout` elapses. Returns
  /// `true` if it changed (or already had), `false` on timeout. The blocking
  /// counterpart to `changed_timeout`.
  #[cfg(feature = "std")]
  #[cfg_attr(docsrs, doc(cfg(feature = "std")))]
  #[inline]
  pub fn block_wait_timeout(self, timeout: Duration) -> bool {
    match self.0 {
      None => true,
      Some(listener) => listener.wait_timeout(timeout).is_some(),
    }
  }

  /// Returns a future that resolves when the watched key or prefix changes
  /// (immediately if it already has) — the async counterpart to `block_wait`.
  /// Runtime-agnostic: drive it with any async executor; works on `no_std + alloc`.
  #[inline]
  #[must_use = "the returned future does nothing unless awaited"]
  pub fn changed(self) -> Changed {
    Changed(self.0)
  }

  /// Like `changed`, but gives up after `timeout` — a lazy [`ChangedTimeout`] future.
  ///
  /// Resolves to `Ok(())` if the watched key or prefix changed in time, or
  /// `Err(`[`Elapsed`](agnostic_lite::time::Elapsed)`)` on timeout — the async
  /// counterpart to `block_wait_timeout`. The runtime is chosen with the type
  /// parameter `R` (e.g. `TokioRuntime`, `SmolRuntime`, `WasmRuntime`,
  /// `EmbassyRuntime`), so the crate stays runtime-agnostic; works on
  /// `no_std + alloc` (e.g. the embassy backend). The `future` feature brings
  /// only the `RuntimeLite` trait; turn on the `tokio`
  /// or `smol` feature for those backends, or add `agnostic-lite` with another
  /// backend, then name `R` from `agnostic_lite` (e.g.
  /// `agnostic_lite::tokio::TokioRuntime`).
  ///
  /// ```no_run
  /// # #[cfg(feature = "tokio")] {
  /// use core::time::Duration;
  /// use iradix::{TokioRuntime, sync::Watch};
  ///
  /// async fn reload_on_change(watch: Watch) {
  ///   match watch.changed_timeout::<TokioRuntime>(Duration::from_secs(5)).await {
  ///     Ok(()) => { /* changed in time — reload the holder and re-arm */ }
  ///     Err(_elapsed) => { /* timed out — still pending */ }
  ///   }
  /// }
  /// # }
  /// ```
  #[cfg(feature = "future")]
  #[cfg_attr(docsrs, doc(cfg(feature = "future")))]
  #[inline]
  pub fn changed_timeout<R>(self, timeout: Duration) -> ChangedTimeout<R>
  where
    R: agnostic_lite::RuntimeLite,
  {
    ChangedTimeout {
      changed: Some(self.changed()),
      timeout,
      armed: None,
    }
  }
}

/// The future returned by [`Watch::changed_timeout`]. Resolves to `Ok(())` when the
/// watched key or prefix changes in a published version, or
/// `Err(`[`Elapsed`](agnostic_lite::time::Elapsed)`)` once the timeout elapses.
///
/// Lazy: the runtime timer is built on the first poll (inside the executor), not when
/// `changed_timeout` is called — so constructing it never touches the runtime, and the
/// timeout budget starts when the future is first polled.
#[cfg(feature = "future")]
#[cfg_attr(docsrs, doc(cfg(feature = "future")))]
#[must_use = "futures do nothing unless awaited"]
pub struct ChangedTimeout<R: RuntimeLite> {
  changed: Option<Changed>,
  timeout: Duration,
  // Boxed so this future is `Unpin` even when the runtime's timeout is not, keeping
  // the crate free of unsafe pin projection; built on first poll for laziness.
  armed: Option<Pin<Box<R::Timeout<Changed>>>>,
}

#[cfg(feature = "future")]
impl<R: RuntimeLite> Future for ChangedTimeout<R> {
  type Output = Result<(), Elapsed>;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let this = self.get_mut();
    if let Some(armed) = this.armed.as_mut() {
      return armed.as_mut().poll(cx);
    }
    // First poll: build the runtime timeout now, inside the executor — never at
    // construction, so it neither touches the runtime early nor starts the budget late.
    let changed = this
      .changed
      .take()
      .expect("changed is Some until the timeout is armed on first poll");
    this
      .armed
      .insert(Box::pin(R::timeout(this.timeout, changed)))
      .as_mut()
      .poll(cx)
  }
}

impl<C, V> Radix<C, V>
where
  C: Ord,
{
  /// Wake every [`Watch`] armed on `base` whose key — or any key in its subtree —
  /// differs in `self`.
  ///
  /// Call EXACTLY ONCE, and ONLY AFTER `self` has been published as the live version
  /// (e.g. a winning [`ArcSwap`](https://docs.rs/arc-swap) compare-and-swap).
  /// Committing produces a tree but does not notify; a committed-then-discarded tree
  /// (a lost CAS) must NOT call this. May over-notify (a descendant change can wake a
  /// key watcher — re-read to confirm); never under-notifies, given non-panicking key
  /// comparisons.
  ///
  /// Firing is **all-or-nothing**: the changed nodes' slots are *collected* by a
  /// fallible diff walk (which runs user `C` comparisons), then fired in a separate
  /// loop. So a panicking `Ord`/`PartialEq` aborts the walk having fired nothing for
  /// this transition — matching the crate's strong-exception style. The fire loop
  /// calls each slot's `Event::notify`, which wakes any registered async waker; the
  /// guarantee assumes those wakers do not panic. A panicking waker (a `Waker`-
  /// contract violation) can abort the loop after a partial fire and is **out of
  /// scope**, exactly as a panicking `Drop` is for the mutators (see the crate docs).
  ///
  /// `self` is the newly published tree and `base` the version the producing
  /// transaction was opened from. See [`publish_to`](Radix::publish_to) to fold the
  /// publish and this notify into one call, and the [module docs](crate::sync#lock-free-sharing)
  /// for the commit → publish → notify discipline.
  #[inline]
  pub fn notify_changes_since(&self, base: &Self) {
    // Collect first (fallible: user comparisons), then fire (infallible). A panic in
    // the collect walk drops `slots`, firing nothing — so a transition either fires
    // every changed slot or none of them, never a partial set that strands watchers.
    let mut slots = Vec::new();
    let fire_empty = self.inner.collect_changes(&base.inner, &mut slots);
    for slot in slots {
      slot.fire();
    }
    if fire_empty {
      base.empty.fire();
    }
  }

  /// Publish-then-notify in one call.
  ///
  /// `swap` attempts to install `self` as the live version and returns `true` iff it
  /// won (became visible). On a win, fires notifications relative to `base` (via
  /// [`notify_changes_since`](Radix::notify_changes_since)); on a loss, fires
  /// nothing. This is the [`watch`](Radix::watch)-safe shape of the commit → publish
  /// → notify discipline: a tree that lost the race never notifies.
  ///
  /// ```no_run
  /// # #[cfg(feature = "watch")] {
  /// use std::sync::Arc;
  /// use arc_swap::ArcSwap;
  /// use iradix::sync::Radix;
  ///
  /// let holder: Arc<ArcSwap<Radix<u8, u32>>> = Arc::new(ArcSwap::from_pointee(Radix::new()));
  /// let base = holder.load_full();
  /// let next = Arc::new({
  ///   let mut t = base.txn();
  ///   t.insert(b"k".as_slice(), 1);
  ///   t.commit()
  /// });
  /// next.publish_to(&base, || {
  ///   let prev = holder.compare_and_swap(&base, Arc::clone(&next));
  ///   Arc::ptr_eq(&base, &prev) // true iff our CAS won
  /// });
  /// # }
  /// ```
  #[inline]
  pub fn publish_to(&self, base: &Self, swap: impl FnOnce() -> bool) {
    if swap() {
      self.notify_changes_since(base);
    }
  }

  /// Returns a [`Watch`] that fires when the value at `key` next changes in a
  /// published version.
  ///
  /// One change channel exists per node, so a change to a *descendant* of `key` also
  /// fires (the watch may wake without `key`'s own value changing); re-read and
  /// re-arm with a fresh `watch` on the new live version. On an empty trie the watch
  /// fires when the first value is inserted. The change must be *published* — a
  /// committed tree fires nothing until its
  /// [`notify_changes_since`](Radix::notify_changes_since) runs after it wins
  /// publication. Prefer [`get_watch`](Radix::get_watch) to read and arm on one snapshot.
  #[inline]
  pub fn watch<K>(&self, key: &K) -> Watch
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    self.watch_at(key)
  }

  /// Returns a [`Watch`] that fires when any key under `prefix` next changes in a
  /// published version (the whole subtree, inclusive of an exact match at `prefix`).
  #[inline]
  pub fn watch_prefix<K>(&self, prefix: &K) -> Watch
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    self.watch_at(prefix)
  }

  /// Read the value at `key` and arm a [`Watch`] for its next change against this one
  /// immutable snapshot, so the value and the watch are consistent. A version
  /// published afterward is caught by the `Watch` — or, if it already replaced this
  /// node, the `Watch` is already-resolved. Prefer this over separate `get` + `watch`.
  /// See [`watch`](Radix::watch) for the reload-and-re-arm loop.
  #[inline]
  pub fn get_watch<K>(&self, key: &K) -> (Option<&V>, Watch)
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    (self.get(key), self.watch_at(key))
  }

  #[inline]
  fn watch_at<K>(&self, key: &K) -> Watch
  where
    K: RadixKey<Component = C> + ?Sized,
  {
    let (listener, already) = match self.inner.watch_slot(key.components()) {
      Some(slot) => slot.listen(),
      None => self.empty.listen(),
    };
    Watch(if already { None } else { Some(listener) })
  }
}

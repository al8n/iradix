# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0]

### Changed

- **BREAKING — `RadixKey` redesign for allocation-free walks.** The trait now
  distinguishes the **borrowed** component yielded while walking a key from the
  **owned** component the trie stores, so a walk (every lookup, ancestor/descendant
  query, and removal) borrows straight from the key and allocates *nothing* — only
  `insert` turns a walked component into its owned form. The old single
  `type Component` plus `fn components(&self) -> impl Iterator<Item: Borrow<Component>>`
  is replaced by:
  - `type Component<'a>: Clone` — the zero-copy borrowed component yielded by the walk;
  - `type Owned: Ord + Clone` — the owned component the trie stores (this is the trie's
    `C`; a `Radix<C, V>` method bound is now `RadixKey<Owned = C>`, was
    `RadixKey<Component = C>`);
  - `type Iter<'a>: Iterator<Item = Component<'a>>` — the nameable walk iterator;
  - `fn components(&self) -> Self::Iter<'_>`;
  - `fn to_owned(c: Self::Component<'_>) -> Self::Owned` — used only on the `insert`
    path (the sole allocation of the key walk);
  - `fn compare(owned: &Self::Owned, walked: Self::Component<'_>) -> Ordering` — the
    descent step, comparing a stored owned component against a walked borrowed one; it
    must agree with `Owned: Ord`.
- **`Path` / `PathBuf` walks are now allocation-free.** Previously `Path`/`PathBuf`
  decomposed to an owned `OsString` per component *on every walk* (including reads);
  now the walk yields borrowed `std::path::Component`s (`Component<'a> =
  std::path::Component<'a>`, `Iter<'a> = std::path::Components<'a>`) and only `insert`
  allocates — one `OsString` (`Owned = OsString`) per **stored** component, never on a
  read. `compare` bridges a stored `OsString` and a walked `Component` through their
  common `OsStr`. `Path::components` normalization semantics are unchanged (a leading
  `.` and any `..` are preserved). A `Path`-keyed trie is still `Radix<OsString, V>`.
- All built-in impls migrated to the new shape, preserving their exact semantics: slice
  keys (`[C]`, `Vec<C>`, `[C; N]`, `Box<[C]>`) yield `&C` / store `C`; `str` / `String`
  yield and store `char`; `CStr` / `CString` / `OsStr` / `OsString` yield `&u8` /
  store `u8`; the integer types yield `u8` by value / store `u8` (same big-endian,
  order-preserving-within-one-type encoding).

## [0.2.2]

### Added

- use `triomphe::Arc` to replace all `std::sync::Arc` usage when `triomphe` feature is enabled
- add descending ordered queries `range_rev` / `seek_reverse_lower_bound` and node-inclusive `descendants_inclusive` (go-immutable-radix parity)
- add key-carrying `(key, value)` walks `walk_prefix` / `walk_prefix_strict` / `walk_path` (each with a `_rev` form) — go's `WalkPrefix` / `WalkPath`
- add `RadixKey` impls for `String`, `Box<[C]>`, `[C; N]`, `CStr` / `CString`, the integer types (`u8`…`u128` / `i8`…`i128`, big-endian order-preserving within one integer type — numeric tries must be homogeneous), byte-keyed `OsStr` / `OsString`, and component-keyed `Path` / `PathBuf`

## [0.2.0]

### Added

- **`triomphe` feature** (off by default) — back the `sync` face with
  [`triomphe`](https://docs.rs/triomphe)'s `Arc` (a more compact atomic refcount with
  no weak count) instead of `std::sync::Arc`, through archery's `ArcTK` pointer kind.
  `sync::Radix` keeps the same public API and `Send + Sync`, so it is a drop-in,
  opt-in swap; `no_std + alloc`.

- **`watch` feature** (off by default) — observe key/prefix changes across
  published versions on the `sync` face. `sync::Radix::watch(key)` /
  `watch_prefix(prefix)` (and `get_watch(key)`, which reads the value and arms a
  watch against one immutable snapshot) return a `Watch` that resolves once a
  change to the watched key — or anything in its subtree — is *published* —
  blocking via `Watch::block_wait()` / `block_wait_timeout()` (need `std`) or async
  via the named `Watch::changed()` future (works on `no_std + alloc`), built on
  `event-listener`. The optional **`future` feature** adds a runtime-agnostic
  async timeout — `Watch::changed_timeout::<R>(d)` (`R` = `TokioRuntime` /
  `SmolRuntime` / `WasmRuntime` / `EmbassyRuntime` / …) resolving `Ok(())` on change
  or `Err(Elapsed)` on timeout — while staying `no_std + alloc`. The feature brings
  the `RuntimeLite` trait, re-exported as `iradix::RuntimeLite`. Convenience `tokio` /
  `smol` features enable that backend and re-export its runtime (`iradix::TokioRuntime`
  / `iradix::SmolRuntime`), so a runtime is nameable from iradix alone; other backends
  are reached through a direct `agnostic-lite` dependency.
  Notification is **at publish, not at commit**, following the
  **commit → publish → notify** discipline: `Txn::commit` builds the next tree but
  fires nothing; after that tree wins publication (e.g. a successful
  `ArcSwap::compare_and_swap`) the winner calls `Radix::notify_changes_since(&base)`
  — or folds the publish and notify into one `Radix::publish_to(&base, swap)`. This
  is sound under lock-free CAS: a tree that loses the race is discarded without ever
  notifying, so a lost CAS cannot strand or falsely wake a watcher. A `Watch` is
  edge-triggered against the snapshot it was armed on and single-use; it **may
  over-notify** (a descendant or merge change can wake a key watcher — re-read to
  confirm) and never under-notifies (given non-panicking key comparisons and async
  wakers — a panic in either during notification is out of scope, like a panicking
  `Drop`), so use it in a reload-and-re-arm loop to track a key across versions. Each node carries its own change channel; the publish-time
  diff fires the events of replaced nodes via a pointer-identity walk that prunes
  pointer-equal subtrees (work scales with the changed paths and their siblings —
  the replaced nodes plus the direct children scanned at each — not the whole tree);
  the mutation path is unchanged and a non-`watch` build is byte-identical. Pulls
  an optional `event-listener` dependency. With `watch`, `Radix::new` is not `const`
  (it allocates the shared empty-position channel). Worked example:
  `examples/watch.rs`.

## [0.1.0]

Initial release: a generic, persistent (copy-on-write) radix trie with structural
sharing — bring-your-own-key, `no_std + alloc`, parameterized over `Rc` / `Arc`.

### Bring-your-own-key

- `RadixKey` trait. `Component` is the owned, `Sized` element the trie stores;
  `components()` yields anything `Borrow<Component>`, so a lookup walks its key
  lazily — allocation-free for these foundational impls (a `[u8]` walk borrows `&u8`
  and copies nothing; a key type's own `components()` may allocate). Foundational impls for `[C]`,
  `Vec<C>`, and `str` (char-addressed). An *unsized* component (e.g.
  `Component = str`) is not supported — use an owned component (`String`, or a
  cheap-clone newtype segment). `components()` must be deterministic (the same
  sequence on every call) — the trie may walk a key more than once per operation; a
  non-deterministic impl is a logic error, like an inconsistent `Ord`/`Hash`.

### Structure

- A persistent copy-on-write radix trie with structural sharing, split into two
  public faces over one shared internal algorithm core (the reference-counting
  pointer is an internal detail, never in the public API):
  - `unsync::Radix<C, V>` — `Rc`-backed, `!Send`; a direct `&mut self` copy-on-write
    handle; `.clone()` is an `O(1)` structurally-shared snapshot.
  - `sync::Radix<C, V>` — `Arc`-backed, `Send + Sync` (auto-derived); an **immutable
    persistent tree** (go-immutable-radix's `Tree`) with cross-thread-shareable
    snapshots. Writes are via a `Txn` (`sync::Txn<C, V>`, an owned working copy —
    open with `txn()`, edit, then `commit()` into the next tree); there are no one-op
    `&self` mutators. Build one in bulk from `(Vec<C>, V)` pairs via `FromIterator`.
- Path-compressed edges held in the parent's `Vec`, sorted by first component and
  located by binary search — no hashing.

### Reads (`V`-bound-free, return `&V`)

- `get`, `contains`, `len`, `is_empty`.
- Prefix queries: `get_ancestor` (inclusive — the longest covered prefix),
  `strict_ancestor`, `has_ancestor`, `ancestors`, `descendants`.

### Ordered queries

- `minimum` / `maximum`: the smallest / largest key (component order) and its value.
- `values` / `values_rev` and `descendants` / `descendants_rev`: forward and
  reverse value iteration in key order.
- `range`: every `(key, value)` whose key lies within any `RangeBounds`, ascending,
  honoring every `Included` / `Excluded` / `Unbounded` combination on both ends.
- `seek_lower_bound`: a forward cursor at the first entry with key `>= key`.

  Ordered queries reconstruct keys as `Vec<C>`; value-only iterators stay
  decompose-only.

### Mutators (`C: Ord + Clone`, `V: Clone`)

The same set on every write face — `&mut self` on `unsync::Radix` and `sync::Txn`
(`sync::Radix` writes go through a `sync::Txn`, never one-op `&self` mutators):

- `insert`, `remove`, `clear`.
- Strict — keep the value at the key: `remove_descendants` (count) /
  `drain_descendants` (values).
- Node-inclusive — remove the key too (go-immutable-radix's `DeletePrefix`):
  `delete_prefix` (count) / `drain_prefix` (values, ascending).

  Removal clones no *removed* value; only retained values on the copied
  copy-on-write path may be cloned, as in every mutator.

  Every mutator traverses the key lazily rather than pre-materializing a whole-key
  `Vec`. The removal mutators — `remove` / `remove_descendants` / `drain_descendants` /
  `delete_prefix` / `drain_prefix` — allocate nothing for the key; `insert` walks
  lazily too but stores the unmatched suffix as an owned edge label (a `Box<[C]>`), as
  any radix insert must. All preserve the no-copy-on-absent guarantee — an absent key
  or prefix triggers no copy-on-write at all.

### Concurrency & `no_std`

- No built-in concurrent holder: `sync::Radix` is an immutable, `O(1)`-clone tree,
  so lock-free sharing is the user's `arc_swap::ArcSwap<sync::Radix<…>>` — readers
  `load()` a wait-free snapshot; a writer opens a `txn()`, `commit()`s the next
  tree, and publishes it with a compare-and-swap retry loop (a worked example ships
  in `examples/`). `unsync::Radix` stays the direct `&mut self` copy-on-write handle.
- `no_std` with independent `std` and `alloc` features (the crate always needs the
  heap; `std` does not imply `alloc`).

### Panic safety

Strong exception guarantee against a panicking user `Clone` / `Ord` / `PartialEq`:
an unwind leaves the trie and `len` consistent and never drops or loses a returned
value. Panicking `Drop` and allocation failure are out of scope — both abort while
unwinding, matching the standard library.

[Unreleased]: https://github.com/al8n/iradix/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/al8n/iradix/releases/tag/v0.1.0

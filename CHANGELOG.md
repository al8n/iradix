# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0]

Initial release: a generic, persistent (copy-on-write) radix trie with structural
sharing — bring-your-own-key, `no_std + alloc`, parameterized over `Rc` / `Arc`.

### Bring-your-own-key

- `RadixKey` trait. `Component` is the owned, `Sized` element the trie stores;
  `components()` yields anything `Borrow<Component>`, so lookups are zero-alloc (a
  `[u8]` walk borrows `&u8` and copies nothing). Foundational impls for `[C]`,
  `Vec<C>`, and `str` (char-addressed). An *unsized* component (e.g.
  `Component = str`) is not supported — use an owned component (`String`, or a
  cheap-clone newtype segment).

### Structure

- A persistent copy-on-write radix trie with structural sharing, split into two
  public faces over one shared internal algorithm core (the reference-counting
  pointer is an internal detail, never in the public API):
  - `unsync::Radix<C, V>` — `Rc`-backed, `!Send`; direct `&mut self` copy-on-write
    mutation; `.clone()` is an `O(1)` structurally-shared snapshot.
  - `sync::Radix<C, V>` — `Arc`-backed, `Send + Sync` (auto-derived); the same API
    with cross-thread-shareable snapshots.
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

- `insert`, `remove`, `clear`.
- Strict — keep the value at the key: `remove_descendants` (count) /
  `drain_descendants` (values).
- Node-inclusive — remove the key too (go-immutable-radix's `DeletePrefix`):
  `delete_prefix` (count) / `drain_prefix` (values, ascending).

  Removal clones no *removed* value; only retained values on the copied
  copy-on-write path may be cloned, as in every mutator.

### Concurrency & `no_std`

- No built-in concurrent holder: `sync::Radix` is an immutable, `O(1)`-clone
  snapshot (like go-immutable-radix's `Tree`) — clone it, mutate the clone, and
  publish it yourself (e.g. wrap it in `arc_swap::ArcSwap<sync::Radix<…>>` or a
  `Mutex` for a shared atomic holder).
- `no_std` with independent `std` and `alloc` features (the crate always needs the
  heap; `std` does not imply `alloc`).

### Panic safety

Strong exception guarantee against a panicking user `Clone` / `Ord` / `PartialEq`:
an unwind leaves the trie and `len` consistent and never drops or loses a returned
value. Panicking `Drop` and allocation failure are out of scope — both abort while
unwinding, matching the standard library.

[Unreleased]: https://github.com/al8n/iradix/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/al8n/iradix/releases/tag/v0.1.0

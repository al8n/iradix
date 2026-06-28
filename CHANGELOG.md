# Changelog

## UNRELEASED

### Added — go-immutable-radix parity

Ordered operations on both `unsync::Radix` and `sync::Radix` (and forwarded
through the concurrent `Txn`). Ordered queries return reconstructed keys as
`Vec<C::Owned>`; value-only iterators stay decompose-only and `V`-bound-free.

- `minimum` / `maximum`: the smallest / largest key (component lexicographic
  order) and its value.
- `delete_prefix`: removes the value at the key **and** every strict descendant
  (node-inclusive; go's `DeletePrefix`), returning the count. Contrast the
  existing strict-only `remove_descendants`, which keeps the value at the key.
- `drain_prefix`: same node-inclusive removal, returning the removed values in
  ascending key order (the value at the key, if any, first).
- `values_rev`: every value in reverse key order (mirror of `values`).
- `descendants_rev`: a key's strict descendants in reverse key order (mirror of
  `descendants`).
- `range`: every `(key, value)` whose key lies within a `RangeBounds`, ascending,
  honoring every `Included` / `Excluded` / `Unbounded` combination on both ends.
- `seek_lower_bound`: a forward cursor at the first entry with key `>= key`, then
  ascending (go's `SeekLowerBound`).

### Fixed

- `descendants` now yields values in ascending key order across all child
  subtrees (previously the largest child subtree came first), matching its
  documented "key order" contract. `drain_descendants` consequently returns
  ascending too.

## 0.1.0

Initial release.

- `RadixKey` bring-your-own-key trait with foundational impls for `[C]`,
  `Vec<C>`, and `str` (char-addressed).
- A persistent copy-on-write radix trie with structural sharing, split into two
  public faces over a shared internal algorithm core (the reference-counting
  pointer is an internal detail, never in the public API):
  - `unsync::Radix<C, V>` (`Rc`, `!Send`): direct `&mut self` copy-on-write
    mutation; `.clone()` is an O(1) structurally-shared snapshot.
  - `sync::Radix<C, V>` (`Arc`, `Send + Sync` auto-derived): the same direct
    `&mut self` API, with cross-thread-shareable snapshots.
- Reads (`V`-bound-free): `get`, `contains`, `get_ancestor` (inclusive),
  `strict_ancestor`, `has_ancestor`, `values`, `ancestors`, `descendants`.
- Mutators: `insert`, `remove`, `remove_descendants` (count + unlink, no value
  clone), `drain_descendants`, `clear`.
- `sync::ConcurrentRadix<C, V>` lock-free shared holder: wait-free snapshot reads
  (`load`), and transactional writes via a private working copy published with a
  compare-and-swap (`txn` / `commit` returning `Conflict` on a lost race, plus a
  `commit_with` retry convenience). `arc-swap` backend on `std`; `spin::RwLock`
  backend on `no_std` + `alloc`.
- `no_std` support with independent `std` and `alloc` features.

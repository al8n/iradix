# Changelog

## UNRELEASED

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

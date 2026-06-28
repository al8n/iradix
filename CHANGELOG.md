# Changelog

## UNRELEASED

## 0.1.0

Initial release.

- `RadixKey` bring-your-own-key trait with foundational impls for `[C]`,
  `Vec<C>`, and `str` (char-addressed).
- `Radix<C, V, P>` persistent copy-on-write radix trie with structural sharing,
  parameterized over `archery::SharedPointerKind`; `LocalRadix` (`Rc`) and
  `SyncRadix` (`Arc`) aliases.
- Reads: `get`, `contains`, `get_ancestor` (inclusive), `strict_ancestor`,
  `has_ancestor`, `values`, `ancestors`, `descendants`.
- Mutators: `insert`, `remove`, `remove_descendants`, `drain_descendants`,
  `clear`, and a `Txn` for batched edits.
- `ConcurrentRadix` holder with wait-free snapshot reads (`arc-swap` on `std`,
  `spin::RwLock` on `no_std`) and serialized single-writer transactions.
- `no_std` support with independent `std` and `alloc` features.

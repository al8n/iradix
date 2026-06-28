<div align="center">
<h1>iradix</h1>
</div>
<div align="center">

A generic, persistent (copy-on-write) radix trie with structural sharing — bring-your-own-key, `no_std + alloc`, parameterized over `Rc`/`Arc`.

[<img alt="github" src="https://img.shields.io/badge/github-al8n/iradix-8da0cb?style=for-the-badge&logo=Github" height="22">][Github-url]
[<img alt="Build" src="https://img.shields.io/github/actions/workflow/status/al8n/iradix/ci.yml?logo=Github-Actions&style=for-the-badge" height="22">][CI-url]
[<img alt="docs.rs" src="https://img.shields.io/badge/docs.rs-iradix-66c2a5?style=for-the-badge&labelColor=555555&logo=docs.rs" height="22">][doc-url]
[<img alt="crates.io" src="https://img.shields.io/crates/v/iradix?style=for-the-badge&logo=rust" height="22">][crates-url]
<img alt="license" src="https://img.shields.io/badge/License-Apache%202.0/MIT-blue.svg?style=for-the-badge&fontColor=white&logoColor=f5c076" height="22">

</div>

`iradix` is a persistent (immutable, copy-on-write) [radix trie] keyed by an
arbitrary sequence of *components*. Each mutation returns a logically new trie;
unchanged subtrees are physically shared between versions, so a [`clone`] and a
snapshot are O(1) and a write copies only the path it touches.

It is the data-structure half of a filesystem-watch stack, but it has no
dependency on any of it: it is a plain, reusable container.

## Highlights

- **Bring-your-own-key.** [`RadixKey`] decomposes a key into components; lookups
  are zero-copy. Foundational impls cover `[C]`, `Vec<C>`, and `str` (which is
  `char`-addressed). A `str` key and a `Vec<char>` key over the same characters
  address the same path.
- **Persistent + structurally shared.** A shared internal `node` core mutates in
  place at refcount 1 and copies only when a node is shared, so old snapshots are
  never disturbed. The reference-counting pointer is an internal detail.
- **Sync / unsync split.** [`unsync::Radix`] uses `Rc` (`!Send`, no atomics);
  [`sync::Radix`] uses `Arc` (`Send + Sync`, auto-derived). Both expose the same
  direct `&mut self` copy-on-write mutation API.
- **Path-compressed, ordered.** Edges live in the parent's `Vec`, sorted by their
  first component and located by binary search — no hashing.
- **Prefix queries.** Inclusive [`get_ancestor`] (longest covered prefix),
  [`strict_ancestor`], `ancestors`, `descendants`, and bulk `remove_descendants`
  / `drain_descendants` (strict) or `delete_prefix` / `drain_prefix`
  (node-inclusive).
- **Ordered queries.** `minimum` / `maximum`, forward and reverse value iteration
  (`values` / `values_rev`, `descendants` / `descendants_rev`), and key-ordered
  `range` (any `RangeBounds`) / `seek_lower_bound` cursors that yield reconstructed
  `(key, value)` pairs.
- **Lock-free concurrent holder.** [`sync::ConcurrentRadix`] wraps a shared trie
  with wait-free snapshot reads and compare-and-swap transactional writes (build
  a private working copy, then `commit` — retry on conflict).
- **`no_std` + `alloc`.** `std` and `alloc` are independent features.

## Installation

```toml
[dependencies]
iradix = "0.1"
```

For `no_std` (heap required):

```toml
[dependencies]
iradix = { version = "0.1", default-features = false, features = ["alloc"] }
```

## Usage

### Insert, look up, and find the longest covering prefix

```rust
use iradix::unsync::Radix;

let mut trie: Radix<u8, &str> = Radix::new();
trie.insert(b"/a".as_slice(), "a");
trie.insert(b"/a/b".as_slice(), "ab");

assert_eq!(trie.get(b"/a/b".as_slice()), Some(&"ab"));
assert!(trie.contains(b"/a".as_slice()));

// get_ancestor is inclusive: an exact match counts as its own ancestor.
assert_eq!(trie.get_ancestor(b"/a/b/c".as_slice()), Some(&"ab"));
assert_eq!(trie.get_ancestor(b"/a/b".as_slice()), Some(&"ab"));

// strict_ancestor excludes the exact key.
assert_eq!(trie.strict_ancestor(b"/a/b".as_slice()), Some(&"a"));
```

### Ordered queries: min/max, ranges, and a lower-bound cursor

```rust
use iradix::unsync::Radix;

let mut trie: Radix<u8, u32> = Radix::new();
for (k, v) in [
    (b"a".as_slice(), 1),
    (b"ab".as_slice(), 2),
    (b"b".as_slice(), 3),
    (b"c".as_slice(), 4),
] {
    trie.insert(k, v);
}

assert_eq!(trie.minimum(), Some((b"a".to_vec(), &1)));
assert_eq!(trie.maximum(), Some((b"c".to_vec(), &4)));

// `range` yields reconstructed (key, value) pairs in ascending key order.
let got: Vec<(Vec<u8>, u32)> = trie
    .range::<[u8], _>((core::ops::Bound::Included(b"ab".as_slice()), core::ops::Bound::Included(b"b".as_slice())))
    .map(|(k, v)| (k, *v))
    .collect();
assert_eq!(got, vec![(b"ab".to_vec(), 2), (b"b".to_vec(), 3)]);

// `seek_lower_bound` is a forward cursor at the first key >= the argument.
let from_b: Vec<u32> = trie.seek_lower_bound(b"b".as_slice()).map(|(_, v)| *v).collect();
assert_eq!(from_b, vec![3, 4]);

// `delete_prefix` removes the key and every descendant (node-inclusive).
assert_eq!(trie.delete_prefix(b"a".as_slice()), 2); // "a" and "ab"
```

### Snapshots are O(1) and isolated

```rust
use iradix::unsync::Radix;

let mut trie: Radix<u8, u32> = Radix::new();
trie.insert(b"k".as_slice(), 1);

let snapshot = trie.clone(); // O(1), no value clones
trie.insert(b"k".as_slice(), 2);

assert_eq!(snapshot.get(b"k".as_slice()), Some(&1)); // unaffected
assert_eq!(trie.get(b"k".as_slice()), Some(&2));
```

### Lock-free concurrent holder with transactional writes

```rust
use iradix::sync::ConcurrentRadix;

let holder: ConcurrentRadix<u8, u32> = ConcurrentRadix::new();

// `commit_with` builds a private working copy and publishes it with a CAS,
// retrying automatically if a concurrent writer wins the race.
holder.commit_with(|txn| {
    txn.insert(b"a".as_slice(), 1);
    txn.insert(b"a/b".as_slice(), 2);
    txn.remove_descendants(b"a".as_slice()); // removes a/b but not a
});

// Readers take a wait-free, point-in-time snapshot.
let snap = holder.load();
assert_eq!(snap.get(b"a".as_slice()), Some(&1));
assert_eq!(snap.get(b"a/b".as_slice()), None);
```

## Cargo features

| feature | default | effect |
|---|---|---|
| `std` | yes | enables the `std` build and the wait-free [`sync::ConcurrentRadix`] backend (`arc-swap`). |
| `alloc` | no | `no_std` + heap; provides the `spin::RwLock` [`sync::ConcurrentRadix`] backend. Independent of `std`. |
| `lockfree-nostd` | no | **reserved** for a future `haphazard` lock-free `no_std` backend; not yet active (it enables `alloc`, and the `spin` backend stays in place). |

`std` and `alloc` are independent (`std` does **not** imply `alloc`). The crate
always needs the heap, so the bare no-feature configuration does not compile —
build with at least `alloc`.

## `#![no_std]`

The crate is `no_std` with `alloc`. Build it with `--no-default-features
--features alloc`.

#### License

<sup>
Licensed under either of <a href="LICENSE-APACHE">Apache License, Version
2.0</a> or <a href="LICENSE-MIT">MIT license</a> at your option.
</sup>

<br>

<sub>
Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this crate by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.
</sub>

[radix trie]: https://en.wikipedia.org/wiki/Radix_tree
[`RadixKey`]: https://docs.rs/iradix/latest/iradix/trait.RadixKey.html
[`unsync::Radix`]: https://docs.rs/iradix/latest/iradix/unsync/struct.Radix.html
[`sync::Radix`]: https://docs.rs/iradix/latest/iradix/sync/struct.Radix.html
[`sync::ConcurrentRadix`]: https://docs.rs/iradix/latest/iradix/sync/struct.ConcurrentRadix.html
[`get_ancestor`]: https://docs.rs/iradix/latest/iradix/unsync/struct.Radix.html#method.get_ancestor
[`strict_ancestor`]: https://docs.rs/iradix/latest/iradix/unsync/struct.Radix.html#method.strict_ancestor
[`clone`]: https://docs.rs/iradix/latest/iradix/unsync/struct.Radix.html#impl-Clone-for-Radix
[Github-url]: https://github.com/al8n/iradix
[CI-url]: https://github.com/al8n/iradix/actions/workflows/ci.yml
[doc-url]: https://docs.rs/iradix
[crates-url]: https://crates.io/crates/iradix

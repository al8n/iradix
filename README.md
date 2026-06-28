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
- **Persistent + structurally shared.** Built on
  [`archery::SharedPointer`]: `make_mut` mutates in place at refcount 1 and
  copies only when a node is shared, so old snapshots are never disturbed.
- **Pick your pointer.** `Radix<C, V, P>` is generic over
  [`archery::SharedPointerKind`]; [`LocalRadix`] uses `Rc` (`!Send`, no atomics)
  and [`SyncRadix`] uses `Arc` (`Send + Sync`). `Send`/`Sync` are fully
  auto-derived.
- **Path-compressed, ordered.** Edges live in the parent's `Vec`, sorted by their
  first component and located by binary search — no hashing.
- **Prefix queries.** Inclusive [`get_ancestor`] (longest covered prefix),
  [`strict_ancestor`], [`ancestors`], [`descendants`], and bulk
  [`remove_descendants`] / [`drain_descendants`].
- **Atomic multi-step writes.** A [`Txn`] batches several edits into one root
  publish, and [`ConcurrentRadix`] wraps a shared trie with lock-free snapshot
  reads and serialized single-writer transactions.
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
use iradix::LocalRadix;

let mut trie: LocalRadix<u8, &str> = LocalRadix::new();
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

### Snapshots are O(1) and isolated

```rust
use iradix::LocalRadix;

let mut trie: LocalRadix<u8, u32> = LocalRadix::new();
trie.insert(b"k".as_slice(), 1);

let snapshot = trie.clone(); // O(1), no value clones
trie.insert(b"k".as_slice(), 2);

assert_eq!(snapshot.get(b"k".as_slice()), Some(&1)); // unaffected
assert_eq!(trie.get(b"k".as_slice()), Some(&2));
```

### Atomic multi-step write

```rust
use iradix::SyncRadix;

let mut trie: SyncRadix<u8, u32> = SyncRadix::new();
let mut txn = trie.txn();
txn.insert(b"a".as_slice(), 1);
txn.insert(b"a/b".as_slice(), 2);
txn.remove_descendants(b"a".as_slice()); // removes a/b but not a
txn.commit(); // one publish

assert_eq!(trie.get(b"a".as_slice()), Some(&1));
assert_eq!(trie.get(b"a/b".as_slice()), None);
```

## Cargo features

| feature | default | effect |
|---|---|---|
| `std` | yes | enables the `std` build and the wait-free [`ConcurrentRadix`] backend (`arc-swap`). |
| `alloc` | no | `no_std` + heap; provides the `spin::RwLock` [`ConcurrentRadix`] backend. Independent of `std`. |
| `lockfree-nostd` | no | **reserved** for a future `haphazard` lock-free `no_std` backend; not yet active (the `spin` backend stays in place). |

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
[`Radix`]: https://docs.rs/iradix/latest/iradix/struct.Radix.html
[`LocalRadix`]: https://docs.rs/iradix/latest/iradix/type.LocalRadix.html
[`SyncRadix`]: https://docs.rs/iradix/latest/iradix/type.SyncRadix.html
[`Txn`]: https://docs.rs/iradix/latest/iradix/struct.Txn.html
[`ConcurrentRadix`]: https://docs.rs/iradix/latest/iradix/struct.ConcurrentRadix.html
[`get_ancestor`]: https://docs.rs/iradix/latest/iradix/struct.Radix.html#method.get_ancestor
[`strict_ancestor`]: https://docs.rs/iradix/latest/iradix/struct.Radix.html#method.strict_ancestor
[`ancestors`]: https://docs.rs/iradix/latest/iradix/struct.Radix.html#method.ancestors
[`descendants`]: https://docs.rs/iradix/latest/iradix/struct.Radix.html#method.descendants
[`remove_descendants`]: https://docs.rs/iradix/latest/iradix/struct.Radix.html#method.remove_descendants
[`drain_descendants`]: https://docs.rs/iradix/latest/iradix/struct.Radix.html#method.drain_descendants
[`clone`]: https://docs.rs/iradix/latest/iradix/struct.Radix.html#impl-Clone-for-Radix
[`archery::SharedPointer`]: https://docs.rs/archery/latest/archery/shared_pointer/struct.SharedPointer.html
[`archery::SharedPointerKind`]: https://docs.rs/archery/latest/archery/shared_pointer/kind/trait.SharedPointerKind.html
[Github-url]: https://github.com/al8n/iradix
[CI-url]: https://github.com/al8n/iradix/actions/workflows/ci.yml
[doc-url]: https://docs.rs/iradix
[crates-url]: https://crates.io/crates/iradix

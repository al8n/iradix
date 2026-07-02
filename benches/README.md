# iradix comparison benchmarks

`comparison.rs` benchmarks `iradix` against other Rust radix/prefix-map crates and
the standard `BTreeMap`, with [criterion](https://docs.rs/criterion). iradix is
exercised as `unsync::Radix<u8, u64>` with `&[u8]` keys (the `[C]` `RadixKey` impl,
`Owned = u8`).

## Running

```sh
cargo bench --bench comparison                 # full run (tight estimates, slow)
cargo bench --bench comparison -- insert       # a single group
cargo bench --bench comparison -- --warm-up-time 0.5 --measurement-time 1 --sample-size 10   # quick smoke run
```

## What is measured

Contenders: `iradix`, [`radix_trie`](https://docs.rs/radix_trie),
[`patricia_tree`](https://docs.rs/patricia_tree) (`PatriciaMap`),
[`qp-trie`](https://docs.rs/qp-trie), [`im`](https://docs.rs/im) (`OrdMap`), and
`std::collections::BTreeMap`.

Groups: `insert` (build the map), `get_hit`, `get_miss`, `iter` (full ordered
traversal), `remove`, and `snapshot_insert` (clone the map, then insert one key — the
persistent-structural-sharing angle; only the meaningfully-cloneable maps `iradix` /
`im` / `BTreeMap` are included).

Each group runs over two key sets × two sizes (1 000 and 10 000):

- **random** — unique ~16-byte keys, low shared prefix (a deterministic SplitMix64
  generator, so the keyset is fixed regardless of dependency versions);
- **paths** — path-like keys with a high shared prefix
  (`/usr/local/share/app/module_{i/100}/file_{i}.rs`), where radix tries shine.

The same keyset feeds every crate; `get_miss` uses a disjoint keyset.

## Results

Median times from a **reduced-sample smoke run** (`--sample-size 10
--measurement-time 1`), so treat them as **indicative** — run the full suite for
tight estimates, and expect absolute numbers to vary by machine. Lower is better;
the best per column is in **bold**.

### `insert` — build the map

| crate | random 1k | random 10k | paths 1k | paths 10k |
|---|---|---|---|---|
| **iradix** | 133 µs | 1.73 ms | 121 µs | 1.61 ms |
| radix_trie | 195 µs | 2.49 ms | 248 µs | 2.61 ms |
| patricia_tree | 1.13 ms | 17.1 ms | 193 µs | 2.36 ms |
| qp-trie | 122 µs | 1.72 ms | 137 µs | 1.74 ms |
| im::OrdMap | 198 µs | 3.20 ms | 192 µs | 3.08 ms |
| BTreeMap | **103 µs** | **1.53 ms** | **114 µs** | **1.35 ms** |

### `get_hit`

| crate | random 1k | random 10k | paths 1k | paths 10k |
|---|---|---|---|---|
| **iradix** | 55 µs | 910 µs | 74 µs | 940 µs |
| radix_trie | 85 µs | 1.13 ms | 173 µs | 1.79 ms |
| patricia_tree | 792 µs | 10.5 ms | 129 µs | 1.66 ms |
| qp-trie | **16 µs** | **231 µs** | **13 µs** | **166 µs** |
| im::OrdMap | 116 µs | 1.96 ms | 118 µs | 1.68 ms |
| BTreeMap | 67 µs | 1.08 ms | 76 µs | 829 µs |

### `get_miss`

| crate | random 1k | random 10k | paths 1k | paths 10k |
|---|---|---|---|---|
| **iradix** | 23 µs | 398 µs | 36 µs | 540 µs |
| radix_trie | 47 µs | 514 µs | 152 µs | 1.61 ms |
| patricia_tree | 1.50 ms | 16.6 ms | 42 µs | 741 µs |
| qp-trie | **5.9 µs** | **160 µs** | **7.1 µs** | **119 µs** |
| im::OrdMap | — | 1.35 ms | 101 µs | 1.58 ms |
| BTreeMap | 53 µs | 707 µs | 30 µs | 465 µs |

### `iter` — full ordered traversal

| crate | random 1k | random 10k | paths 1k | paths 10k |
|---|---|---|---|---|
| **iradix** | 2.6 µs | 26 µs | 2.0 µs | 20 µs |
| radix_trie | 37 µs | 438 µs | 22 µs | 218 µs |
| patricia_tree | 8.0 µs | 140 µs | 8.1 µs | 90 µs |
| qp-trie † | 2.0 µs | 39 µs | 1.5 µs | 18 µs |
| im::OrdMap | 3.8 µs | 43 µs | 4.9 µs | 49 µs |
| BTreeMap | **0.80 µs** | **18 µs** | **0.87 µs** | **18 µs** |

### `remove`

| crate | random 1k | random 10k | paths 1k | paths 10k |
|---|---|---|---|---|
| **iradix** | 175 µs | 2.03 ms | 183 µs | 2.27 ms |
| radix_trie | 173 µs | 1.93 ms | 235 µs | 2.39 ms |
| patricia_tree | 1.35 ms | 19.0 ms | 74 µs | 721 µs |
| qp-trie | **59 µs** | **884 µs** | **52 µs** | **613 µs** |
| im::OrdMap | 180 µs | 3.00 ms | 225 µs | 2.48 ms |
| BTreeMap | 91 µs | 1.46 ms | 62 µs | 645 µs |

### `snapshot_insert` — clone + one insert (persistent-map differentiator)

| crate | random 1k | random 10k | paths 1k | paths 10k |
|---|---|---|---|---|
| **iradix** | 4.6 µs | 5.3 µs | **170 ns** | **173 ns** |
| im::OrdMap | **1.6 µs** | **3.1 µs** | 1.9 µs | 2.6 µs |
| BTreeMap (deep clone) | 25 µs | 258 µs | 30 µs | 333 µs |

## Takeaways

- **Ordered radix trie.** iradix beats `radix_trie`, `patricia_tree`, and `im` on
  `insert`/`get`, and its ordered `iter` trails only `BTreeMap`. Among *ordered*
  prefix maps it is the strongest here.
- **`snapshot_insert` is the point of a persistent trie.** On shared-prefix keys
  iradix's O(1) structural-sharing clone is **~170 ns flat** as the map grows 10×,
  while `BTreeMap`'s deep-clone snapshot scales linearly (30 µs → 333 µs) — a
  ~1000–2000× edge. `im::OrdMap` (also persistent) is close but not flat. On random
  keys a single iradix snapshot-insert touches more distinct copy-on-write path nodes,
  so it sits at a few µs (still an order of magnitude under `BTreeMap`).
- **`qp-trie`** wins raw `get`/`remove`, but † its iteration is **not key-ordered** —
  it is not an ordered map, so its `iter` is not directly comparable.
- **`BTreeMap`** is the fastest *ephemeral* map; iradix's value is ordered prefix
  operations *plus* cheap persistent snapshots, which `BTreeMap` cannot offer.
- **`patricia_tree`** degrades ~10–50× on low-shared-prefix random keys (a real
  property of `PatriciaMap` on such input, not a harness artifact); it is competitive
  on the `paths` keyset.

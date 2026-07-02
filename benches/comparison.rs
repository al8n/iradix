//! Criterion comparison of `iradix` against other Rust radix / map crates.
//!
//! The trie under test is `iradix::unsync::Radix<u8, u64>` keyed by `&[u8]`
//! byte strings (the natural fit for the byte-string radix crates it is compared
//! against). Every group runs over two key distributions — `random` (low shared
//! prefix) and `paths` (high shared prefix, where radix tries shine) — at sizes
//! 1_000 and 10_000, and every crate sees the *same* generated key set at a given
//! (dataset, size) so the comparison is fair.
//!
//! Comparison targets:
//! - `iradix`         — this crate (`unsync::Radix<u8, u64>`).
//! - `radix_trie`     — classic Rust radix trie (`Trie<Vec<u8>, u64>`).
//! - `patricia_tree`  — `PatriciaMap<u64>`.
//! - `qp_trie`        — QP-trie (`Trie<Vec<u8>, u64>`).
//! - `im`             — persistent `OrdMap<Vec<u8>, u64>` (for the snapshot group).
//! - `BTreeMap`       — `std` baseline (`BTreeMap<Vec<u8>, u64>`).

use std::collections::BTreeMap;

use criterion::{BenchmarkId, Criterion};
use std::hint::black_box;

use im::OrdMap;
use patricia_tree::PatriciaMap;
use qp_trie::Trie as QpTrie;
use radix_trie::{Trie as RadixTrie, TrieCommon};

use iradix::unsync::Radix;

/// The two key sizes every group is parameterized over.
const SIZES: [usize; 2] = [1_000, 10_000];

// ---------------------------------------------------------------------------
// Deterministic key generation (inline SplitMix64 — no external RNG crate, so
// the datasets are reproducible regardless of any dependency's version).
// ---------------------------------------------------------------------------

/// A tiny deterministic PRNG (SplitMix64). Seeded once per key set so the same
/// (dataset, size) yields byte-identical keys across every crate benchmarked.
struct SplitMix64 {
  state: u64,
}

impl SplitMix64 {
  #[inline]
  fn new(seed: u64) -> Self {
    Self { state: seed }
  }

  #[inline]
  fn next_u64(&mut self) -> u64 {
    self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = self.state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
  }
}

/// `n` random ~16-byte keys with low shared prefix. Deduplicated so every key is
/// unique (a collision would make the maps disagree on their final length).
fn random_keys(n: usize) -> Vec<Vec<u8>> {
  let mut rng = SplitMix64::new(0x1234_5678_9ABC_DEF0);
  let mut keys = Vec::with_capacity(n);
  let mut seen = std::collections::HashSet::with_capacity(n);
  while keys.len() < n {
    let mut key = Vec::with_capacity(16);
    // 16 bytes = two u64 draws.
    key.extend_from_slice(&rng.next_u64().to_le_bytes());
    key.extend_from_slice(&rng.next_u64().to_le_bytes());
    if seen.insert(key.clone()) {
      keys.push(key);
    }
  }
  keys
}

/// `n` path-like keys with high shared prefix — the shape radix tries exploit.
fn path_keys(n: usize) -> Vec<Vec<u8>> {
  (0..n)
    .map(|i| format!("/usr/local/share/app/module_{}/file_{}.rs", i / 100, i).into_bytes())
    .collect()
}

/// The absent keys for the `get_miss` group: structurally like the present keys
/// of the same dataset, but guaranteed disjoint from them (a distinct tag byte
/// prefix for `random`, and a `_miss` filename suffix / out-of-range index for
/// `paths`). Same count as the present set.
fn miss_keys(dataset: &str, n: usize, present: &[Vec<u8>]) -> Vec<Vec<u8>> {
  let keys: Vec<Vec<u8>> = match dataset {
    "random" => {
      let mut rng = SplitMix64::new(0xDEAD_BEEF_CAFE_F00D);
      (0..n)
        .map(|_| {
          let mut key = Vec::with_capacity(17);
          // A leading tag byte no present `random` key has (they are all 16
          // bytes with no fixed prefix), plus fresh random bytes.
          key.push(0xFF);
          key.extend_from_slice(&rng.next_u64().to_le_bytes());
          key.extend_from_slice(&rng.next_u64().to_le_bytes());
          key
        })
        .collect()
    }
    "paths" => (0..n)
      .map(|i| {
        // Distinct filename suffix + an index shifted out of the present range,
        // so no miss key coincides with a present one.
        format!(
          "/usr/local/share/app/module_{}/file_{}_miss.rs",
          i / 100,
          i + n
        )
        .into_bytes()
      })
      .collect(),
    other => panic!("unknown dataset: {other}"),
  };

  // Defensive: assert disjointness so a silent overlap can't quietly turn a
  // `get_miss` into a partial `get_hit`.
  let present_set: std::collections::HashSet<&Vec<u8>> = present.iter().collect();
  debug_assert!(
    keys.iter().all(|k| !present_set.contains(k)),
    "miss keys must be disjoint from present keys"
  );
  keys
}

/// Builds the `(key, value)` pairs for a dataset at a size. Value is the index
/// as `u64`.
fn dataset_keys(dataset: &str, n: usize) -> Vec<Vec<u8>> {
  match dataset {
    "random" => random_keys(n),
    "paths" => path_keys(n),
    other => panic!("unknown dataset: {other}"),
  }
}

// ---------------------------------------------------------------------------
// Per-crate map builders (fresh, fully-populated map from a borrowed key set).
// ---------------------------------------------------------------------------

fn build_iradix(keys: &[Vec<u8>]) -> Radix<u8, u64> {
  let mut map = Radix::new();
  for (i, k) in keys.iter().enumerate() {
    map.insert(k.as_slice(), i as u64);
  }
  map
}

fn build_radix_trie(keys: &[Vec<u8>]) -> RadixTrie<Vec<u8>, u64> {
  let mut map = RadixTrie::new();
  for (i, k) in keys.iter().enumerate() {
    map.insert(k.clone(), i as u64);
  }
  map
}

fn build_patricia(keys: &[Vec<u8>]) -> PatriciaMap<u64> {
  let mut map = PatriciaMap::new();
  for (i, k) in keys.iter().enumerate() {
    map.insert(k.as_slice(), i as u64);
  }
  map
}

fn build_qp(keys: &[Vec<u8>]) -> QpTrie<Vec<u8>, u64> {
  let mut map = QpTrie::new();
  for (i, k) in keys.iter().enumerate() {
    map.insert(k.clone(), i as u64);
  }
  map
}

fn build_im(keys: &[Vec<u8>]) -> OrdMap<Vec<u8>, u64> {
  let mut map = OrdMap::new();
  for (i, k) in keys.iter().enumerate() {
    map.insert(k.clone(), i as u64);
  }
  map
}

fn build_btree(keys: &[Vec<u8>]) -> BTreeMap<Vec<u8>, u64> {
  let mut map = BTreeMap::new();
  for (i, k) in keys.iter().enumerate() {
    map.insert(k.clone(), i as u64);
  }
  map
}

// ---------------------------------------------------------------------------
// Benchmark groups.
// ---------------------------------------------------------------------------

/// `insert` — build each map from scratch from the N `(key, value)` pairs.
fn bench_insert(c: &mut Criterion) {
  let mut group = c.benchmark_group("insert");
  for dataset in ["random", "paths"] {
    for &size in &SIZES {
      let keys = dataset_keys(dataset, size);
      let id = |name| BenchmarkId::new(name, format!("{dataset}/{size}"));

      group.bench_with_input(id("iradix"), &keys, |b, keys| {
        b.iter(|| black_box(build_iradix(black_box(keys))));
      });
      group.bench_with_input(id("radix_trie"), &keys, |b, keys| {
        b.iter(|| black_box(build_radix_trie(black_box(keys))));
      });
      group.bench_with_input(id("patricia_tree"), &keys, |b, keys| {
        b.iter(|| black_box(build_patricia(black_box(keys))));
      });
      group.bench_with_input(id("qp_trie"), &keys, |b, keys| {
        b.iter(|| black_box(build_qp(black_box(keys))));
      });
      group.bench_with_input(id("im"), &keys, |b, keys| {
        b.iter(|| black_box(build_im(black_box(keys))));
      });
      group.bench_with_input(id("btreemap"), &keys, |b, keys| {
        b.iter(|| black_box(build_btree(black_box(keys))));
      });
    }
  }
  group.finish();
}

/// `get_hit` — look up all N present keys (each must be found).
fn bench_get_hit(c: &mut Criterion) {
  let mut group = c.benchmark_group("get_hit");
  for dataset in ["random", "paths"] {
    for &size in &SIZES {
      let keys = dataset_keys(dataset, size);
      let id = |name| BenchmarkId::new(name, format!("{dataset}/{size}"));

      let iradix = build_iradix(&keys);
      group.bench_with_input(id("iradix"), &keys, |b, keys| {
        b.iter(|| {
          let mut sum = 0u64;
          for k in keys {
            sum = sum.wrapping_add(*black_box(iradix.get(k.as_slice())).unwrap());
          }
          black_box(sum)
        });
      });

      let radix_trie = build_radix_trie(&keys);
      group.bench_with_input(id("radix_trie"), &keys, |b, keys| {
        b.iter(|| {
          let mut sum = 0u64;
          for k in keys {
            sum = sum.wrapping_add(*black_box(radix_trie.get(k.as_slice())).unwrap());
          }
          black_box(sum)
        });
      });

      let patricia = build_patricia(&keys);
      group.bench_with_input(id("patricia_tree"), &keys, |b, keys| {
        b.iter(|| {
          let mut sum = 0u64;
          for k in keys {
            sum = sum.wrapping_add(*black_box(patricia.get(k.as_slice())).unwrap());
          }
          black_box(sum)
        });
      });

      let qp = build_qp(&keys);
      group.bench_with_input(id("qp_trie"), &keys, |b, keys| {
        b.iter(|| {
          let mut sum = 0u64;
          for k in keys {
            sum = sum.wrapping_add(*black_box(qp.get(k.as_slice())).unwrap());
          }
          black_box(sum)
        });
      });

      let im = build_im(&keys);
      group.bench_with_input(id("im"), &keys, |b, keys| {
        b.iter(|| {
          let mut sum = 0u64;
          for k in keys {
            sum = sum.wrapping_add(*black_box(im.get(k.as_slice())).unwrap());
          }
          black_box(sum)
        });
      });

      let btree = build_btree(&keys);
      group.bench_with_input(id("btreemap"), &keys, |b, keys| {
        b.iter(|| {
          let mut sum = 0u64;
          for k in keys {
            sum = sum.wrapping_add(*black_box(btree.get(k.as_slice())).unwrap());
          }
          black_box(sum)
        });
      });
    }
  }
  group.finish();
}

/// `get_miss` — look up N absent keys (each must be `None`).
fn bench_get_miss(c: &mut Criterion) {
  let mut group = c.benchmark_group("get_miss");
  for dataset in ["random", "paths"] {
    for &size in &SIZES {
      let keys = dataset_keys(dataset, size);
      let misses = miss_keys(dataset, size, &keys);
      let id = |name| BenchmarkId::new(name, format!("{dataset}/{size}"));

      let iradix = build_iradix(&keys);
      group.bench_with_input(id("iradix"), &misses, |b, misses| {
        b.iter(|| {
          let mut hits = 0usize;
          for k in misses {
            if black_box(iradix.get(k.as_slice())).is_some() {
              hits += 1;
            }
          }
          black_box(hits)
        });
      });

      let radix_trie = build_radix_trie(&keys);
      group.bench_with_input(id("radix_trie"), &misses, |b, misses| {
        b.iter(|| {
          let mut hits = 0usize;
          for k in misses {
            if black_box(radix_trie.get(k.as_slice())).is_some() {
              hits += 1;
            }
          }
          black_box(hits)
        });
      });

      let patricia = build_patricia(&keys);
      group.bench_with_input(id("patricia_tree"), &misses, |b, misses| {
        b.iter(|| {
          let mut hits = 0usize;
          for k in misses {
            if black_box(patricia.get(k.as_slice())).is_some() {
              hits += 1;
            }
          }
          black_box(hits)
        });
      });

      let qp = build_qp(&keys);
      group.bench_with_input(id("qp_trie"), &misses, |b, misses| {
        b.iter(|| {
          let mut hits = 0usize;
          for k in misses {
            if black_box(qp.get(k.as_slice())).is_some() {
              hits += 1;
            }
          }
          black_box(hits)
        });
      });

      let im = build_im(&keys);
      group.bench_with_input(id("im"), &misses, |b, misses| {
        b.iter(|| {
          let mut hits = 0usize;
          for k in misses {
            if black_box(im.get(k.as_slice())).is_some() {
              hits += 1;
            }
          }
          black_box(hits)
        });
      });

      let btree = build_btree(&keys);
      group.bench_with_input(id("btreemap"), &misses, |b, misses| {
        b.iter(|| {
          let mut hits = 0usize;
          for k in misses {
            if black_box(btree.get(k.as_slice())).is_some() {
              hits += 1;
            }
          }
          black_box(hits)
        });
      });
    }
  }
  group.finish();
}

/// `iter` — full ordered traversal, summing the values.
fn bench_iter(c: &mut Criterion) {
  let mut group = c.benchmark_group("iter");
  for dataset in ["random", "paths"] {
    for &size in &SIZES {
      let keys = dataset_keys(dataset, size);
      let id = |name| BenchmarkId::new(name, format!("{dataset}/{size}"));

      let iradix = build_iradix(&keys);
      group.bench_function(id("iradix"), |b| {
        b.iter(|| black_box(iradix.values().sum::<u64>()));
      });

      let radix_trie = build_radix_trie(&keys);
      group.bench_function(id("radix_trie"), |b| {
        b.iter(|| black_box(radix_trie.values().sum::<u64>()));
      });

      let patricia = build_patricia(&keys);
      group.bench_function(id("patricia_tree"), |b| {
        b.iter(|| black_box(patricia.values().sum::<u64>()));
      });

      let qp = build_qp(&keys);
      group.bench_function(id("qp_trie"), |b| {
        b.iter(|| black_box(qp.values().sum::<u64>()));
      });

      let im = build_im(&keys);
      group.bench_function(id("im"), |b| {
        b.iter(|| black_box(im.values().sum::<u64>()));
      });

      let btree = build_btree(&keys);
      group.bench_function(id("btreemap"), |b| {
        b.iter(|| black_box(btree.values().sum::<u64>()));
      });
    }
  }
  group.finish();
}

/// `remove` — remove all N keys from a pre-built map. Each iteration gets a fresh
/// clone of the populated map via `iter_batched`, so the timed work is the
/// removals, not the rebuild.
fn bench_remove(c: &mut Criterion) {
  use criterion::BatchSize;

  let mut group = c.benchmark_group("remove");
  for dataset in ["random", "paths"] {
    for &size in &SIZES {
      let keys = dataset_keys(dataset, size);
      let id = |name| BenchmarkId::new(name, format!("{dataset}/{size}"));

      // iradix clone is O(1) structural sharing; the first mutation on the clone
      // triggers copy-on-write, so this measures true remove cost on a fresh view.
      let iradix = build_iradix(&keys);
      group.bench_with_input(id("iradix"), &keys, |b, keys| {
        b.iter_batched(
          || iradix.clone(),
          |mut map| {
            for k in keys {
              black_box(map.remove(k.as_slice()));
            }
            black_box(map)
          },
          BatchSize::SmallInput,
        );
      });

      group.bench_with_input(id("radix_trie"), &keys, |b, keys| {
        b.iter_batched(
          || build_radix_trie(keys),
          |mut map| {
            for k in keys {
              black_box(map.remove(k.as_slice()));
            }
            black_box(map)
          },
          BatchSize::SmallInput,
        );
      });

      group.bench_with_input(id("patricia_tree"), &keys, |b, keys| {
        b.iter_batched(
          || build_patricia(keys),
          |mut map| {
            for k in keys {
              black_box(map.remove(k.as_slice()));
            }
            black_box(map)
          },
          BatchSize::SmallInput,
        );
      });

      group.bench_with_input(id("qp_trie"), &keys, |b, keys| {
        b.iter_batched(
          || build_qp(keys),
          |mut map| {
            for k in keys {
              black_box(map.remove(k.as_slice()));
            }
            black_box(map)
          },
          BatchSize::SmallInput,
        );
      });

      // im OrdMap clone is also O(1) structural sharing.
      let im = build_im(&keys);
      group.bench_with_input(id("im"), &keys, |b, keys| {
        b.iter_batched(
          || im.clone(),
          |mut map| {
            for k in keys {
              black_box(map.remove(k.as_slice()));
            }
            black_box(map)
          },
          BatchSize::SmallInput,
        );
      });

      group.bench_with_input(id("btreemap"), &keys, |b, keys| {
        b.iter_batched(
          || build_btree(keys),
          |mut map| {
            for k in keys {
              black_box(map.remove(k.as_slice()));
            }
            black_box(map)
          },
          BatchSize::SmallInput,
        );
      });
    }
  }
  group.finish();
}

/// `snapshot_insert` — the persistent-structural-sharing angle. Clone the
/// populated map, then insert one new key, measuring clone + insert together.
///
/// Only the crates with a meaningful clone are included: `iradix` (O(1) clone,
/// then copy-on-write on the single insert), `im::OrdMap` (also O(1) persistent
/// clone), and `BTreeMap` (a full deep clone — the cost a non-persistent map
/// pays to take a snapshot). The non-persistent radix crates (`radix_trie`,
/// `patricia_tree`, `qp_trie`) would likewise deep-clone the whole trie here, so
/// they are omitted; `BTreeMap` stands in for the deep-clone baseline.
fn bench_snapshot_insert(c: &mut Criterion) {
  let mut group = c.benchmark_group("snapshot_insert");
  for dataset in ["random", "paths"] {
    for &size in &SIZES {
      let keys = dataset_keys(dataset, size);
      let id = |name| BenchmarkId::new(name, format!("{dataset}/{size}"));

      // A fresh key not present in either dataset, to force a real insert.
      let new_key: Vec<u8> = b"/snapshot/insert/probe/key".to_vec();

      let iradix = build_iradix(&keys);
      group.bench_function(id("iradix"), |b| {
        b.iter(|| {
          let mut snap = black_box(&iradix).clone();
          snap.insert(black_box(new_key.as_slice()), black_box(42u64));
          black_box(snap)
        });
      });

      let im = build_im(&keys);
      group.bench_function(id("im"), |b| {
        b.iter(|| {
          let mut snap = black_box(&im).clone();
          snap.insert(black_box(new_key.clone()), black_box(42u64));
          black_box(snap)
        });
      });

      let btree = build_btree(&keys);
      group.bench_function(id("btreemap"), |b| {
        b.iter(|| {
          let mut snap = black_box(&btree).clone();
          snap.insert(black_box(new_key.clone()), black_box(42u64));
          black_box(snap)
        });
      });
    }
  }
  group.finish();
}

criterion::criterion_group!(
  benches,
  bench_insert,
  bench_get_hit,
  bench_get_miss,
  bench_iter,
  bench_remove,
  bench_snapshot_insert,
);

// `cargo miri test --all-targets` runs this harness-less bench binary too, and
// Criterion's startup probes gnuplot through `Command::output` — a foreign call the
// interpreter cannot perform (and timing under an interpreter would be meaningless).
#[cfg(not(miri))]
criterion::criterion_main!(benches);

#[cfg(miri)]
fn main() {}

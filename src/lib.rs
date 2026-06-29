#![doc = include_str!("../README.md")]
#![doc = r#"
# Panic safety

`iradix` upholds the strong exception guarantee for the recoverable panic a
generic container can actually encounter: a panicking user `Clone`, `Ord`, or
`PartialEq`. If `insert`, `remove`, `remove_descendants`, or `drain_descendants`
unwinds because user code panicked, the trie is left exactly as it was — `len`
stays accurate, every previously stored key still resolves, and no stored value is
dropped or lost (a value-returning call either returns its value or leaves it in
place).

Two failure modes are **out of scope**, both matching the standard library, which
makes no guarantee across either:

- **Panicking `Drop`** — a destructor that panics while unwinding aborts the
  process, so no container can stay sound across one. Removing an edge runs user
  destructors; if one panics, the operation may be left partially committed. Keep
  destructors panic-free.
- **Allocation failure** — like `Vec`/`Box`, `iradix` reaches the global
  allocator through infallible APIs, so out-of-memory invokes the allocation-error
  handler (an abort by default), not an unwind. There is therefore no recoverable
  allocation panic to be safe across; OOM does not corrupt the trie because it
  does not return.
"#]
// Unit tests inherently link `std` (the test harness, `thread_local!`,
// `catch_unwind`); only drop `std` for a non-test no_std build. The no_std code
// paths stay gated on the `std`/`alloc` *features* (independent of `test`), so
// they are still compiled and exercised under `--no-default-features` test runs.
#![cfg_attr(all(not(feature = "std"), not(test)), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(docsrs, allow(unused_attributes))]
#![deny(missing_docs)]

// The `alloc`-as-`std` alias lets genuine-heap `std::` paths compile unchanged
// under no_std+alloc. Suppressed under `test`, where the real `std` is linked.
#[cfg(all(not(feature = "std"), feature = "alloc", not(test)))]
extern crate alloc as std;

#[cfg(any(feature = "std", test))]
extern crate std;

mod key;

// The whole copy-on-write radix algorithm, generic over the pointer kind. It is
// internal: `archery` is confined here and never appears in any public signature.
mod node;

pub mod sync;
pub mod unsync;

pub use key::RadixKey;

/// Re-export of [`agnostic-lite`](https://docs.rs/agnostic-lite)'s `RuntimeLite`
/// trait — the runtime bound for the async
/// [`Watch::changed_timeout`](sync::Watch::changed_timeout). Enabling the `tokio` or
/// `smol` feature additionally re-exports that runtime as `iradix::TokioRuntime` /
/// `iradix::SmolRuntime`, so a concrete runtime is nameable from iradix alone; for the
/// other backends (async-io / wasm / embassy) add `agnostic-lite` directly.
#[cfg(feature = "agnostic-lite")]
#[cfg_attr(docsrs, doc(cfg(feature = "agnostic-lite")))]
pub use agnostic_lite::RuntimeLite;

/// The tokio runtime for [`Watch::changed_timeout`](sync::Watch::changed_timeout),
/// re-exported when the `tokio` feature is enabled.
#[cfg(feature = "tokio")]
#[cfg_attr(docsrs, doc(cfg(feature = "tokio")))]
pub use agnostic_lite::tokio::TokioRuntime;

/// The smol runtime for [`Watch::changed_timeout`](sync::Watch::changed_timeout),
/// re-exported when the `smol` feature is enabled.
#[cfg(feature = "smol")]
#[cfg_attr(docsrs, doc(cfg(feature = "smol")))]
pub use agnostic_lite::smol::SmolRuntime;

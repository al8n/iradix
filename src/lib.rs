#![doc = include_str!("../README.md")]
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
#[cfg(feature = "future")]
#[cfg_attr(docsrs, doc(cfg(feature = "future")))]
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

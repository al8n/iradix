#![doc = include_str!("../README.md")]
#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(docsrs, allow(unused_attributes))]
#![deny(missing_docs)]

#[cfg(all(not(feature = "std"), feature = "alloc"))]
extern crate alloc as std;

#[cfg(feature = "std")]
extern crate std;

mod key;
mod node;
mod radix;

#[cfg(any(feature = "std", feature = "alloc"))]
mod concurrent;

pub use archery::{ArcK, RcK, SharedPointerKind};
pub use key::RadixKey;
pub use radix::{Ancestors, Descendants, LocalRadix, Radix, SyncRadix, Txn, Values};

#[cfg(any(feature = "std", feature = "alloc"))]
pub use concurrent::ConcurrentRadix;

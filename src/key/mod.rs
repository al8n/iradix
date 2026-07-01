use core::{borrow::Borrow, ffi::CStr};

use std::{boxed::Box, string::String, vec::Vec};

/// A key that decomposes into a sequence of [`Component`](RadixKey::Component)s.
///
/// This is the bring-your-own-key seam: a [`Radix`](crate::unsync::Radix) is generic over
/// the *component* type, not over a concrete key type. Implementing `RadixKey`
/// for your own key lets it be looked up and stored: looking up or inserting a key
/// walks its [`components`](RadixKey::components) lazily, and only
/// [`insert`](crate::unsync::Radix::insert) clones each component (it is `Clone`) to
/// store it. See [Allocation](#allocation) for the read-time allocation contract.
///
/// # Allocation
///
/// This is about **producing a key's components** — the
/// [`components`](RadixKey::components) call every lookup, insert, and removal makes
/// to walk the trie — not about an operation's total cost. That decomposition
/// allocates nothing for every built-in key except `Path` / `PathBuf`: a slice key
/// yields `&C` borrows, and by-value components (a `char` from a `str`, an integer's
/// big-endian bytes) are produced on the stack (satisfying the
/// [`Borrow`](core::borrow::Borrow) bound via the blanket `impl Borrow<T> for T`).
/// Only a `Path` / `PathBuf` key allocates during this decomposition — one
/// `OsString` per component.
///
/// Everything else a call does is separate and independent of the key type:
/// [`insert`](crate::unsync::Radix::insert) allocates the trie nodes it stores, and
/// the ordered / iterator reads ([`range`](crate::unsync::Radix::range), `values`,
/// `walk_*`, `minimum`, …) allocate heap traversal state and, where they return keys,
/// `Vec<C>` keys — see those methods.
///
/// # Determinism
///
/// [`components`](RadixKey::components) **must yield the same sequence on every
/// call** for a given key. A single mutating operation may walk the key more than
/// once — for example, `remove` checks existence first so that removing an absent
/// key copies nothing, then walks again to unlink. An implementation whose
/// `components` varies between calls (e.g. via interior mutability) is a logic
/// error: it can read a different key than it writes, producing wrong results.
/// This is not undefined behavior — it is the same class of contract as an
/// inconsistent [`Ord`](core::cmp::Ord) or [`Hash`](core::hash::Hash) for the
/// standard collections. The foundational impls (`[C]`, `Vec<C>`, `str`) satisfy
/// it trivially.
///
/// # Built-in implementations
///
/// Sequence keys decompose to their element `C`: `[C]`, `Vec<C>`, `Box<[C]>`,
/// `[C; N]`. `str` and `String` decompose to `char` (and alias each other). These
/// byte-addressed types decompose to `u8`: `OsStr` / `OsString` / `CStr` / `CString`,
/// and the integer types (`u8`…`u128`, `i8`…`i128`) as big-endian bytes (signed keys
/// flip the sign bit).
///
/// The integer encoding is injective and order-preserving **only within a single
/// integer type** — across types or widths the bytes collide and mis-order (`0i8`
/// and `128u8` both encode `[0x80]`; `256u16` sorts before `255u8`), so a
/// numeric-keyed trie must be **homogeneous** (one integer type).
///
/// `Path` and `PathBuf` decompose to their `OsString` **components** (following
/// [`Path::components`](std::path::Path::components): a *leading* `.` and any `..` are
/// preserved, so `./a` and `a` are different keys), so `a/b` is an ancestor of
/// `a/b/c` but not of `a/bc`. `Path` and `PathBuf` are the only built-ins whose
/// decomposition allocates — one `OsString` per component. The `OsStr` and `Path`
/// families require the `std` feature.
///
/// # Examples
///
/// ```
/// use core::borrow::Borrow;
/// use iradix::RadixKey;
///
/// let key: &[u8] = b"abc";
/// let collected: Vec<u8> = key.components().map(|c| *c.borrow()).collect();
/// assert_eq!(collected, b"abc");
/// ```
pub trait RadixKey {
  /// The owned, `Sized` component the key decomposes into. The trie compares
  /// components by equality ([`Ord`](core::cmp::Ord) on the read paths) and stores
  /// them by value (cloning each on insert).
  type Component;

  /// Returns an iterator over the key's components, in order.
  ///
  /// Each item borrows as a [`Self::Component`]; an implementation may yield
  /// borrowed sub-slices (zero-copy) or owned components by value.
  ///
  /// Must be deterministic — every call yields the same sequence (see the trait's
  /// *Determinism* section); the trie may walk a key more than once per operation.
  fn components(&self) -> impl Iterator<Item: Borrow<Self::Component>> + '_;
}

impl<C> RadixKey for [C] {
  type Component = C;

  #[inline]
  fn components(&self) -> impl Iterator<Item: Borrow<Self::Component>> + '_ {
    self.iter()
  }
}

impl<C> RadixKey for Vec<C> {
  type Component = C;

  #[inline]
  fn components(&self) -> impl Iterator<Item: Borrow<Self::Component>> + '_ {
    self.iter()
  }
}

impl RadixKey for str {
  type Component = char;

  #[inline]
  fn components(&self) -> impl Iterator<Item: Borrow<Self::Component>> + '_ {
    // A `str` is character-addressed, not byte-addressed: each component is a
    // `char`. A `str` key and a `Vec<char>` / `[char]` key over the same
    // characters therefore decompose identically and address the same trie path.
    self.chars()
  }
}

/// `String` is character-addressed, mirroring [`str`] — the two alias (the same
/// characters decompose to the same `char` sequence and address the same path).
impl RadixKey for String {
  type Component = char;

  #[inline]
  fn components(&self) -> impl Iterator<Item: Borrow<Self::Component>> + '_ {
    self.chars()
  }
}

impl<C, const N: usize> RadixKey for [C; N] {
  type Component = C;

  #[inline]
  fn components(&self) -> impl Iterator<Item: Borrow<Self::Component>> + '_ {
    self.iter()
  }
}

impl<C> RadixKey for Box<[C]> {
  type Component = C;

  #[inline]
  fn components(&self) -> impl Iterator<Item: Borrow<Self::Component>> + '_ {
    self.iter()
  }
}

/// A C string keys on its bytes, excluding the terminating nul.
impl RadixKey for CStr {
  type Component = u8;

  #[inline]
  fn components(&self) -> impl Iterator<Item: Borrow<Self::Component>> + '_ {
    self.to_bytes().iter().copied()
  }
}

impl RadixKey for std::ffi::CString {
  type Component = u8;

  #[inline]
  fn components(&self) -> impl Iterator<Item: Borrow<Self::Component>> + '_ {
    self.to_bytes().iter().copied()
  }
}

// Integer keys decompose to their big-endian bytes so that, WITHIN a single integer
// type, the trie's component-lexicographic order matches numeric order and the
// encoding is injective (enabling ordered / range queries over numeric keys). Signed
// integers flip the sign bit first, so `… < -1 < 0 < 1 < …` still holds in unsigned
// byte order. The bytes are NOT comparable ACROSS integer types or widths — `0i8` and
// `128u8` both encode as `[0x80]`, and `256u16` sorts before `255u8` — so a
// numeric-keyed trie must be homogeneous (keyed by one integer type).
macro_rules! impl_radixkey_uint {
  ($($t:ty),+ $(,)?) => {$(
    impl RadixKey for $t {
      type Component = u8;

      #[inline]
      fn components(&self) -> impl Iterator<Item: Borrow<Self::Component>> + '_ {
        self.to_be_bytes().into_iter()
      }
    }
  )+};
}

macro_rules! impl_radixkey_int {
  ($($t:ty => $u:ty),+ $(,)?) => {$(
    impl RadixKey for $t {
      type Component = u8;

      #[inline]
      fn components(&self) -> impl Iterator<Item: Borrow<Self::Component>> + '_ {
        // `(1 as $u).rotate_right(1)` is the sign-bit mask for this width; XOR
        // flips it so negatives sort below non-negatives in big-endian byte order.
        ((*self as $u) ^ (1 as $u).rotate_right(1))
          .to_be_bytes()
          .into_iter()
      }
    }
  )+};
}

impl_radixkey_uint!(u8, u16, u32, u64, u128);
impl_radixkey_int!(i8 => u8, i16 => u16, i32 => u32, i64 => u64, i128 => u128);

/// An OS string keys on its platform-encoded bytes
/// ([`OsStr::as_encoded_bytes`](std::ffi::OsStr::as_encoded_bytes)). This is
/// byte-addressed, **not** path-component-addressed: for a filesystem path whose
/// *components* form the key, use [`Path`](std::path::Path) (which keys on
/// `OsString` segments).
#[cfg(feature = "std")]
#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
impl RadixKey for std::ffi::OsStr {
  type Component = u8;

  #[inline]
  fn components(&self) -> impl Iterator<Item: Borrow<Self::Component>> + '_ {
    self.as_encoded_bytes().iter().copied()
  }
}

#[cfg(feature = "std")]
#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
impl RadixKey for std::ffi::OsString {
  type Component = u8;

  #[inline]
  fn components(&self) -> impl Iterator<Item: Borrow<Self::Component>> + '_ {
    self.as_os_str().as_encoded_bytes().iter().copied()
  }
}

/// A path keys on its **components** — each yielded component is an owned
/// `OsString` (via [`Path::components`](std::path::Path::components), so the
/// decomposition follows that method's normalization: redundant separators and
/// *interior* `.` are dropped, but a **leading** `.` yields a `CurDir` component and
/// `..` a `ParentDir` component, both preserved — so `./a` and `a`, or `a/../b` and
/// `b`, are DIFFERENT keys). Unlike a byte key, `a/b` is an ancestor of `a/b/c` but
/// **not** of `a/bc`. Its `components` allocates one `OsString` per yielded component.
#[cfg(feature = "std")]
#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
impl RadixKey for std::path::Path {
  type Component = std::ffi::OsString;

  #[inline]
  fn components(&self) -> impl Iterator<Item: Borrow<Self::Component>> + '_ {
    std::path::Path::components(self).map(|c| c.as_os_str().to_os_string())
  }
}

/// Keys on its `OsString` path components, exactly like [`Path`](std::path::Path);
/// its `components` allocates one `OsString` per yielded component.
#[cfg(feature = "std")]
#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
impl RadixKey for std::path::PathBuf {
  type Component = std::ffi::OsString;

  #[inline]
  fn components(&self) -> impl Iterator<Item: Borrow<Self::Component>> + '_ {
    std::path::Path::components(self).map(|c| c.as_os_str().to_os_string())
  }
}

#[cfg(test)]
mod tests;

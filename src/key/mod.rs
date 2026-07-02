use core::{cmp::Ordering, ffi::CStr};

use std::{boxed::Box, string::String, vec::Vec};

/// A key that decomposes into a sequence of components.
///
/// This is the bring-your-own-key seam: a [`Radix`](crate::unsync::Radix) is generic over
/// the *owned component* type, not over a concrete key type. Implementing `RadixKey`
/// for your own key lets it be looked up and stored.
///
/// A walk is **zero-copy**: [`components`](RadixKey::components) yields borrowed
/// [`Component<'_>`](RadixKey::Component)s that borrow directly from the key, and the
/// descent compares each against a stored [`Owned`](RadixKey::Owned) component with
/// [`compare`](RadixKey::compare). Only [`insert`](crate::unsync::Radix::insert) turns a
/// walked component into its owned form via [`to_owned`](RadixKey::to_owned) — the sole
/// allocation on the whole key path. See [Allocation](#allocation).
///
/// # Consistency
///
/// [`Owned`](RadixKey::Owned)'s [`Ord`](core::cmp::Ord) orders stored components against
/// each other (the trie keeps children sorted by their owned first component);
/// [`compare`](RadixKey::compare) orders a stored owned component against a walked
/// borrowed one (the descent step). The two **must agree**: for any component `c`,
/// `Self::compare(&Self::to_owned(c.clone()), c)` must be [`Equal`](Ordering::Equal), and
/// `compare` must impose the same total order on owned components as `Owned: Ord`. An
/// inconsistent pair reads a different key than it writes, exactly like an inconsistent
/// [`Ord`](core::cmp::Ord) / [`Hash`](core::hash::Hash) for the standard collections.
///
/// # Allocation
///
/// This is about **walking a key's components** — the
/// [`components`](RadixKey::components) call every lookup, insert, and removal makes to
/// descend the trie — not about an operation's total cost. **The walk allocates nothing
/// for every built-in key, `Path` / `PathBuf` included:** a slice key yields `&C`
/// borrows, a `str` yields `char`s and an integer its big-endian bytes on the stack, and
/// a `Path` yields borrowed [`std::path::Component`]s. Turning a walked component into the
/// owned form the trie stores happens **only on [`insert`](crate::unsync::Radix::insert)**,
/// via [`to_owned`](RadixKey::to_owned): for most keys that is a trivial `Copy`, and for a
/// `Path` it is the one allocation — an `OsString` per *stored* component (never on a read).
///
/// Everything else a call does is separate and independent of the key type:
/// [`insert`](crate::unsync::Radix::insert) allocates the trie nodes it stores, and
/// the ordered / iterator reads ([`range`](crate::unsync::Radix::range), `values`,
/// `walk_*`, `minimum`, …) allocate heap traversal state and, where they return keys,
/// `Vec<`[`Owned`](RadixKey::Owned)`>` keys — see those methods.
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
/// Sequence keys yield `&C` and store `C`: `[C]`, `Vec<C>`, `Box<[C]>`, `[C; N]`
/// (`C: Ord + Clone`). `str` and `String` yield and store `char` (and alias each
/// other). These byte-addressed types yield `&u8` and store `u8`: `OsStr` / `OsString`
/// / `CStr` / `CString`, and the integer types (`u8`…`u128`, `i8`…`i128`) yield `u8`
/// by value (their big-endian bytes; signed keys flip the sign bit).
///
/// The integer encoding is injective and order-preserving **only within a single
/// integer type** — across types or widths the bytes collide and mis-order (`0i8`
/// and `128u8` both encode `[0x80]`; `256u16` sorts before `255u8`), so a
/// numeric-keyed trie must be **homogeneous** (one integer type).
///
/// `Path` and `PathBuf` yield borrowed [`std::path::Component`]s and store `OsString`
/// (following [`Path::components`](std::path::Path::components): a *leading* `.` and any
/// `..` are preserved, so `./a` and `a` are different keys), so `a/b` is an ancestor of
/// `a/b/c` but not of `a/bc`. Their **walk is allocation-free**; only
/// [`insert`](crate::unsync::Radix::insert) allocates, one `OsString` per stored
/// component (a `std::path::Component` cannot be reconstructed from owned storage — its
/// prefix variant has no public constructor — so the trie stores and orders `OsString`,
/// with `compare` bridging a stored `OsString` and a walked `Component` through
/// [`OsStr`](std::ffi::OsStr)). The `OsStr` and `Path` families require the `std` feature.
///
/// # Examples
///
/// ```
/// use iradix::RadixKey;
///
/// let key: &[u8] = b"abc";
/// let collected: Vec<u8> = key.components().copied().collect();
/// assert_eq!(collected, b"abc");
/// ```
pub trait RadixKey {
  /// The borrowed component yielded while walking the key — zero-copy, it borrows
  /// from the key. The descent compares it against a stored [`Owned`](Self::Owned)
  /// component with [`compare`](Self::compare); [`insert`](crate::unsync::Radix::insert)
  /// turns it into its owned form with [`to_owned`](Self::to_owned).
  type Component<'a>: Clone
  where
    Self: 'a;

  /// The owned, `Sized` component the trie stores (produced only on
  /// [`insert`](crate::unsync::Radix::insert)). Its [`Ord`](core::cmp::Ord) orders
  /// stored components against each other; it must agree with
  /// [`compare`](Self::compare) (see the trait's *Consistency* section).
  type Owned: Ord + Clone;

  /// The (nameable) iterator [`components`](Self::components) returns.
  type Iter<'a>: Iterator<Item = Self::Component<'a>>
  where
    Self: 'a;

  /// Returns an iterator over the key's components, in order.
  ///
  /// Each item is a borrowed [`Component<'_>`](Self::Component) — the walk is
  /// zero-copy. Must be deterministic — every call yields the same sequence (see the
  /// trait's *Determinism* section); the trie may walk a key more than once per
  /// operation.
  fn components(&self) -> Self::Iter<'_>;

  /// Turns a walked [`Component`](Self::Component) into the [`Owned`](Self::Owned)
  /// component the trie stores. Called only on the
  /// [`insert`](crate::unsync::Radix::insert) path — the sole allocation of the key
  /// walk (and, for most keys, a trivial `Copy`).
  fn to_owned(c: Self::Component<'_>) -> Self::Owned;

  /// Compares a **stored** [`Owned`](Self::Owned) component against a **walked**
  /// [`Component`](Self::Component) — the descent step. Must be consistent with
  /// [`Owned: Ord`](Self::Owned) (see the trait's *Consistency* section).
  fn compare(owned: &Self::Owned, walked: Self::Component<'_>) -> Ordering;
}

impl<C> RadixKey for [C]
where
  C: Ord + Clone,
{
  type Component<'a>
    = &'a C
  where
    Self: 'a;
  type Owned = C;
  type Iter<'a>
    = core::slice::Iter<'a, C>
  where
    Self: 'a;

  #[inline]
  fn components(&self) -> Self::Iter<'_> {
    self.iter()
  }

  #[inline]
  fn to_owned(c: Self::Component<'_>) -> Self::Owned {
    c.clone()
  }

  #[inline]
  fn compare(owned: &Self::Owned, walked: Self::Component<'_>) -> Ordering {
    owned.cmp(walked)
  }
}

impl<C> RadixKey for Vec<C>
where
  C: Ord + Clone,
{
  type Component<'a>
    = &'a C
  where
    Self: 'a;
  type Owned = C;
  type Iter<'a>
    = core::slice::Iter<'a, C>
  where
    Self: 'a;

  #[inline]
  fn components(&self) -> Self::Iter<'_> {
    self.iter()
  }

  #[inline]
  fn to_owned(c: Self::Component<'_>) -> Self::Owned {
    c.clone()
  }

  #[inline]
  fn compare(owned: &Self::Owned, walked: Self::Component<'_>) -> Ordering {
    owned.cmp(walked)
  }
}

impl RadixKey for str {
  type Component<'a> = char;
  type Owned = char;
  type Iter<'a> = core::str::Chars<'a>;

  #[inline]
  fn components(&self) -> Self::Iter<'_> {
    // A `str` is character-addressed, not byte-addressed: each component is a
    // `char`. A `str` key and a `Vec<char>` / `[char]` key over the same
    // characters therefore decompose identically and address the same trie path.
    self.chars()
  }

  #[inline]
  fn to_owned(c: Self::Component<'_>) -> Self::Owned {
    c
  }

  #[inline]
  fn compare(owned: &Self::Owned, walked: Self::Component<'_>) -> Ordering {
    owned.cmp(&walked)
  }
}

/// `String` is character-addressed, mirroring [`str`] — the two alias (the same
/// characters decompose to the same `char` sequence and address the same path).
impl RadixKey for String {
  type Component<'a> = char;
  type Owned = char;
  type Iter<'a> = core::str::Chars<'a>;

  #[inline]
  fn components(&self) -> Self::Iter<'_> {
    self.chars()
  }

  #[inline]
  fn to_owned(c: Self::Component<'_>) -> Self::Owned {
    c
  }

  #[inline]
  fn compare(owned: &Self::Owned, walked: Self::Component<'_>) -> Ordering {
    owned.cmp(&walked)
  }
}

impl<C, const N: usize> RadixKey for [C; N]
where
  C: Ord + Clone,
{
  type Component<'a>
    = &'a C
  where
    Self: 'a;
  type Owned = C;
  type Iter<'a>
    = core::slice::Iter<'a, C>
  where
    Self: 'a;

  #[inline]
  fn components(&self) -> Self::Iter<'_> {
    self.iter()
  }

  #[inline]
  fn to_owned(c: Self::Component<'_>) -> Self::Owned {
    c.clone()
  }

  #[inline]
  fn compare(owned: &Self::Owned, walked: Self::Component<'_>) -> Ordering {
    owned.cmp(walked)
  }
}

impl<C> RadixKey for Box<[C]>
where
  C: Ord + Clone,
{
  type Component<'a>
    = &'a C
  where
    Self: 'a;
  type Owned = C;
  type Iter<'a>
    = core::slice::Iter<'a, C>
  where
    Self: 'a;

  #[inline]
  fn components(&self) -> Self::Iter<'_> {
    self.iter()
  }

  #[inline]
  fn to_owned(c: Self::Component<'_>) -> Self::Owned {
    c.clone()
  }

  #[inline]
  fn compare(owned: &Self::Owned, walked: Self::Component<'_>) -> Ordering {
    owned.cmp(walked)
  }
}

/// A C string keys on its bytes, excluding the terminating nul.
impl RadixKey for CStr {
  type Component<'a> = &'a u8;
  type Owned = u8;
  type Iter<'a> = core::slice::Iter<'a, u8>;

  #[inline]
  fn components(&self) -> Self::Iter<'_> {
    self.to_bytes().iter()
  }

  #[inline]
  fn to_owned(c: Self::Component<'_>) -> Self::Owned {
    *c
  }

  #[inline]
  fn compare(owned: &Self::Owned, walked: Self::Component<'_>) -> Ordering {
    owned.cmp(walked)
  }
}

impl RadixKey for std::ffi::CString {
  type Component<'a> = &'a u8;
  type Owned = u8;
  type Iter<'a> = core::slice::Iter<'a, u8>;

  #[inline]
  fn components(&self) -> Self::Iter<'_> {
    self.to_bytes().iter()
  }

  #[inline]
  fn to_owned(c: Self::Component<'_>) -> Self::Owned {
    *c
  }

  #[inline]
  fn compare(owned: &Self::Owned, walked: Self::Component<'_>) -> Ordering {
    owned.cmp(walked)
  }
}

// Integer keys decompose to their big-endian bytes so that, WITHIN a single integer
// type, the trie's component-lexicographic order matches numeric order and the
// encoding is injective (enabling ordered / range queries over numeric keys). Signed
// integers flip the sign bit first, so `… < -1 < 0 < 1 < …` still holds in unsigned
// byte order. The bytes are NOT comparable ACROSS integer types or widths — `0i8` and
// `128u8` both encode as `[0x80]`, and `256u16` sorts before `255u8` — so a
// numeric-keyed trie must be homogeneous (keyed by one integer type). The bytes are
// yielded by value (a walked component is `u8`, not `&u8`) since they live only for the
// duration of the walk.
macro_rules! impl_radixkey_uint {
  ($($t:ty => $n:literal),+ $(,)?) => {$(
    impl RadixKey for $t {
      type Component<'a> = u8;
      type Owned = u8;
      type Iter<'a> = core::array::IntoIter<u8, $n>;

      #[inline]
      fn components(&self) -> Self::Iter<'_> {
        self.to_be_bytes().into_iter()
      }

      #[inline]
      fn to_owned(c: Self::Component<'_>) -> Self::Owned {
        c
      }

      #[inline]
      fn compare(owned: &Self::Owned, walked: Self::Component<'_>) -> Ordering {
        owned.cmp(&walked)
      }
    }
  )+};
}

macro_rules! impl_radixkey_int {
  ($($t:ty => $u:ty, $n:literal),+ $(,)?) => {$(
    impl RadixKey for $t {
      type Component<'a> = u8;
      type Owned = u8;
      type Iter<'a> = core::array::IntoIter<u8, $n>;

      #[inline]
      fn components(&self) -> Self::Iter<'_> {
        // `(1 as $u).rotate_right(1)` is the sign-bit mask for this width; XOR
        // flips it so negatives sort below non-negatives in big-endian byte order.
        ((*self as $u) ^ (1 as $u).rotate_right(1))
          .to_be_bytes()
          .into_iter()
      }

      #[inline]
      fn to_owned(c: Self::Component<'_>) -> Self::Owned {
        c
      }

      #[inline]
      fn compare(owned: &Self::Owned, walked: Self::Component<'_>) -> Ordering {
        owned.cmp(&walked)
      }
    }
  )+};
}

impl_radixkey_uint!(u8 => 1, u16 => 2, u32 => 4, u64 => 8, u128 => 16);
impl_radixkey_int!(
  i8 => u8, 1,
  i16 => u16, 2,
  i32 => u32, 4,
  i64 => u64, 8,
  i128 => u128, 16,
);

/// An OS string keys on its platform-encoded bytes
/// ([`OsStr::as_encoded_bytes`](std::ffi::OsStr::as_encoded_bytes)). This is
/// byte-addressed, **not** path-component-addressed: for a filesystem path whose
/// *components* form the key, use [`Path`](std::path::Path) (which keys on
/// `OsString` segments).
#[cfg(feature = "std")]
#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
impl RadixKey for std::ffi::OsStr {
  type Component<'a> = &'a u8;
  type Owned = u8;
  type Iter<'a> = core::slice::Iter<'a, u8>;

  #[inline]
  fn components(&self) -> Self::Iter<'_> {
    self.as_encoded_bytes().iter()
  }

  #[inline]
  fn to_owned(c: Self::Component<'_>) -> Self::Owned {
    *c
  }

  #[inline]
  fn compare(owned: &Self::Owned, walked: Self::Component<'_>) -> Ordering {
    owned.cmp(walked)
  }
}

#[cfg(feature = "std")]
#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
impl RadixKey for std::ffi::OsString {
  type Component<'a> = &'a u8;
  type Owned = u8;
  type Iter<'a> = core::slice::Iter<'a, u8>;

  #[inline]
  fn components(&self) -> Self::Iter<'_> {
    self.as_os_str().as_encoded_bytes().iter()
  }

  #[inline]
  fn to_owned(c: Self::Component<'_>) -> Self::Owned {
    *c
  }

  #[inline]
  fn compare(owned: &Self::Owned, walked: Self::Component<'_>) -> Ordering {
    owned.cmp(walked)
  }
}

/// A path keys on its **components** — the walk yields borrowed
/// [`std::path::Component`]s (via [`Path::components`](std::path::Path::components), so
/// it follows that method's normalization: redundant separators and *interior* `.` are
/// dropped, but a **leading** `.` yields a `CurDir` component and `..` a `ParentDir`
/// component, both preserved — so `./a` and `a`, or `a/../b` and `b`, are DIFFERENT
/// keys). Unlike a byte key, `a/b` is an ancestor of `a/b/c` but **not** of `a/bc`.
///
/// The **walk is allocation-free**. The trie stores each component as an
/// [`OsString`](std::ffi::OsString) (a [`std::path::Component`] cannot be reconstructed
/// from owned storage — its prefix variant has no public constructor), so only
/// [`insert`](crate::unsync::Radix::insert) allocates: one `OsString` per stored
/// component, via [`to_owned`](RadixKey::to_owned). Stored components are ordered by
/// `OsString`'s [`Ord`](core::cmp::Ord), and [`compare`](RadixKey::compare) bridges a
/// stored `OsString` and a walked `Component` through their common
/// [`OsStr`](std::ffi::OsStr) — the two agree.
#[cfg(feature = "std")]
#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
impl RadixKey for std::path::Path {
  type Component<'a> = std::path::Component<'a>;
  type Owned = std::ffi::OsString;
  type Iter<'a> = std::path::Components<'a>;

  #[inline]
  fn components(&self) -> Self::Iter<'_> {
    std::path::Path::components(self)
  }

  #[inline]
  fn to_owned(c: Self::Component<'_>) -> Self::Owned {
    c.as_os_str().to_os_string()
  }

  #[inline]
  fn compare(owned: &Self::Owned, walked: Self::Component<'_>) -> Ordering {
    owned.as_os_str().cmp(walked.as_os_str())
  }
}

/// Keys on its `OsString` path components, exactly like [`Path`](std::path::Path);
/// its walk is allocation-free and only [`insert`](crate::unsync::Radix::insert)
/// allocates the stored `OsString`s.
#[cfg(feature = "std")]
#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
impl RadixKey for std::path::PathBuf {
  type Component<'a> = std::path::Component<'a>;
  type Owned = std::ffi::OsString;
  type Iter<'a> = std::path::Components<'a>;

  #[inline]
  fn components(&self) -> Self::Iter<'_> {
    std::path::Path::components(self)
  }

  #[inline]
  fn to_owned(c: Self::Component<'_>) -> Self::Owned {
    c.as_os_str().to_os_string()
  }

  #[inline]
  fn compare(owned: &Self::Owned, walked: Self::Component<'_>) -> Ordering {
    owned.as_os_str().cmp(walked.as_os_str())
  }
}

#[cfg(test)]
mod tests;

use core::borrow::Borrow;

use std::vec::Vec;

/// A key that decomposes into a sequence of [`Component`](RadixKey::Component)s.
///
/// This is the bring-your-own-key seam: a [`Radix`](crate::unsync::Radix) is generic over
/// the *component* type, not over a concrete key type. Implementing `RadixKey`
/// for your own key lets it be looked up and stored without the trie ever
/// allocating during a read — [`components`](RadixKey::components) yields borrowed
/// components, and only [`insert`](crate::unsync::Radix::insert) clones each one
/// (the component is `Clone`) to store it.
///
/// # Zero-copy lookups
///
/// A slice key (`&[C]`) yields `&C` borrows, so [`get`](crate::unsync::Radix::get) walks
/// the trie without allocating. A synthesized key may instead yield owned
/// components by value (a `char` from a `str`, say); these still satisfy the
/// [`Borrow`](core::borrow::Borrow) bound via the blanket `impl Borrow<T> for T`,
/// and a read still allocates nothing.
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

#[cfg(test)]
mod tests;

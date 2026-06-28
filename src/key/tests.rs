use super::*;
use core::borrow::Borrow;
use std::{vec, vec::Vec};

fn collect<K>(key: &K) -> Vec<K::Component>
where
  K: RadixKey + ?Sized,
  K::Component: Clone,
{
  key.components().map(|c| c.borrow().clone()).collect()
}

#[test]
fn slice_components_in_order() {
  let key: &[u8] = b"abc";
  assert_eq!(collect(key), vec![b'a', b'b', b'c']);
}

#[test]
fn vec_components_in_order() {
  let key: Vec<u8> = b"xyz".to_vec();
  assert_eq!(collect(&key), vec![b'x', b'y', b'z']);
}

#[test]
fn str_components_are_chars() {
  let key = "ab€";
  assert_eq!(collect(key), vec!['a', 'b', '€']);
}

#[test]
fn str_aliases_vec_char() {
  // The aliasing contract: a `str` key and a `Vec<char>` key over the same
  // characters decompose to the same component sequence with the same component
  // type, so they address the same trie path.
  let s = "héllo";
  let v: Vec<char> = "héllo".chars().collect();
  assert_eq!(collect(s), collect(&v));
}

#[test]
fn empty_keys_yield_no_components() {
  let empty_slice: &[u8] = &[];
  let empty_vec: Vec<u8> = Vec::new();
  assert!(empty_slice.components().next().is_none());
  assert!(empty_vec.components().next().is_none());
  assert!("".components().next().is_none());
}

use super::*;
use core::borrow::Borrow;
use std::{boxed::Box, string::String, vec, vec::Vec};

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

#[test]
fn string_aliases_str() {
  let s = String::from("héllo");
  assert_eq!(collect(&s), collect("héllo"));
}

#[test]
fn array_and_box_slice_are_element_keyed() {
  let arr: [u8; 3] = [1, 2, 3];
  assert_eq!(collect(&arr), vec![1u8, 2, 3]);
  let bx: Box<[u8]> = vec![4u8, 5].into_boxed_slice();
  assert_eq!(collect(&bx), vec![4u8, 5]);
}

#[test]
fn cstr_keys_on_bytes_excluding_nul() {
  let cs = std::ffi::CString::new("abc").unwrap();
  assert_eq!(collect(cs.as_c_str()), vec![b'a', b'b', b'c']);
  assert_eq!(collect(&cs), vec![b'a', b'b', b'c']);
}

#[test]
fn unsigned_int_is_big_endian_and_order_preserving() {
  assert_eq!(collect(&0x0102_0304u32), vec![0x01u8, 0x02, 0x03, 0x04]);
  // component-lexicographic order matches numeric order.
  assert!(collect(&1u32) < collect(&2u32));
  assert!(collect(&255u32) < collect(&256u32));
}

#[test]
fn signed_int_is_order_preserving_across_zero() {
  // the flipped sign bit keeps negatives below non-negatives in byte order.
  assert!(collect(&i32::MIN) < collect(&-1i32));
  assert!(collect(&-1i32) < collect(&0i32));
  assert!(collect(&0i32) < collect(&1i32));
  assert!(collect(&1i32) < collect(&i32::MAX));
}

#[test]
fn integer_encoding_is_per_type_only() {
  // The byte encoding is injective + order-preserving only WITHIN one integer type;
  // it is NOT comparable across types or widths, so a numeric trie must be
  // homogeneous. These cross-type collisions / mis-orderings are the documented
  // contract, asserted here so the constraint stays explicit.
  assert_eq!(collect(&0i8), collect(&128u8)); // both [0x80]
  assert_eq!(collect(&-1i8), collect(&127u8)); // both [0x7f]
  assert!(collect(&256u16) < collect(&255u8)); // [0x01,0x00] < [0xff] — widths differ
}

#[cfg(feature = "std")]
#[test]
fn os_str_and_os_string_are_byte_keyed() {
  let os = std::ffi::OsStr::new("abc");
  assert_eq!(collect(os), b"abc".to_vec());
  let oss = std::ffi::OsString::from("xy");
  assert_eq!(collect(&oss), b"xy".to_vec());
}

#[cfg(feature = "std")]
#[test]
fn path_is_component_keyed() {
  use std::{
    ffi::OsString,
    path::{Path, PathBuf},
  };

  let want = vec![
    OsString::from("a"),
    OsString::from("b"),
    OsString::from("c"),
  ];
  assert_eq!(collect(Path::new("a/b/c")), want);
  assert_eq!(collect(&PathBuf::from("a/b/c")), want);
  // redundant separators and *interior* `.` normalize away (Path::components).
  assert_eq!(collect(Path::new("a//b/./c")), want);

  // Component keying (unlike byte keying): "a/b" is a prefix of "a/b/c" but the
  // components of "a/bc" differ from "a/b", so it is not an ancestor.
  let ab = collect(Path::new("a/b"));
  let abc = collect(Path::new("a/b/c"));
  assert!(abc.starts_with(&ab));
  assert_ne!(collect(Path::new("a/bc")), ab);
}

#[cfg(feature = "std")]
#[test]
fn path_leading_dot_and_parent_dir_are_preserved() {
  use std::path::Path;

  // Interior "." and redundant separators normalize away…
  assert_eq!(collect(Path::new("a/./b//c")), collect(Path::new("a/b/c")));
  // …but a LEADING "." is a CurDir component, so "./a" and "a" differ.
  assert_ne!(collect(Path::new("./a")), collect(Path::new("a")));
  assert_eq!(collect(Path::new(".")).len(), 1); // "." alone: one CurDir component
  // ".." is preserved as a ParentDir component (never resolved).
  assert_eq!(collect(Path::new("a/../b")).len(), 3); // a, "..", b
  assert_ne!(collect(Path::new("a/../b")), collect(Path::new("b")));
}

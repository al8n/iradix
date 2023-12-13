use core::fmt::Debug;

use crate::sync::Arc;

pub enum Cow<T> {
  Borrowed(Arc<T>),
  Owned(T),
}

impl<T> core::ops::Deref for Cow<T> {
  type Target = T;

  fn deref(&self) -> &Self::Target {
    match self {
      Cow::Borrowed(t) => t,
      Cow::Owned(t) => t,
    }
  }
}

impl<T: Clone> core::ops::DerefMut for Cow<T> {
  fn deref_mut(&mut self) -> &mut Self::Target {
    match self {
      Cow::Borrowed(t) => {
        *self = Cow::Owned((**t).clone());
        match self {
          Cow::Borrowed(_) => unreachable!(),
          Cow::Owned(t) => t,
        }
      }
      Cow::Owned(t) => t,
    }
  }
}

impl<T: Clone> Cow<T> {
  pub fn to_borrowed(&self) -> Self {
    match self {
      Cow::Borrowed(t) => Cow::Borrowed(t.clone()),
      Cow::Owned(t) => Cow::Borrowed(Arc::new(t.clone())),
    }
  }
}

impl<T: Debug> Debug for Cow<T> {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    match self {
      Cow::Borrowed(t) => f.debug_tuple("Cow::Borrowed").field(t).finish(),
      Cow::Owned(t) => f.debug_tuple("Cow::Owned").field(t).finish(),
    }
  }
}

impl<T: Default> Default for Cow<T> {
  fn default() -> Self {
    Cow::Owned(T::default())
  }
}

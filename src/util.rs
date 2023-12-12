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

impl<T: Default> Default for Cow<T> {
  fn default() -> Self {
    Cow::Owned(T::default())
  }
}

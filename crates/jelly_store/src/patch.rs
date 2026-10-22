/// A nullable value in a partial update. Keeping and clearing stay distinct all the way
/// to the write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Patch<T> {
    #[default]
    Keep,
    Set(T),
    Clear,
}

impl<T> Patch<T> {
    pub fn set_or_clear(value: Option<T>) -> Self {
        value.map_or(Self::Clear, Self::Set)
    }

    /// Whether the statement should write the column.
    pub(crate) const fn is_touched(&self) -> bool {
        match self {
            Self::Keep => false,
            Self::Set(_) | Self::Clear => true,
        }
    }

    pub(crate) const fn value(&self) -> Option<T>
    where
        T: Copy,
    {
        match *self {
            Self::Set(value) => Some(value),
            Self::Keep | Self::Clear => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Patch;

    #[test]
    fn a_patch_tells_keeping_a_column_from_clearing_it() {
        assert_eq!(Patch::<u32>::default(), Patch::Keep);
        assert_eq!(Patch::Set(9).value(), Some(9));
        assert_eq!(Patch::<u32>::Clear.value(), None);

        assert!(!Patch::<u32>::Keep.is_touched());
        assert!(Patch::Set(1).is_touched());
        assert!(Patch::<u32>::Clear.is_touched());

        assert_eq!(Patch::set_or_clear(Some(3)), Patch::Set(3));
        assert_eq!(Patch::<u32>::set_or_clear(None), Patch::Clear);
    }
}

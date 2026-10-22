use jelly_core::Amount;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MoneroBalance {
    pub(crate) total: Amount,
    pub(crate) spendable: Amount,
    pub(crate) reserved: Amount,
}

impl MoneroBalance {
    pub const fn spendable(self) -> Amount {
        self.spendable
    }

    pub const fn total(self) -> Amount {
        self.total
    }

    pub const fn reserved(self) -> Amount {
        self.reserved
    }

    pub const fn locked(self) -> Amount {
        self.total.saturating_sub(self.spendable).saturating_sub(self.reserved)
    }
}

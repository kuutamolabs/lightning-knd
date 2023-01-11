use anyhow::Result;
use bdk::Balance;
use lightning_knd::api::WalletInterface;

pub struct MockWallet {
    balance: Balance,
}

impl WalletInterface for MockWallet {
    fn balance(&self) -> Result<bdk::Balance> {
        Ok(self.balance.clone())
    }
}

impl Default for MockWallet {
    fn default() -> Self {
        Self {
            balance: Balance {
                immature: 1,
                trusted_pending: 2,
                untrusted_pending: 3,
                confirmed: 4,
            },
        }
    }
}

use std::fmt;

use lightning::ln::{PaymentPreimage, PaymentSecret};
use postgres_types::{FromSql, ToSql};

#[derive(Debug, ToSql, FromSql, PartialEq)]
#[postgres(name = "htlc_status")]
pub enum HTLCStatus {
    #[postgres(name = "succeeded")]
    Succeeded,
    #[postgres(name = "failed")]
    Failed,
}

#[derive(Debug, PartialEq)]
pub struct Payment {
    pub preimage: Option<PaymentPreimage>,
    pub secret: Option<PaymentSecret>,
    pub status: HTLCStatus,
    pub amount_msat: MillisatAmount,
    pub is_outbound: bool
}

#[derive(Debug, PartialEq)]
pub struct MillisatAmount(pub i64);

impl fmt::Display for MillisatAmount {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

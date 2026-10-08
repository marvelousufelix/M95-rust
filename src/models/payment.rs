use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Payment {
    pub id: Uuid,
    pub merchant_id: Uuid,
    pub wallet_id: Uuid,
    pub wallet_address: String,
    pub tx_hash: String,
    pub amount_stroops: i64,
    pub asset: String,
    pub network: String,
    pub status: String,
    pub confirmations: i32,
    /// The Stellar ledger sequence where this transaction was confirmed.
    /// Set when the transaction is first detected on-chain; used to compute confirmation progress.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmation_ledger: Option<i64>,
    /// Number of ledgers required to pass since confirmation_ledger before this payment
    /// is considered finalized. Currently fixed at 32 for all payments.
    pub confirmation_threshold: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewPayment {
    pub merchant_id: Uuid,
    pub wallet_id: Uuid,
    pub wallet_address: String,
    pub tx_hash: String,
    pub amount_stroops: i64,
    pub asset: String,
    pub network: String,
}

#[derive(Debug, Clone, Copy)]
pub enum UpdatePaymentStatus {
    Verified,
    Confirmed,
    Failed,
}
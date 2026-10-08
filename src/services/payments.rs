use sqlx::PgPool;
use uuid::Uuid;

use crate::models::{NewPayment, Payment, UpdatePaymentStatus};

#[derive(Debug, thiserror::Error)]
pub enum PaymentError {
    #[error("wallet not found")]
    WalletNotFound,
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

pub async fn record_deposit(db: &PgPool, payment: NewPayment) -> Result<Payment, PaymentError> {
    let existing = sqlx::query_as::<_, Payment>(
        "SELECT id, merchant_id, wallet_id, wallet_address, tx_hash, amount_stroops, asset,
                network, status, confirmations, confirmation_ledger, confirmation_threshold, created_at, updated_at
           FROM payments
          WHERE tx_hash = $1",
    )
    .bind(&payment.tx_hash)
    .fetch_optional(db)
    .await?;

    if let Some(p) = existing {
        return Ok(p);
    }

    sqlx::query_as::<_, Payment>(
        "INSERT INTO payments (
             merchant_id, wallet_id, wallet_address, tx_hash, amount_stroops, asset, network, status, confirmation_threshold
         )
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'detected', 32)
         RETURNING id, merchant_id, wallet_id, wallet_address, tx_hash, amount_stroops, asset,
                   network, status, confirmations, confirmation_ledger, confirmation_threshold, created_at, updated_at",
    )
    .bind(payment.merchant_id)
    .bind(payment.wallet_id)
    .bind(&payment.wallet_address)
    .bind(&payment.tx_hash)
    .bind(payment.amount_stroops)
    .bind(&payment.asset)
    .bind(&payment.network)
    .fetch_one(db)
    .await
    .map_err(PaymentError::Database)
}

/// Sets a payment's status and, if updating to Verified, also sets the confirmation_ledger.
/// This is used when transitioning detected → verified.
pub async fn set_status(
    db: &PgPool,
    id: Uuid,
    new_status: UpdatePaymentStatus,
) -> Result<Option<Payment>, sqlx::Error> {
    let status = match new_status {
        UpdatePaymentStatus::Verified => "verified",
        UpdatePaymentStatus::Confirmed => "confirmed",
        UpdatePaymentStatus::Failed => "failed",
    };
    sqlx::query_as::<_, Payment>(
        "UPDATE payments
            SET status = $2, updated_at = now()
          WHERE id = $1
          RETURNING id, merchant_id, wallet_id, wallet_address, tx_hash, amount_stroops, asset,
                    network, status, confirmations, confirmation_ledger, confirmation_threshold, created_at, updated_at",
    )
    .bind(id)
    .bind(status)
    .fetch_optional(db)
    .await
}

/// Sets the confirmation_ledger when a payment is first verified on-chain.
pub async fn set_confirmation_ledger(
    db: &PgPool,
    id: Uuid,
    ledger: i64,
) -> Result<Option<Payment>, sqlx::Error> {
    sqlx::query_as::<_, Payment>(
        "UPDATE payments
            SET confirmation_ledger = $2, updated_at = now()
          WHERE id = $1
          RETURNING id, merchant_id, wallet_id, wallet_address, tx_hash, amount_stroops, asset,
                    network, status, confirmations, confirmation_ledger, confirmation_threshold, created_at, updated_at",
    )
    .bind(id)
    .bind(ledger)
    .fetch_optional(db)
    .await
}

pub async fn payments_by_merchant(
    db: &PgPool,
    merchant_id: Uuid,
    limit: i64,
) -> Result<Vec<Payment>, sqlx::Error> {
    sqlx::query_as::<_, Payment>(
        "SELECT id, merchant_id, wallet_id, wallet_address, tx_hash, amount_stroops, asset,
                network, status, confirmations, confirmation_ledger, confirmation_threshold, created_at, updated_at
           FROM payments
          WHERE merchant_id = $1
          ORDER BY created_at DESC
          LIMIT $2",
    )
    .bind(merchant_id)
    .bind(limit)
    .fetch_all(db)
    .await
}

pub async fn payment_by_id(db: &PgPool, id: Uuid) -> Result<Option<Payment>, sqlx::Error> {
    sqlx::query_as::<_, Payment>(
        "SELECT id, merchant_id, wallet_id, wallet_address, tx_hash, amount_stroops, asset,
                network, status, confirmations, confirmation_ledger, confirmation_threshold, created_at, updated_at
           FROM payments
          WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
}

/// Finds all verified payments that haven't reached their confirmation threshold yet.
/// Returns (payment_id, confirmation_ledger, confirmation_threshold) for each pending confirmation.
pub async fn pending_confirmations(
    db: &PgPool,
    current_ledger: i64,
) -> Result<Vec<(Uuid, i64, i32)>, sqlx::Error> {
    sqlx::query_as::<_, (Uuid, i64, i32)>(
        "SELECT id, confirmation_ledger, confirmation_threshold
           FROM payments
          WHERE status = 'verified'
            AND confirmation_ledger IS NOT NULL
            AND $1 - confirmation_ledger < confirmation_threshold",
    )
    .bind(current_ledger)
    .fetch_all(db)
    .await
}

/// Finds all verified payments that have reached their confirmation threshold.
/// These are ready to transition to confirmed.
pub async fn ready_to_confirm(
    db: &PgPool,
    current_ledger: i64,
) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT id
           FROM payments
          WHERE status = 'verified'
            AND confirmation_ledger IS NOT NULL
            AND $1 - confirmation_ledger >= confirmation_threshold
          ORDER BY created_at ASC",
    )
    .bind(current_ledger)
    .fetch_all(db)
    .await
}

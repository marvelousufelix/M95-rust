use sqlx::PgPool;
use uuid::Uuid;

use crate::blockchain::{keypair, wallet_crypto};
use crate::models::{NewWallet, Wallet};

#[derive(Debug, thiserror::Error)]
pub enum CreateWalletError {
    #[error("failed to encrypt wallet secret: {0}")]
    Encryption(String),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

/// Creates a wallet idempotently: if an active wallet already exists for the merchant,
/// returns it. Otherwise, generates a new keypair, encrypts it, inserts a new wallet,
/// and marks it as active (deactivating any previous wallets to maintain invariant).
pub async fn create_wallet(
    db: &PgPool,
    merchant_id: Uuid,
    network: &str,
    encryption_key: &[u8; 32],
) -> Result<Wallet, CreateWalletError> {
    // Check if an active wallet already exists
    if let Some(active) = get_active_wallet(db, merchant_id).await? {
        return Ok(active);
    }

    // No active wallet exists; generate a new one
    let generated = keypair::generate_keypair();
    let secret_key_encrypted = wallet_crypto::encrypt(encryption_key, &generated.secret_seed)
        .map_err(CreateWalletError::Encryption)?;

    let wallet = NewWallet {
        merchant_id,
        address: generated.public_address,
        network: network.to_string(),
        secret_key_encrypted,
    };

    // Insert new wallet and set it as active in a transaction to ensure atomicity
    let mut tx = db.begin().await?;

    // Deactivate any existing wallets (shouldn't happen but defensive)
    sqlx::query("UPDATE wallets SET is_active = false WHERE merchant_id = $1")
        .bind(merchant_id)
        .execute(&mut *tx)
        .await?;

    // Insert new wallet as active
    let wallet = sqlx::query_as::<_, Wallet>(
        "INSERT INTO wallets (merchant_id, address, network, secret_key_encrypted, is_active)
         VALUES ($1, $2, $3, $4, true)
         RETURNING id, merchant_id, address, network, created_at",
    )
    .bind(wallet.merchant_id)
    .bind(&wallet.address)
    .bind(&wallet.network)
    .bind(&wallet.secret_key_encrypted)
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(wallet)
}

/// Returns the active wallet for a merchant, if one exists.
pub async fn get_active_wallet(db: &PgPool, merchant_id: Uuid) -> Result<Option<Wallet>, sqlx::Error> {
    sqlx::query_as::<_, Wallet>(
        "SELECT id, merchant_id, address, network, created_at
           FROM wallets
          WHERE merchant_id = $1 AND is_active = true
          LIMIT 1",
    )
    .bind(merchant_id)
    .fetch_optional(db)
    .await
}

pub async fn wallet_by_merchant(
    db: &PgPool,
    merchant_id: Uuid,
) -> Result<Option<Wallet>, sqlx::Error> {
    // After migration, return the active wallet. This is now the only wallet that
    // should be used for payment requests and other operations.
    get_active_wallet(db, merchant_id).await
}

pub async fn all_wallets(db: &PgPool) -> Result<Vec<Wallet>, sqlx::Error> {
    sqlx::query_as::<_, Wallet>(
        "SELECT id, merchant_id, address, network, created_at FROM wallets WHERE network = 'stellar'",
    )
    .fetch_all(db)
    .await
}

pub async fn wallet_by_id(db: &PgPool, id: Uuid) -> Result<Option<Wallet>, sqlx::Error> {
    sqlx::query_as::<_, Wallet>(
        "SELECT id, merchant_id, address, network, created_at FROM wallets WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
}

pub async fn wallet_by_address(db: &PgPool, address: &str) -> Result<Option<Wallet>, sqlx::Error> {
    sqlx::query_as::<_, Wallet>(
        "SELECT id, merchant_id, address, network, created_at FROM wallets WHERE address = $1",
    )
    .bind(address)
    .fetch_optional(db)
    .await
}

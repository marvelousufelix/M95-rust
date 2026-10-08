use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;
use uuid::Uuid;

use crate::blockchain::stellar::{BlockchainListener, StellarListener};
use crate::models::{NewPayment, UpdateBalance, UpdatePaymentStatus};
use crate::services::{balances, payment_requests, payments, wallets};
use crate::AppState;

pub async fn run(state: Arc<AppState>, horizon_url: String, poll_interval_secs: u64) {
    let listener = StellarListener::new(horizon_url);

    loop {
        if let Err(err) = poll_once(&state.db, &listener).await {
            tracing::warn!(error = %err, "deposit poll failed");
        }
        tokio::time::sleep(Duration::from_secs(poll_interval_secs)).await;
    }
}

async fn poll_once(db: &PgPool, listener: &StellarListener) -> Result<(), String> {
    // Fetch current ledger from Horizon to check confirmation progress.
    let current_ledger = fetch_current_ledger(listener).await?;

    let addresses: Vec<String> = wallets::all_wallets(db)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|w| w.address)
        .collect();
    if addresses.is_empty() {
        return Ok(());
    }

    // Phase 1: Detect new deposits.
    let deposits = listener.fetch_deposits(&addresses).await?;
    for deposit in deposits {
        if let Err(err) = process_deposit(db, deposit).await {
            tracing::warn!(error = %err, "failed to process deposit");
        }
    }

    // Phase 2: Check for payments that have reached their confirmation threshold.
    let ready_ids = payments::ready_to_confirm(db, current_ledger)
        .await
        .map_err(|e| e.to_string())?;
    for payment_id in ready_ids {
        if let Err(err) = finalize_payment(db, payment_id).await {
            tracing::warn!(error = %err, payment_id = %payment_id, "failed to finalize payment");
        }
    }

    Ok(())
}

/// Fetch the current ledger sequence from Horizon so we can compute confirmation progress.
async fn fetch_current_ledger(listener: &StellarListener) -> Result<i64, String> {
    let url = format!(
        "{}/ledgers?order=desc&limit=1",
        listener.horizon_url.trim_end_matches('/')
    );
    let response = reqwest::get(&url)
        .await
        .map_err(|e| format!("failed to fetch current ledger: {e}"))?;

    #[derive(serde::Deserialize)]
    struct LedgersPage {
        #[serde(rename = "_embedded")]
        embedded: LedgersEmbedded,
    }

    #[derive(serde::Deserialize)]
    struct LedgersEmbedded {
        records: Vec<LedgerRecord>,
    }

    #[derive(serde::Deserialize)]
    struct LedgerRecord {
        sequence: i64,
    }

    let page: LedgersPage = response
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;

    page.embedded
        .records
        .first()
        .map(|r| r.sequence)
        .ok_or_else(|| "no ledgers returned from Horizon".to_string())
}

async fn process_deposit(db: &PgPool, d: crate::blockchain::stellar::DetectedDeposit) -> Result<(), String> {
    let Some(wallet) = wallets::wallet_by_address(db, &d.destination).await.map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    let memo = d.memo.clone();

    let payment = payments::record_deposit(
        db,
        NewPayment {
            merchant_id: wallet.merchant_id,
            wallet_id: wallet.id,
            wallet_address: wallet.address.clone(),
            tx_hash: d.tx_hash.clone(),
            amount_stroops: d.amount_stroops,
            asset: d.asset.clone(),
            network: "stellar".into(),
        },
    )
    .await
    .map_err(|e| e.to_string())?;

    if payment.status != "detected" {
        return Ok(());
    }

    // Transition detected → verified and record the confirmation ledger.
    payments::set_status(db, payment.id, UpdatePaymentStatus::Verified)
        .await
        .map_err(|e| e.to_string())?;

    payments::set_confirmation_ledger(db, payment.id, d.confirmation_ledger)
        .await
        .map_err(|e| e.to_string())?;

    // Add to pending balance (will move to available once confirmed).
    balances::apply_delta(
        db,
        &UpdateBalance {
            merchant_id: wallet.merchant_id,
            asset: d.asset.clone(),
            available_delta: 0,
            pending_delta: d.amount_stroops,
        },
    )
    .await
    .map_err(|e| e.to_string())?;

    tracing::info!(
        payment_id = %payment.id,
        tx_hash = %d.tx_hash,
        confirmation_ledger = %d.confirmation_ledger,
        "deposit verified — waiting for {} ledgers to confirm",
        32
    );

    // Try to correlate with a payment request (memo-based).
    if let Some(memo) = memo {
        if let Some(pr) = payment_requests::find_pending_by_wallet_and_memo(db, wallet.id, &memo)
            .await
            .map_err(|e| e.to_string())?
        {
            if payment.amount_stroops >= pr.amount_stroops {
                payment_requests::mark_paid(db, pr.id, payment.id)
                    .await
                    .map_err(|e| e.to_string())?;
            } else {
                tracing::warn!(
                    expected = pr.amount_stroops,
                    actual = payment.amount_stroops,
                    request_id = %pr.id,
                    "payment request underpaid — marking partial"
                );
                payment_requests::mark_partial(db, pr.id, payment.id)
                    .await
                    .map_err(|e| e.to_string())?;
            }
        }
    }

    Ok(())
}

/// Finalize a payment that has reached its confirmation threshold.
/// Transitions it from verified → confirmed and moves balance from pending → available.
async fn finalize_payment(db: &PgPool, payment_id: Uuid) -> Result<(), String> {
    let Some(payment) = payments::payment_by_id(db, payment_id)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(());
    };

    if payment.status != "verified" {
        return Ok(());
    }

    // Transition verified → confirmed.
    payments::set_status(db, payment_id, UpdatePaymentStatus::Confirmed)
        .await
        .map_err(|e| e.to_string())?;

    // Move balance from pending → available.
    balances::apply_delta(
        db,
        &UpdateBalance {
            merchant_id: payment.merchant_id,
            asset: payment.asset.clone(),
            available_delta: payment.amount_stroops,
            pending_delta: -payment.amount_stroops,
        },
    )
    .await
    .map_err(|e| e.to_string())?;

    tracing::info!(
        payment_id = %payment_id,
        tx_hash = %payment.tx_hash,
        "deposit confirmed — moved to available balance"
    );

    // TODO: dispatch payment.confirmed webhook.
    Ok(())
}

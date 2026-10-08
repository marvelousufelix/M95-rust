-- Settlement sweep batches: one row per wallet per settlement window.
-- Tracks the on-chain sweep of a merchant wallet's cNGN balance to the
-- platform settlement wallet before redemption.
--
-- Idempotency: idempotency_key = sha256(wallet_id || window_start_iso)
-- ensures a restart of the settlement worker cannot produce duplicate sweeps
-- for the same wallet in the same window.
--
-- State machine:
--   identified → sweeping → swept → redeeming → redeemed → ready_for_payout
--                    ↓                  ↓
--              sweep_failed      redemption_failed
--                    ↓                  ↓ (after max retries)
--                (retry)          terminal_failed
--
-- Atomic state transitions use:
--   UPDATE sweep_batches SET status = $new WHERE id = $id AND status = $expected
-- to guarantee only one worker wins a race.

CREATE TABLE sweep_batches (
    id                  UUID        PRIMARY KEY DEFAULT gen_random_uuid(),

    -- Which wallet is being swept
    wallet_id           UUID        NOT NULL REFERENCES wallets(id),
    merchant_id         UUID        NOT NULL REFERENCES merchants(id),

    -- The ISO-8601 timestamp of the settlement window start (e.g. '2026-10-08T02:00:00Z').
    -- Combined with wallet_id to form the idempotency_key.
    window_start        TIMESTAMPTZ NOT NULL,

    -- sha256(wallet_id::text || window_start::text) — prevents duplicate sweeps
    -- if the worker restarts mid-run. Computed by the application layer.
    idempotency_key     TEXT        NOT NULL UNIQUE,

    -- How many stroops of cNGN were eligible at batch creation time.
    -- Used to validate the actual swept amount matches expectations.
    eligible_stroops    BIGINT      NOT NULL CHECK (eligible_stroops > 0),

    -- Asset being swept (typically 'cNGN')
    asset               TEXT        NOT NULL DEFAULT 'cNGN',

    -- Current lifecycle state.
    -- CHECK constraint enforces the valid set; transition rules are application-enforced.
    status              TEXT        NOT NULL DEFAULT 'identified'
                        CHECK (status IN (
                            'identified',       -- eligible funds identified, sweep not yet started
                            'sweeping',         -- Stellar transaction submitted, awaiting confirmation
                            'swept',            -- Stellar payment confirmed on-chain
                            'redeeming',        -- issuer redemption API call in progress
                            'redeemed',         -- issuer confirmed NGN credited
                            'ready_for_payout', -- NGN available; merchant payout can proceed
                            'sweep_failed',     -- on-chain sweep failed (transient, will retry)
                            'redemption_failed',-- issuer redemption failed (transient, will retry)
                            'terminal_failed'   -- exceeded max retries; ops alert sent
                        )),

    -- Stellar transaction details (populated during 'sweeping' state, before submission,
    -- so a crash-recovery lookup can check Horizon for the tx outcome).
    stellar_tx_hash     TEXT        UNIQUE,     -- populated before Horizon submission
    stellar_tx_xdr      TEXT,                   -- the signed XDR envelope (for resubmission)

    -- Retry tracking
    retry_count         INT         NOT NULL DEFAULT 0,
    last_error          TEXT,                   -- sanitised error message, never contains secrets

    -- Timestamps
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Fast lookup of all batches in a given state (used by the worker loop)
CREATE INDEX idx_sweep_batches_status ON sweep_batches(status);

-- Fast lookup of all pending/active batches for a merchant
CREATE INDEX idx_sweep_batches_merchant_status ON sweep_batches(merchant_id, status);

-- Fast lookup by wallet for eligibility deduplication checks
CREATE INDEX idx_sweep_batches_wallet_window ON sweep_batches(wallet_id, window_start DESC);

-- Redemption attempts: tracks individual calls to the cNGN issuer API.
-- A sweep_batch transitions through at most N+1 redemption_attempts before
-- either succeeding (status='redeemed') or being marked terminal_failed.
--
-- Each attempt is a separate row so we get a full audit trail of every
-- issuer call, including transient failures, without losing history.
--
-- Idempotency: idempotency_key = sha256(sweep_batch_id || attempt_number)
-- Sent to the issuer as the redemption reference so the issuer can detect
-- duplicate submissions and return the same outcome safely.

CREATE TABLE redemption_attempts (
    id                  UUID        PRIMARY KEY DEFAULT gen_random_uuid(),

    -- The parent sweep batch this attempt belongs to
    sweep_batch_id      UUID        NOT NULL REFERENCES sweep_batches(id),

    -- Monotonically increasing counter per sweep_batch_id (1-based).
    -- Used as part of the idempotency_key and for ordering attempts.
    attempt_number      INT         NOT NULL CHECK (attempt_number >= 1),

    -- sha256(sweep_batch_id::text || attempt_number::text)
    -- Sent to the issuer as a stable reference for deduplication.
    idempotency_key     TEXT        NOT NULL UNIQUE,

    -- The amount sent to the issuer for redemption (in cNGN stroops).
    -- Should match sweep_batches.eligible_stroops unless the sweep was partial.
    amount_stroops      BIGINT      NOT NULL CHECK (amount_stroops > 0),

    -- Attempt lifecycle state
    status              TEXT        NOT NULL DEFAULT 'pending'
                        CHECK (status IN (
                            'pending',      -- about to be submitted
                            'submitted',    -- API call sent, awaiting callback/polling
                            'succeeded',    -- issuer confirmed NGN credited
                            'failed'        -- this attempt failed (may be retried in new row)
                        )),

    -- Issuer-assigned reference (populated on success or if issuer returns one on failure)
    issuer_reference    TEXT,

    -- The NGN amount (in kobo) that the issuer credited. Populated on success.
    ngn_amount_kobo     BIGINT,

    -- Sanitised error detail for failed attempts (no secrets, no keys)
    error_message       TEXT,

    -- Timestamps
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),

    -- Each (sweep_batch_id, attempt_number) pair must be unique
    UNIQUE (sweep_batch_id, attempt_number)
);

-- Fast lookup of all attempts for a given batch (ordered by attempt_number)
CREATE INDEX idx_redemption_attempts_batch ON redemption_attempts(sweep_batch_id, attempt_number);

-- Fast lookup of in-flight attempts (for worker recovery)
CREATE INDEX idx_redemption_attempts_status ON redemption_attempts(status)
    WHERE status IN ('pending', 'submitted');

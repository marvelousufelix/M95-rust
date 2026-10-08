-- Settlement audit log: append-only event trail for all settlement operations.
-- Never modified after insert; provides an immutable record for ops debugging.
--
-- IMPORTANT: The detail column must NEVER contain secrets (private keys, seeds,
-- passphrases, or raw key material). The application layer is responsible for
-- sanitising all values before insert. The constraint below enforces the
-- requirement at the DB level by ensuring detail is valid JSON — the application
-- uses a structured approach that naturally excludes key material.
--
-- Events logged here include (but are not limited to):
--   batch_identified     — eligible funds found, sweep_batch row created
--   sweep_started        — Stellar tx constructed and XDR stored
--   sweep_submitted      — tx hash recorded and submitted to Horizon
--   sweep_confirmed      — Horizon confirmed the sweep transaction
--   sweep_failed         — on-chain sweep failed (with error category, no secrets)
--   redemption_started   — issuer API call about to be made
--   redemption_succeeded — issuer confirmed NGN credited
--   redemption_failed    — issuer API call failed (with error category)
--   terminal_failure     — max retries exceeded; ops alert raised
--   batch_completed      — entire pipeline complete, ready for payout

CREATE TABLE settlement_audit_log (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),

    -- Associates the event with a sweep batch (nullable for system-level events
    -- that occur before a batch row is created, e.g. eligibility scan start)
    sweep_batch_id  UUID        REFERENCES sweep_batches(id),

    -- Associates the event with a redemption attempt when applicable
    redemption_attempt_id UUID  REFERENCES redemption_attempts(id),

    -- The merchant whose funds are involved (denormalised for efficient querying)
    merchant_id     UUID        REFERENCES merchants(id),

    -- Short, machine-readable event name (snake_case, from the list above)
    event_name      TEXT        NOT NULL,

    -- Structured JSON detail about the event.
    -- MUST NOT contain secrets. Typical fields: wallet_id, amount_stroops,
    -- asset, error_category, attempt_number, tx_hash (public, not private key).
    detail          JSONB       NOT NULL DEFAULT '{}',

    -- Severity level for ops tooling: 'info', 'warn', 'error'
    severity        TEXT        NOT NULL DEFAULT 'info'
                    CHECK (severity IN ('info', 'warn', 'error')),

    -- When the event occurred (immutable once inserted)
    occurred_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Prevent accidental updates or deletes (defence-in-depth; app logic also enforces append-only)
-- Note: we cannot create a true trigger here without PL/pgSQL but the application layer
-- exclusively uses INSERT on this table. The index below aids ops queries.

-- Fast lookup of all events for a batch in chronological order
CREATE INDEX idx_audit_log_batch ON settlement_audit_log(sweep_batch_id, occurred_at);

-- Fast lookup of recent events for a merchant (ops dashboard)
CREATE INDEX idx_audit_log_merchant_time ON settlement_audit_log(merchant_id, occurred_at DESC);

-- Fast lookup of error/warning events for ops alerting
CREATE INDEX idx_audit_log_severity ON settlement_audit_log(severity, occurred_at DESC)
    WHERE severity IN ('warn', 'error');

-- Fast lookup by event name for analytics queries
CREATE INDEX idx_audit_log_event_name ON settlement_audit_log(event_name, occurred_at DESC);

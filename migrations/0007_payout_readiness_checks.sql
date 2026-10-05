-- Track payout readiness checks for audit and debugging.
-- This table records whether Aframp's payout provider account had sufficient
-- balance to support withdrawals at the time a withdrawal was requested.
-- Used to correlate withdrawal failures with provider funding availability.
CREATE TABLE payout_readiness_checks (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  merchant_id UUID NOT NULL REFERENCES merchants(id),
  withdrawal_id UUID REFERENCES withdrawals(id),
  provider TEXT NOT NULL,
  is_ready BOOLEAN NOT NULL,
  available_balance BIGINT,
  message TEXT NOT NULL,
  checked_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Index for querying readiness checks by withdrawal
CREATE INDEX idx_payout_readiness_withdrawal ON payout_readiness_checks(withdrawal_id);

-- Index for querying readiness checks by merchant + time
CREATE INDEX idx_payout_readiness_merchant_time ON payout_readiness_checks(merchant_id, checked_at DESC);

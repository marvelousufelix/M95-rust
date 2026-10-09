-- Add confirmation ledger tracking for 32-ledger finality policy
ALTER TABLE payments ADD COLUMN confirmation_ledger BIGINT;
ALTER TABLE payments ADD COLUMN confirmation_threshold INT NOT NULL DEFAULT 32;

-- Create an index on confirmation_ledger to support the worker's query for pending confirmations
CREATE INDEX idx_payments_confirmed_pending ON payments(merchant_id, status, confirmation_ledger)
WHERE status = 'verified' AND confirmation_ledger IS NOT NULL;

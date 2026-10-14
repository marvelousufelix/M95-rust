-- Add is_active column to wallets table to enforce one-active-wallet-per-merchant invariant
ALTER TABLE wallets
  ADD COLUMN is_active BOOLEAN NOT NULL DEFAULT false;

-- Set the most recent wallet for each merchant as active
UPDATE wallets w SET is_active = true
WHERE id IN (
  SELECT id FROM (
    SELECT id, ROW_NUMBER() OVER (PARTITION BY merchant_id ORDER BY created_at DESC) as rn
    FROM wallets
  ) sub
  WHERE rn = 1
);

-- Unique index: only one active wallet per merchant
-- Partial index on is_active=true ensures NULL values don't violate uniqueness
CREATE UNIQUE INDEX idx_wallets_merchant_active
  ON wallets(merchant_id)
  WHERE is_active = true;

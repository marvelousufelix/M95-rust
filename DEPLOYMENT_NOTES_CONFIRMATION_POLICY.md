# Deployment Notes: Stellar Confirmation Policy

## Pre-Deployment Checklist

- [ ] Review PRD.md §9.2 for policy rationale
- [ ] Backup production database
- [ ] Test migrations in staging environment
- [ ] Verify merchant notification strategy (optional feature for confirmation progress)

## Migration Steps

### 1. Apply Database Migration
```bash
# Manually apply or via migration runner:
sqlx migrate add -r 0008_confirmation_threshold.sql

# Or manually:
psql $DATABASE_URL < migrations/0008_confirmation_threshold.sql
```

**What it does:**
- Adds `confirmation_ledger` column (nullable, for existing payments)
- Adds `confirmation_threshold` column (default 32)
- Creates index on (merchant_id, status, confirmation_ledger) for efficient queries

### 2. Deploy Code
```bash
# Build and test
cargo build --release
cargo test

# Deploy
docker build -t m95-api:vX.X.X .
docker push m95-api:vX.X.X
```

## Post-Deployment Behavior

### Immediate (First Poll)
- Worker detects new deposits as usual
- All new deposits get `confirmation_ledger` populated from Horizon
- Deposits transition: `detected` → `verified` (pending balance credited)
- No balance change yet visible to merchants (remains pending)

### After ~160 seconds (32 ledgers)
- Worker's Phase 2 identifies ready-to-confirm payments
- Transitions: `verified` → `confirmed`
- Balance moves: `pending` → `available`
- Merchants see balance appear in `GET /balance` available field

### Existing Data
- Any existing `detected` payments remain `detected` (unaffected)
- Any existing `verified` payments have `confirmation_ledger = NULL` and won't be auto-finalized
  - Manual fix (optional): `UPDATE payments SET confirmation_ledger = 47118521 WHERE confirmation_ledger IS NULL` (for a specific ledger)
  - Or allow them to eventually be replaced by new deposits

## Rollback Plan

If issues arise, rollback is safe because:
- Old code ignores new columns (NULL handling)
- Deposits still work through old detection pipeline
- Just prevents new confirmation logic from running

```bash
# Revert code
git revert <commit-hash>
docker build -t m95-api:vX.X.X-rollback .

# DB can stay; new columns harmless but unused
```

## Monitoring

### Metrics to Track
1. **Confirmation Time** — Should average ~160s (detect → confirmed)
2. **Pending Balance** — Should drop to zero ~160s after deposit detected
3. **Worker Errors** — Check logs for ledger fetch failures
4. **Payment Statuses** — Confirm `verified` count decreases over time as payments confirm

### Useful Queries
```sql
-- Payments awaiting confirmation
SELECT id, tx_hash, status, confirmation_ledger, created_at 
FROM payments 
WHERE status = 'verified' AND confirmation_ledger IS NOT NULL
ORDER BY created_at DESC;

-- Average confirmation time (once data available)
SELECT AVG(EXTRACT(EPOCH FROM (updated_at - created_at))) / 60 as avg_minutes_to_confirm
FROM payments
WHERE status = 'confirmed' AND created_at > NOW() - INTERVAL '1 day';

-- Pending balances by merchant
SELECT m.id, m.name, b.asset, b.pending, b.available 
FROM balances b
JOIN merchants m ON b.merchant_id = m.id
WHERE b.pending > 0
ORDER BY b.pending DESC;
```

## Testing in Staging

### 1. Run Test Suite
```bash
TEST_DATABASE_URL=postgres://... cargo test --test deposit_confirmation_flow
```

### 2. Manual Test Flow
1. Create merchant account
2. Create wallet
3. Simulate deposit (insert into payments via SQL or test helper)
4. Verify status = `detected`
5. Trigger worker poll manually
6. Verify status = `verified`, balance in pending
7. Wait/simulate ledger progression
8. Trigger worker poll again
9. Verify status = `confirmed`, balance in available

## FAQ

**Q: What if a payment's confirmation_ledger is NULL after migration?**
A: It won't be auto-finalized. The worker only processes payments with `confirmation_ledger IS NOT NULL`. Manual fix or just let new payments take over.

**Q: Can the threshold be changed later?**
A: Yes — update `confirmation_threshold` in config or per-payment if desired. Current implementation allows both.

**Q: What about payments in-flight during deployment?**
A: They continue through detection → verification as before. May take 2+ poll cycles to see balance movement but will eventually confirm.

**Q: How do merchants see confirmation progress?**
A: Via `confirmation_ledger` and `confirmation_threshold` fields in transaction details. Further UI enhancements (progress bar, ETA) can be built on top.

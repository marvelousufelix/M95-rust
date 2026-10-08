# Stellar Deposit Confirmation - Quick Reference

## What Changed

### Database
- New migration `0008_confirmation_threshold.sql` adds:
  - `confirmation_ledger` (i64, nullable) — Stellar ledger where transaction confirmed
  - `confirmation_threshold` (i32, default=32) — Ledgers required for finality
  - Index for efficient confirmation queries

### Core Logic
- **Detected → Verified:** Payment recorded with `confirmation_ledger` from Horizon, added to `pending` balance
- **Verified → Confirmed:** After 32 ledgers pass, payment moves to `confirmed`, balance moves to `available`
- **Worker:** Two-phase polling (detect deposits, then finalize confirmed ones)

### API Changes
- `GET /transactions` now includes `confirmation_ledger` and `confirmation_threshold` fields
- `GET /balance` properly separates `pending` (not yet final) from `available` (confirmed)
- Merchants see confirmation progress

## Testing
- 10 new tests in `tests/deposit_confirmation_flow.rs`
- Covers all acceptance criteria
- Verifies pending/confirmed balance behavior

## Files Modified
1. `PRD.md` — Policy documented
2. `migrations/0008_confirmation_threshold.sql` — Schema
3. `src/models/payment.rs` — Payment struct updated
4. `src/services/payments.rs` — Confirmation logic
5. `src/blockchain/stellar.rs` — Ledger extraction
6. `src/blockchain/worker.rs` — Two-phase polling
7. `tests/deposit_confirmation_flow.rs` — Test coverage

## Migration & Deployment
1. Apply migration `0008_confirmation_threshold.sql`
2. Existing `detected` and `verified` payments will have NULL `confirmation_ledger` (edge case, but won't break)
3. All new payments will use new fields correctly
4. Worker immediately starts tracking confirmations on next deployment

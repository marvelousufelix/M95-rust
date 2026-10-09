# M95-rust Development Workflow Guide

This document provides a complete reference for all development workflows in the M95-rust backend project.

## Table of Contents

- [Quick Start](#quick-start)
- [Database Management](#database-management)
- [Development Commands](#development-commands)
- [Testing](#testing)
- [API Testing](#api-testing)
- [Deployment](#deployment)
- [Troubleshooting](#troubleshooting)

---

## Quick Start

### First Time Setup

```bash
# 1. Copy environment template
cp .env.example .env

# 2. Generate secrets (run 3 times for JWT, WEBHOOK, and WALLET_ENCRYPTION_KEY)
openssl rand -hex 32

# 3. Edit .env with generated secrets and database URL
# DATABASE_URL=postgres://postgres:postgres@localhost/aframp
# JWT_SECRET=<generated-secret>
# WEBHOOK_SECRET=<generated-secret>
# WALLET_ENCRYPTION_KEY=<generated-secret>
# STELLAR_SYSTEM_WALLET_ADDRESS=G...YOUR_STELLAR_SYSTEM_WALLET
# PAYSTACK_SECRET_KEY=sk_test_...

# 4. Start PostgreSQL (Docker)
docker run -d --name aframp-postgres \
  -e POSTGRES_USER=postgres \
  -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=aframp \
  -p 5432:5432 postgres:16

# 5. Wait for PostgreSQL to be ready (5-10 seconds)
sleep 5

# 6. Run migrations
for f in migrations/*.sql; do
  docker exec -i aframp-postgres psql -U postgres -d aframp < "$f"
done

# 7. Build and run
cargo build
RUST_LOG=info cargo run
```

**Server runs on:** `http://127.0.0.1:3000`

### Using Quick Start Scripts

**Unix/Mac/Linux:**
```bash
./quick-start.sh
```

**Windows:**
```bash
quick-start.bat
```

---

## Database Management

### Docker PostgreSQL Commands

```bash
# Start existing container
docker start aframp-postgres

# Stop container
docker stop aframp-postgres

# View logs
docker logs -f aframp-postgres

# Connect to database
docker exec -it aframp-postgres psql -U postgres -d aframp

# Run migrations
for f in migrations/*.sql; do 
  docker exec -i aframp-postgres psql -U postgres -d aframp < "$f"
done

# Destroy container and data (WARNING: deletes everything)
docker rm -f aframp-postgres
```

### Database Migrations

Migrations are located in `migrations/` and numbered sequentially:

- `0001_init.sql` - Initial schema
- `0002_wallet_secret_key.sql` - Add wallet encryption
- `0003_withdrawal_failure_reason.sql` - Add withdrawal tracking
- `0004_payment_requests.sql` - Payment request system
- `0005_payment_request_partial_status.sql` - Status updates
- `0006_unique_payment_wallet_tx_hash.sql` - Transaction deduplication

**Creating new migrations:**
1. Create `migrations/XXXX_description.sql`
2. Write SQL commands
3. Apply with: `docker exec -i aframp-postgres psql -U postgres -d aframp < migrations/XXXX_description.sql`

### Using sqlx-cli

```bash
# Install sqlx-cli
cargo install sqlx-cli --no-default-features --features rustls,postgres

# Create database
sqlx database create

# Run migrations
sqlx migrate run

# Revert last migration
sqlx migrate revert
```

---

## Development Commands

### Build and Check

```bash
# Check code without building
cargo check

# Build (debug mode)
cargo build

# Build (release mode)
cargo build --release

# Auto-fix warnings (unused imports, dead code, etc.)
cargo fix --lib -p aframp
```

### Running the Server

```bash
# Run with default logging
cargo run

# Run with debug logging
RUST_LOG=debug cargo run

# Run with info logging
RUST_LOG=info cargo run

# Run in release mode
cargo run --release
```

### Code Quality

```bash
# Format code
cargo fmt

# Check formatting
cargo fmt -- --check

# Run clippy linter
cargo clippy

# Run clippy with all warnings
cargo clippy -- -D warnings
```

---

## Testing

### Test Database Setup

Integration tests require a dedicated PostgreSQL database. If `TEST_DATABASE_URL`
is not set, the test harness **panics immediately** with an actionable error
message rather than silently passing — so a missing variable cannot hide failing
tests.

```bash
# 1. Create test database (one-time setup)
docker exec -i aframp-postgres psql -U postgres -c "CREATE DATABASE aframp_test;"

# 2. Always run integration tests with TEST_DATABASE_URL set
TEST_DATABASE_URL=postgres://postgres:postgres@localhost:5432/aframp_test cargo test
```

Unit tests (library-internal tests that do not touch the database) can still be
run without Postgres:

```bash
# Unit tests only — no database required
cargo test --lib
```

### Test Commands

```bash
# Run all tests
TEST_DATABASE_URL=postgres://postgres:postgres@localhost:5432/aframp_test cargo test

# Run specific test
TEST_DATABASE_URL=postgres://postgres:postgres@localhost:5432/aframp_test cargo test test_name

# Run tests with output
TEST_DATABASE_URL=postgres://postgres:postgres@localhost:5432/aframp_test cargo test -- --nocapture

# Run tests in parallel (default)
TEST_DATABASE_URL=postgres://postgres:postgres@localhost:5432/aframp_test cargo test

# Run tests single-threaded
TEST_DATABASE_URL=postgres://postgres:postgres@localhost:5432/aframp_test cargo test -- --test-threads=1
```

### Test Structure

```
tests/
├── auth_flow.rs              - Signup, login, JWT authentication
├── wallet_flow.rs            - Wallet creation and management
├── payment_request_flow.rs   - Payment request lifecycle
├── withdrawal_flow.rs        - Withdrawal and Paystack integration
└── common/
    └── mod.rs                - Shared test utilities
```

### End-to-End Test

Run the complete payment flow with real Stellar testnet:

```bash
cargo run --example prove_payment_loop
```

This creates a merchant, customer, and submits a real Stellar transaction with memo tagging.

---

## API Testing

### Health Check

```bash
# Basic health check (should return 204)
curl -sS http://127.0.0.1:3000/health -w "\nHTTP:%{http_code}\n"

# Root endpoint (should return 404)
curl -sS http://127.0.0.1:3000/
```

### Authentication

```bash
# Signup
curl -sS -X POST http://127.0.0.1:3000/signup \
  -H "Content-Type: application/json" \
  -d '{"email":"you@example.com","password":"at-least-8-chars","name":"Your Name"}'

# Login
curl -sS -X POST http://127.0.0.1:3000/login \
  -H "Content-Type: application/json" \
  -d '{"email":"you@example.com","password":"at-least-8-chars"}'

# Save token for subsequent requests
TOKEN=$(curl -sS -X POST http://127.0.0.1:3000/login \
  -H "Content-Type: application/json" \
  -d '{"email":"you@example.com","password":"at-least-8-chars"}' \
  | python3 -c "import sys,json;print(json.load(sys.stdin)['token'])")

echo $TOKEN
```

### Wallet Operations

```bash
# Create wallet
curl -sS -X POST http://127.0.0.1:3000/wallet/create \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{}'

# Get wallet info
curl -sS http://127.0.0.1:3000/wallet \
  -H "Authorization: Bearer $TOKEN"
```

### Balance and Transactions

```bash
# Get balance
curl -sS http://127.0.0.1:3000/balance \
  -H "Authorization: Bearer $TOKEN"

# Get transactions (default limit 50)
curl -sS http://127.0.0.1:3000/transactions \
  -H "Authorization: Bearer $TOKEN"

# Get transactions with custom limit
curl -sS "http://127.0.0.1:3000/transactions?limit=10" \
  -H "Authorization: Bearer $TOKEN"
```

### User Profile

```bash
# Get current user profile
curl -sS http://127.0.0.1:3000/me \
  -H "Authorization: Bearer $TOKEN"
```

### Payment Requests

```bash
# Create payment request (2.5 XLM)
curl -sS -X POST http://127.0.0.1:3000/payment-requests \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"amount_stroops":25000000}'

# Create with custom expiry (5 minutes)
curl -sS -X POST http://127.0.0.1:3000/payment-requests \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"amount_stroops":25000000,"expires_in_secs":300}'

# List payment requests
curl -sS http://127.0.0.1:3000/payment-requests \
  -H "Authorization: Bearer $TOKEN"

# Get specific payment request (public endpoint)
curl -sS http://127.0.0.1:3000/payment-requests/<id>
```

### Withdrawals

```bash
# Create withdrawal (minimum 50 NGN = 500,000,000 stroops)
curl -sS -X POST http://127.0.0.1:3000/withdraw \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "amount_stroops":500000000,
    "asset":"cNGN",
    "bank_code":"058",
    "account_number":"0123456789"
  }'

# List withdrawals
curl -sS http://127.0.0.1:3000/withdrawals \
  -H "Authorization: Bearer $TOKEN"
```

### Stellar Testnet Tools

```bash
# Check account on testnet
curl -sS "https://horizon-testnet.stellar.org/accounts/<G...ADDRESS>"

# Fund testnet account (10,000 XLM)
curl -sS "https://friendbot.stellar.org/?addr=<G...ADDRESS>"
```

---

## Deployment

### Docker Build

```bash
# Build Docker image
docker build -t m95-rust .

# Run Docker container
docker run -d \
  --name m95-rust-api \
  -p 3000:3000 \
  --env-file .env \
  m95-rust

# View logs
docker logs -f m95-rust-api

# Stop container
docker stop m95-rust-api

# Remove container
docker rm m95-rust-api
```

### Environment Variables

Required environment variables for production:

```bash
DATABASE_URL=postgres://user:password@host:5432/aframp
APP_BIND_ADDR=0.0.0.0:3000
JWT_SECRET=<32-byte-hex>
WEBHOOK_SECRET=<32-byte-hex>
WALLET_ENCRYPTION_KEY=<32-byte-hex>
STELLAR_SYSTEM_WALLET_ADDRESS=G...
STELLAR_HORIZON_URL=https://horizon-testnet.stellar.org
STELLAR_POLL_INTERVAL_SECS=60
PAYSTACK_SECRET_KEY=sk_live_...
CORS_ALLOWED_ORIGINS=https://your-frontend.com
COOKIE_SECURE=true
COOKIE_SAME_SITE=lax
```

### Cloudflare Containers

See the separate `aframp-cloudflare-worker` repository for Cloudflare deployment.

---

## Troubleshooting

### PostgreSQL Connection Issues

```bash
# Check if PostgreSQL is running
docker ps | grep aframp-postgres

# If not running, start it
docker start aframp-postgres

# Check logs for errors
docker logs aframp-postgres

# Test connection
docker exec -it aframp-postgres psql -U postgres -d aframp -c "SELECT 1;"
```

### Port Already in Use

```bash
# Find process using port 3000
lsof -i :3000  # macOS/Linux
netstat -ano | findstr :3000  # Windows

# Kill the process
kill -9 <PID>  # macOS/Linux
taskkill /PID <PID> /F  # Windows
```

### Migration Errors

```bash
# Check which migrations have been applied
docker exec -it aframp-postgres psql -U postgres -d aframp \
  -c "SELECT * FROM _sqlx_migrations ORDER BY version;"

# Re-run specific migration
docker exec -i aframp-postgres psql -U postgres -d aframp < migrations/XXXX_name.sql
```

### Integration Tests Failing With "Missing TEST_DATABASE_URL"

**Problem:** Test run panics with:

```
Missing TEST_DATABASE_URL.
Integration tests require a running PostgreSQL instance.
```

**Solution:** Set `TEST_DATABASE_URL` before running the integration suite:

```bash
TEST_DATABASE_URL=postgres://postgres:postgres@localhost:5432/aframp_test cargo test
```

To run only unit tests (no database required):

```bash
cargo test --lib
```

### Cargo Build Errors

```bash
# Clean build artifacts
cargo clean

# Update dependencies
cargo update

# Check for issues
cargo check
```

### Stellar Worker Not Polling

**Check:**
1. `STELLAR_HORIZON_URL` is set correctly
2. `STELLAR_POLL_INTERVAL_SECS` is set (default 60)
3. Merchant has a wallet created
4. Check logs for errors: `RUST_LOG=debug cargo run`

---

## Useful Resources

- **API Documentation:** [`API.md`](API.md)
- **OpenAPI Spec:** [`openapi.yaml`](openapi.yaml)
- **Command Reference:** [`command.txt`](command.txt)
- **Product Requirements:** [`PRD.md`](PRD.md)
- **Project README:** [`README.md`](README.md)

---

## Development Checklist

Before committing code:

- [ ] `cargo fmt` - Format code
- [ ] `cargo clippy` - Run linter
- [ ] `TEST_DATABASE_URL=... cargo test` - Run tests
- [ ] `cargo check` - Verify compilation
- [ ] Update documentation if API changed
- [ ] Add migration if schema changed

---

## Common Workflows

### Adding a New API Endpoint

1. Define route in `src/api/mod.rs`
2. Create handler in appropriate `src/api/*.rs` file
3. Add service logic in `src/services/*.rs`
4. Add models in `src/models/*.rs` if needed
5. Write integration test in `tests/`
6. Update `API.md` and `openapi.yaml`
7. Test with curl commands

### Adding a New Database Table

1. Create migration: `migrations/XXXX_table_name.sql`
2. Add model in `src/models/`
3. Add service functions in `src/services/`
4. Apply migration: `sqlx migrate run`
5. Test with integration tests

### Debugging Stellar Integration

1. Enable debug logging: `RUST_LOG=debug cargo run`
2. Check Horizon is accessible: `curl https://horizon-testnet.stellar.org`
3. Verify wallet address exists on testnet
4. Check polling interval is reasonable (60 seconds)
5. Look for worker logs in output

---

**M95-rust** - All workflows preserved from original Aframp backend.

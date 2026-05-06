# Implementation Plan: Permanent Username Registry

**Date:** 2026-05-06
**Based on Design:** `/Users/vikas/dev/project/TempMail/thoughts/shared/designs/2026-05-06-permanent-username-registry-design.md`

---

## Overview

Add a permanent `email_registry` table to track all usernames ever created, and modify `create_email_address` to check this registry first for global uniqueness.

---

## Step 1: Update Database Schema Initialization

**File:** `database/src/database.rs`

In the `connect()` method, add the `email_registry` table creation to the existing SQL batch string (after the `analytics` table creation block):

```sql
CREATE TABLE IF NOT EXISTS email_registry (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    username TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL DEFAULT (now()::text)
);
CREATE INDEX IF NOT EXISTS email_registry_username_idx ON email_registry(username);
```

**Verification:** Run the app and check that `email_registry` table is created successfully.

---

## Step 2: Add New Struct for Registry Entries

**File:** `database/src/database.rs`

Add after the `EmailAddressInfo` struct definition:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailRegistryEntry {
    pub username: String,
    pub created_at: Option<String>,
}
```

---

## Step 3: Modify `create_email_address` Method

**File:** `database/src/database.rs`

Replace the existing `create_email_address` method. The new implementation must:
1. Begin a transaction
2. Insert into `email_registry` first (this is the global uniqueness gatekeeper)
3. If `email_registry` insert returns 0 rows (conflict), rollback and return error
4. Continue with existing quota and `email_addresses` inserts
5. If `email_addresses` insert returns 0 rows (conflict), rollback and return error
6. Commit transaction

**Key change:** The `email_registry` insert uses `ON CONFLICT (username) DO NOTHING` and checks `rows_affected`. If 0, it means the username already exists globally.

**Error message:** Keep the same error message format for consistency: `"Username '{}' is already taken. Please choose a different username."`

**Verification:** 
- Create a new email → should succeed with 201
- Create the same email again → should fail with 409
- Check database: both `email_registry` and `email_addresses` should have the entry

---

## Step 4: Add `list_registered_usernames` Method

**File:** `database/src/database.rs`

Add a new public method in the `DatabaseClient` impl block (in the "Email Address Operations" section):

```rust
pub async fn list_registered_usernames(&self) -> Result<Vec<EmailRegistryEntry>, Box<dyn Error + Send + Sync>>
```

This method queries `SELECT username, created_at FROM email_registry ORDER BY created_at DESC` and maps rows to `Vec<EmailRegistryEntry>`.

**Verification:** Call the method and verify it returns all registered usernames.

---

## Step 5: Update Module Exports

**File:** `database/src/lib.rs`

Add `EmailRegistryEntry` to the `pub use database::{...}` line.

**Verification:** Check that `EmailRegistryEntry` is accessible from the `http` crate.

---

## Step 6: Add Admin Endpoint in HTTP API

**File:** `http/src/main.rs`

### 6a. Add import
Add `EmailRegistryEntry` to the existing database imports.

### 6b. Add handler function
Add a new async handler function `list_registry`:

```rust
async fn list_registry(State(db): State<Arc<DatabaseClient>>) -> Response {
    info!("Listing registered usernames");
    match db.list_registered_usernames().await {
        Ok(entries) => {
            info!("Found {} registered usernames", entries.len());
            (StatusCode::OK, Json(ApiResponse::success(entries))).into_response()
        }
        Err(e) => {
            error!("Failed to list registered usernames: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Vec<EmailRegistryEntry>>::error(
                    "Internal server error".to_string(),
                )),
            )
                .into_response()
        }
    }
}
```

### 6c. Add route
In the `Router` chain, add:

```rust
.route("/api/admin/registry", get(list_registry))
```

Place it near the other `get` routes for organization.

**Verification:** `curl http://localhost:3000/api/admin/registry` should return registered usernames.

---

## Step 7: Create Migration SQL

**File:** `database/migrations/003_email_registry_backfill.sql`

Create a new migration file to backfill existing usernames from `email_addresses` into `email_registry`:

```sql
-- Migration: Backfill existing email addresses into email_registry
-- Run this after deploying the code that creates the email_registry table

INSERT INTO email_registry (username, created_at)
SELECT DISTINCT split_part(address, '@', 1), created_at
FROM email_addresses
ON CONFLICT (username) DO NOTHING;
```

**Note:** This is a one-time migration. It should be run manually on existing databases.

---

## Step 8: Build and Test

### Build
```bash
cd /Users/vikas/dev/project/TempMail
cargo build
```

### Manual Test Cases
1. **Fresh creation:**
   ```bash
   curl -X POST http://localhost:3000/api/emails -H "Content-Type: application/json" -d '{"username":"testuser"}'
   ```
   Expected: `201 Created` with address

2. **Duplicate creation:**
   ```bash
   curl -X POST http://localhost:3000/api/emails -H "Content-Type: application/json" -d '{"username":"testuser"}'
   ```
   Expected: `409 Conflict` with "already taken" message

3. **Admin registry list:**
   ```bash
   curl http://localhost:3000/api/admin/registry
   ```
   Expected: `200 OK` with list containing `testuser`

4. **Verify cleanup doesn't affect registry:**
   Wait for cleanup scheduler (or manually trigger) and verify `email_registry` still has `testuser` while `email_addresses` may not.

---

## Rollback Plan

If issues arise:
1. Revert code changes in `database/src/database.rs`, `database/src/lib.rs`, `http/src/main.rs`
2. Drop the `email_registry` table: `DROP TABLE email_registry;`
3. Redeploy previous version

No data loss risk since `email_registry` is additive and no existing tables are modified.

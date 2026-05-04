---
date: 2026-05-04
topic: "Fix Email Address Auto-Deletion and Username Reuse"
status: draft
---

## Problem Statement

Email addresses are never fully deleted, preventing username reuse:

1. **Auto-cleanup only deletes emails** — The daily/hourly cleanup jobs only remove old rows from `mail`. `email_addresses`, `quota`, and `user_config` rows persist forever.
2. **Manual delete is incomplete** — The `DELETE /api/emails/:address` endpoint removes the address from `email_addresses` and `quota`, but leaves emails in `mail` and webhook configs in `user_config` orphaned.
3. **Broken foreign key** — `user_config.address` stores the username portion only (e.g., `vikas`), but the FK references `quota(address)` which stores the full email (e.g., `vikas@xelio.me`). This prevents `ON DELETE CASCADE` from working.
4. **No transactions** — `create_email_address` and `delete_email_address` perform multiple writes without transactions, risking partial state on failure.

The result: after any form of deletion, recreating the same username fails with "username taken" because state was left behind.

## Constraints

- Must use existing PostgreSQL schema (with migration for the FK fix)
- Must preserve all existing API behavior
- Must work for both HTTP scheduler (daily) and SMTP background task (hourly)
- Must not break existing webhook or analytics functionality

## Approach

Use **database transactions** to make address creation and deletion atomic, and **fix the `user_config` schema** so cascade cleanup works correctly.

### Why transactions over ad-hoc cleanup

Without transactions, a failure between steps (e.g., quota deleted but address not) leaves the database inconsistent. Wrapping the entire lifecycle in a transaction guarantees all-or-nothing behavior.

### Why fix `user_config.address`

The current FK is logically broken. Fixing it to store the full address enables PostgreSQL's `ON DELETE CASCADE` to automatically clean up webhook configs when a quota row is deleted, reducing manual cleanup logic.

## Architecture

### Before (Broken)

```
SMTP/HTTP → delete_old_mail() → DELETE FROM mail WHERE date < $1
                           → (email_addresses, quota, user_config untouched)

HTTP DELETE → delete_email_address() → DELETE FROM quota
                                     → DELETE FROM email_addresses
                                     → (mail and user_config orphaned)
```

### After (Fixed)

```
SMTP/HTTP → delete_old_mail() → DELETE FROM mail WHERE date < $1
         → delete_old_email_addresses() → BEGIN
                                        → DELETE FROM mail WHERE recipients = $1
                                        → DELETE FROM user_config WHERE mail = $1
                                        → DELETE FROM quota WHERE address = $1
                                        → DELETE FROM email_addresses WHERE address = $1
                                        → COMMIT

HTTP DELETE → delete_email_address_cascade() → BEGIN
                                             → DELETE FROM mail WHERE recipients = $1
                                             → DELETE FROM user_config WHERE mail = $1
                                             → DELETE FROM quota WHERE address = $1
                                             → DELETE FROM email_addresses WHERE address = $1
                                             → COMMIT
```

## Components

### 1. Transaction-Aware Database Methods

Wrap multi-step operations in PostgreSQL transactions using the existing `deadpool-postgres` client.

### 2. `delete_email_address_cascade`

Replaces the current `delete_email_address` method. In a single transaction:
1. Delete all emails where `recipients = $address`
2. Delete `user_config` where `mail = $address`
3. Delete `quota` where `address = $address`
4. Delete `email_addresses` where `address = $address`
5. Return whether the address existed

### 3. `delete_old_email_addresses`

New cleanup method. Finds addresses where `created_at < now() - 1 day` and cascade-deletes each one (along with all associated data).

### 4. `create_email_address` (transactional)

Wrap the existing quota-insert + address-insert in a transaction. If the address insert fails (duplicate), rollback the quota insert to avoid restoring deleted quota rows.

### 5. Schema Fix for `user_config`

- Update `user_config` insertion to store the **full email address** in the `address` column
- Add a migration to update existing rows
- The existing FK `user_config.address -> quota(address)` then works correctly with `ON DELETE CASCADE`

### 6. Updated Cleanup Schedulers

- **HTTP scheduler** (`scheduler.rs`): After `delete_old_mail`, call `delete_old_email_addresses`
- **SMTP background task** (`clear_old_mails.rs`): After `delete_old_mail`, call `delete_old_email_addresses`

## Data Flow

### Address Creation (Fixed)

```
Client POST /api/emails {username: "vikas"}
  → UsernameValidator validates
  → BEGIN TRANSACTION
    → INSERT INTO quota (address, quota_limit, completed) ... ON CONFLICT DO UPDATE
    → INSERT INTO email_addresses (address, created_at) ... ON CONFLICT DO NOTHING
    → IF rows == 0: ROLLBACK, return "already taken"
    → ELSE: COMMIT, return success
  → Increment analytics (fire-and-forget)
```

### Address Deletion (Fixed)

```
Client DELETE /api/emails/vikas%40xelio.me
  → validate_email_format
  → BEGIN TRANSACTION
    → DELETE FROM mail WHERE recipients = 'vikas@xelio.me'
    → DELETE FROM user_config WHERE mail = 'vikas@xelio.me'
    → DELETE FROM quota WHERE address = 'vikas@xelio.me'
    → DELETE FROM email_addresses WHERE address = 'vikas@xelio.me'
    → IF rows == 0: ROLLBACK, return 404
    → ELSE: COMMIT, return 200
```

### Auto-Cleanup (Fixed)

```
Scheduler trigger (daily 2AM / hourly)
  → delete_old_mail: DELETE FROM mail WHERE date < 1_day_ago
  → delete_old_email_addresses:
      SELECT address FROM email_addresses WHERE created_at < 1_day_ago
      FOR EACH address: BEGIN
        → DELETE FROM mail WHERE recipients = address
        → DELETE FROM user_config WHERE mail = address
        → DELETE FROM quota WHERE address = address
        → DELETE FROM email_addresses WHERE address = address
      COMMIT
```

## Error Handling Strategy

- **Transaction failures**: Log the error, return a generic "Internal server error" to the client. The transaction ensures no partial state.
- **Quota check failures during SMTP**: Continue allowing the email (existing behavior — don't lose mail on quota check errors)
- **Cleanup failures**: Log and continue. Don't let one bad address block cleanup of others.
- **Schema migration failure**: Log error but don't crash. The app should handle both old and new schema states during transition.

## Testing Strategy

1. **Unit tests** for `delete_email_address_cascade`:
   - Create address → delete → verify all tables are clean
   - Verify recreation succeeds after deletion
2. **Unit tests** for `create_email_address` transaction:
   - Simulate duplicate key → verify quota insert is rolled back
3. **Property tests** (proptest) for username validation remain unchanged
4. **Integration test** (manual or scripted):
   - Create address, wait (or mock time), trigger cleanup, verify address gone
   - Create address, delete via API, verify recreation succeeds

## Migration

```sql
-- Fix existing user_config rows to store full email in address column
UPDATE user_config
SET address = mail
WHERE address != mail AND mail IS NOT NULL;

-- Note: Future insertions must also use full email for address column
```

## Open Questions

- None. The approach is straightforward and preserves all existing behavior while fixing the data lifecycle.

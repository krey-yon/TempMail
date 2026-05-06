---
date: 2026-05-06
topic: "Permanent Username Registry with Dual Table Storage"
status: validated
---

# Permanent Username Registry with Dual Table Storage

## Problem Statement

The current temp mail system lacks a permanent record of created usernames. The `email_addresses` table (which enforces uniqueness via `ON CONFLICT`) gets cleaned up after 1 day, meaning:
1. There's no historical record of what usernames have ever been created
2. The admin has no way to reference past username usage
3. The uniqueness check is tied to a temporary table

We need to decouple the **permanent username registry** (for admin reference) from the **active email storage** (for user operations).

## Constraints

- Must preserve existing functionality (active emails still expire after 1 day)
- Must atomically prevent duplicate usernames at creation time
- Must not break existing API endpoints
- Must be backward-compatible with existing data
- No code in the design doc — architecture and data flow only

## Approach

**Chosen approach: Add a dedicated `email_registry` table as the global uniqueness gatekeeper.**

I considered three approaches:
1. **Use `email_addresses` with a `permanent` flag** — Rejected because it complicates the cleanup logic and mixes concerns.
2. **Use `quota` table as the registry** — Rejected because `quota` is for limits, not historical tracking, and it already has different semantics.
3. **Add `email_registry` table** — **Chosen.** Clean separation of concerns. Permanent storage for admin, distinct from active user-facing storage.

## Architecture

### Tables

**`email_registry`** (NEW — permanent, admin reference)
- `id` UUID PRIMARY KEY DEFAULT gen_random_uuid()
- `username` TEXT NOT NULL UNIQUE
- `created_at` TEXT NOT NULL DEFAULT (now()::text)
- Purpose: Global registry of all usernames ever created. Never deleted.

**`email_addresses`** (EXISTING — temporary, user-facing)
- `id` UUID PRIMARY KEY DEFAULT gen_random_uuid()
- `address` TEXT NOT NULL UNIQUE
- `created_at` TEXT NOT NULL DEFAULT (now()::text)
- Purpose: Active email addresses. Cleaned up after 1 day by scheduler.

**`mail`** (EXISTING)
- Stores received email content. Unchanged.

**`quota`** (EXISTING)
- Stores per-address email limits. Unchanged.

### Key Design Decision

The `email_registry` table becomes the **single source of truth for username uniqueness**. All creation flows through it first. Once a username is registered, it can never be reused, even after the active `email_addresses` entry expires.

## Components

### Database Layer
- **Schema initialization**: Add `email_registry` table creation to the existing init SQL block
- **`create_email_address`**: Modified to use a transaction that inserts into `email_registry` first, then `quota`, then `email_addresses`
- **`list_registered_usernames`**: New query to fetch all entries from `email_registry`
- **Migration SQL**: Backfill existing usernames from `email_addresses` into `email_registry`

### HTTP API Layer
- **`POST /api/emails`**: Unchanged behavior from caller's perspective — still returns `201 Created` or `409 Conflict`
- **`GET /api/admin/registry`**: New endpoint to list permanently registered usernames (admin reference)

### Scheduler
- **Unchanged.** `delete_old_email_addresses` continues to clean up `email_addresses` only. `email_registry` is intentionally excluded.

## Data Flow

### Email Creation Flow

```
User POST /api/emails {username: "testuser"}
  ↓
Validation (3-32 chars, alphanumeric/hyphen/underscore)
  ↓
db.create_email_address("testuser")
  ↓
BEGIN TRANSACTION
  ├─ INSERT INTO email_registry (username, created_at)
  │   ON CONFLICT (username) DO NOTHING
  │   → If rows_affected == 0: ROLLBACK, return 409 Conflict
  │
  ├─ INSERT INTO quota (address, quota_limit, completed)
  │   ON CONFLICT (address) DO UPDATE SET ...
  │
  ├─ INSERT INTO email_addresses (address, created_at)
  │   ON CONFLICT (address) DO NOTHING
  │   → If rows_affected == 0: ROLLBACK, return 409 Conflict
  │
  └─ COMMIT
  ↓
Return 201 Created with {address, created_at}
```

### Cleanup Flow (unchanged)

```
Scheduler runs daily at 2 AM UTC
  ↓
SELECT address FROM email_addresses WHERE created_at < 1 day ago
  ↓
For each address:
  ├─ DELETE FROM mail WHERE recipients = address
  ├─ DELETE FROM user_config WHERE mail = address
  ├─ DELETE FROM quota WHERE address = address
  └─ DELETE FROM email_addresses WHERE address = address
  ↓
email_registry is NOT touched
```

## Error Handling Strategy

| Scenario | Behavior |
|----------|----------|
| Username already in `email_registry` | Return `409 Conflict` with clear message |
| Username already in `email_addresses` (race condition) | Return `409 Conflict` |
| Database connection failure during transaction | Rollback, return `503 Service Unavailable` |
| Any other DB error | Rollback, return `500 Internal Server Error` |

All operations inside `create_email_address` are wrapped in a single transaction. Partial writes are impossible.

## Testing Strategy

1. **Unit tests for registry insertion**: Verify that duplicate usernames in `email_registry` are rejected
2. **Unit tests for transaction rollback**: Verify that if `email_addresses` insert fails, `email_registry` entry is also rolled back
3. **Integration test for full flow**: Create email, verify it exists in both tables, attempt duplicate creation and verify 409
4. **Backward compatibility test**: Verify existing `email_addresses` data continues to work
5. **Cleanup test**: Verify `delete_old_email_addresses` does NOT delete from `email_registry`

## Open Questions

- Should we add an admin-only authentication mechanism for `/api/admin/registry`? For now, no — the endpoint is lightweight and the project doesn't have an auth system. Can be added later if needed.
- Should existing `email_addresses` entries be backfilled into `email_registry` on deploy? Yes, via a migration script.

## Migration

A one-time SQL migration should backfill existing usernames:

```sql
INSERT INTO email_registry (username, created_at)
SELECT DISTINCT split_part(address, '@', 1), created_at
FROM email_addresses
ON CONFLICT (username) DO NOTHING;
```

This ensures existing active usernames are preserved in the registry and won't cause conflicts when their `email_addresses` entries expire.

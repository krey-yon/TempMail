# Implementation Plan: Fix Email Address Auto-Deletion and Username Reuse

## Overview

Fix the bug where email addresses are never fully deleted, preventing username reuse after both auto-cleanup and manual deletion.

## Files to Modify

### 1. `database/src/database.rs`
**Changes:**
- Modify `delete_email_address` to use a transaction and cascade-delete all associated data (mail, user_config, quota, email_addresses)
- Add new method `delete_old_email_addresses` that finds addresses older than 1 day and cascade-deletes them
- Modify `create_email_address` to wrap quota + email_addresses inserts in a transaction (rollback on duplicate)
- Fix `set_webhook` to store the full email address in `user_config.address` instead of just the username part

### 2. `http/src/scheduler.rs`
**Changes:**
- After `delete_old_mail`, call the new `db.delete_old_email_addresses().await`

### 3. `smtp/src/lib.rs` or `database/src/clear_old_mails.rs`
**Changes:**
- After `delete_old_mail`, call the new `db.delete_old_email_addresses().await`

### 4. `database/migrations/002_fix_user_config_address.sql`
**New file:**
- Migration to update existing `user_config` rows to store full email in `address` column

## Implementation Order

1. **Migration first** — Run `002_fix_user_config_address.sql` to fix existing data
2. **Update `database.rs`** — Core logic changes (methods must be updated before callers)
3. **Update `webhooks.rs`** — Fix `set_webhook` insertion logic
4. **Update `scheduler.rs`** — HTTP cleanup scheduler
5. **Update `clear_old_mails.rs`** — SMTP cleanup task
6. **Build & test**

## Detailed Changes

### `database/src/database.rs`

#### `delete_email_address` → `delete_email_address_cascade`

Replace with transaction-based implementation:

```rust
pub async fn delete_email_address(&self, address: &str) -> Result<bool, Box<dyn Error + Send + Sync>> {
    let mut client = self.pool.get().await?;
    let tx = client.transaction().await?;
    
    // Delete mails
    let _ = tx.execute("DELETE FROM mail WHERE recipients = $1", &[&address]).await;
    
    // Delete webhook config
    let _ = tx.execute("DELETE FROM user_config WHERE mail = $1", &[&address]).await;
    
    // Delete quota
    let _ = tx.execute("DELETE FROM quota WHERE address = $1", &[&address]).await;
    
    // Delete email address
    let rows = tx.execute("DELETE FROM email_addresses WHERE address = $1", &[&address]).await?;
    
    tx.commit().await?;
    Ok(rows > 0)
}
```

#### New method: `delete_old_email_addresses`

```rust
pub async fn delete_old_email_addresses(&self) -> Result<u64, Box<dyn Error + Send + Sync>> {
    let mut client = self.pool.get().await?;
    let tx = client.transaction().await?;
    
    let now = chrono::Utc::now();
    let a_day_ago = now - chrono::Duration::days(1);
    let a_day_ago = a_day_ago.format("%Y-%m-%d %H:%M:%S%.3f").to_string();
    
    // Find old addresses
    let rows = tx.query("SELECT address FROM email_addresses WHERE created_at < $1", &[&a_day_ago]).await?;
    let mut deleted_count = 0;
    
    for row in rows {
        let address: String = row.get(0);
        
        let _ = tx.execute("DELETE FROM mail WHERE recipients = $1", &[&address]).await;
        let _ = tx.execute("DELETE FROM user_config WHERE mail = $1", &[&address]).await;
        let _ = tx.execute("DELETE FROM quota WHERE address = $1", &[&address]).await;
        let result = tx.execute("DELETE FROM email_addresses WHERE address = $1", &[&address]).await?;
        
        if result > 0 {
            deleted_count += 1;
        }
    }
    
    tx.commit().await?;
    info!("Deleted {} old email addresses", deleted_count);
    Ok(deleted_count)
}
```

#### `create_email_address` (transactional)

Wrap the two inserts in a transaction. If the email_addresses insert returns 0 (already exists), rollback the quota update.

```rust
pub async fn create_email_address(&self, username: &str) -> Result<EmailAddress, Box<dyn Error + Send + Sync>> {
    let domain = env::var("MAIL_DOMAIN").unwrap_or_else(|_| "xelio.me".to_string());
    let address = format!("{}@{}", username, domain);
    let created_at = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string();
    let default_quota_limit: i32 = 1000;
    
    let mut client = self.pool.get().await?;
    let tx = client.transaction().await?;
    
    // Insert or update quota
    let quota_sql = "INSERT INTO quota (address, quota_limit, completed) VALUES ($1, $2, 0) ON CONFLICT (address) DO UPDATE SET quota_limit = EXCLUDED.quota_limit, completed = 0";
    tx.execute(quota_sql, &[&address, &default_quota_limit]).await?;
    
    // Insert email address
    let addr_sql = "INSERT INTO email_addresses (address, created_at) VALUES ($1, $2) ON CONFLICT (address) DO NOTHING";
    let rows = tx.execute(addr_sql, &[&address, &created_at]).await?;
    
    if rows == 0 {
        // Already exists — rollback the quota update
        tx.rollback().await?;
        return Err(format!("Username '{}' is already taken. Please choose a different username.", username).into());
    }
    
    tx.commit().await?;
    Ok(EmailAddress { address, created_at: Some(created_at) })
}
```

### `database/src/webhooks.rs`

#### Fix `set_webhook` to store full address

Change:
```rust
let address = mail.split('@').next().unwrap_or(mail);
```
to:
```rust
let address = mail; // store full email address
```

### `http/src/scheduler.rs`

Add call after `delete_old_mail`:
```rust
match db.delete_old_email_addresses().await {
    Ok(count) => info!("Deleted {} old email addresses", count),
    Err(e) => error!("Failed to delete old email addresses: {}", e),
}
```

### `database/src/clear_old_mails.rs`

Add call after `delete_old_mail`:
```rust
match db.delete_old_email_addresses().await {
    Ok(count) => info!("Deleted {} old email addresses", count),
    Err(e) => error!("Failed to delete old email addresses: {}", e),
}
```

### `database/migrations/002_fix_user_config_address.sql`

```sql
-- Fix existing user_config rows to store full email address
-- This makes the FK user_config.address -> quota(address) actually work
UPDATE user_config
SET address = mail
WHERE address != mail;
```

## Testing Steps

1. **Build the project**: `cargo build --release`
2. **Run migration**: `psql ... -f database/migrations/002_fix_user_config_address.sql`
3. **Test manual deletion and reuse**:
   ```bash
   curl -X POST http://localhost:3000/api/emails -H "Content-Type: application/json" -d '{"username":"testreuse"}'
   curl -X DELETE "http://localhost:3000/api/emails/testreuse%40xelio.me"
   curl -X POST http://localhost:3000/api/emails -H "Content-Type: application/json" -d '{"username":"testreuse"}'
   # Should succeed (201), not fail with 409
   ```
4. **Test auto-cleanup**: Create an address, manually backdate its `created_at` in the DB to > 1 day ago, trigger cleanup, verify address is gone and username is reusable.
5. **Test transaction rollback**: Try creating an already-existing username. Verify quota is NOT incremented/restored (check `quota.completed` and `quota.quota_limit` remain unchanged).

## Success Criteria

- [ ] Creating an address, deleting it via API, and recreating it with the same username succeeds
- [ ] Addresses older than 1 day are automatically deleted along with all their emails and webhook configs
- [ ] No orphaned rows remain in `mail`, `quota`, or `user_config` after address deletion
- [ ] The `user_config.address` column correctly stores full email addresses
- [ ] Existing tests continue to pass (`cargo test`)

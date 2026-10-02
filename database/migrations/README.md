# Database Migrations

`004_reusable_usernames.sql` runs automatically during database initialization
after schema creation. It preserves the best available lifetime creation count
before releasing historical username reservations. It is safe to run again.
Deploy and restart both HTTP and SMTP together; older binaries still implement
permanent reservations and nontransactional creation counting.

## Running Migrations

### Option 1: Direct SQL (Recommended for production)

Run the migration SQL file directly on your PostgreSQL database:

```bash
psql -h $DB_HOST -U $DB_USER -d $DB_NAME -f 001_uuid_migration.sql
```

### Option 2: Drop and Recreate (Loses all data)

If you want a fresh start, you can drop and recreate tables. **WARNING: This deletes all existing data.**

```bash
psql -h $DB_HOST -U $DB_USER -d $DB_NAME -c "DROP TABLE IF EXISTS mail, quota, user_config, email_addresses, analytics CASCADE;"
```

Then restart the application - it will recreate tables with UUID IDs.

## Migration Checklist

After running migration, verify:

1. ✅ All tables have UUID primary keys
2. ✅ Analytics table exists
3. ✅ Restart both HTTP and SMTP services
4. ✅ Test creating a new email address

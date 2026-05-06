-- Migration: Backfill existing email addresses into email_registry
-- Run this after deploying the code that creates the email_registry table

INSERT INTO email_registry (username, created_at)
SELECT DISTINCT split_part(address, '@', 1), created_at
FROM email_addresses
ON CONFLICT (username) DO NOTHING;

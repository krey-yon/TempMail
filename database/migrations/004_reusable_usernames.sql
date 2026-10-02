-- Preserve the best available historical creation count before releasing names.
-- Re-running this migration never adds active records to the existing count.
INSERT INTO analytics (event_type, event_count, last_updated)
SELECT 'email_address_created', GREATEST(
    (SELECT COUNT(*) FROM email_registry),
    (SELECT COUNT(*) FROM email_addresses)
), now()::text
ON CONFLICT (event_type) DO UPDATE SET
    event_count = GREATEST(analytics.event_count, EXCLUDED.event_count);

-- The registry now describes current reservations, not permanent ownership.
DELETE FROM email_registry r
WHERE NOT EXISTS (
    SELECT 1 FROM email_addresses ea
    WHERE split_part(ea.address, '@', 1) = r.username
);

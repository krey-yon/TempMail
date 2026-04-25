-- Migration: UUID IDs and Analytics Table
-- Run this on your PostgreSQL database to migrate from BIGSERIAL to UUID

-- Enable UUID generation
CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ============================================
-- Migrate mail table
-- ============================================
ALTER TABLE mail ADD COLUMN IF NOT EXISTS id_new UUID;
UPDATE mail SET id_new = gen_random_uuid() WHERE id_new IS NULL;
ALTER TABLE mail DROP COLUMN id;
ALTER TABLE mail RENAME COLUMN id_new TO id;
ALTER TABLE mail ALTER COLUMN id SET DEFAULT gen_random_uuid();
ALTER TABLE mail ADD PRIMARY KEY USING INDEX (SELECT WHERE id IS NOT NULL); -- Recreate PK

-- ============================================
-- Migrate quota table
-- ============================================
ALTER TABLE quota ADD COLUMN IF NOT EXISTS id_new UUID;
UPDATE quota SET id_new = gen_random_uuid() WHERE id_new IS NULL;
ALTER TABLE quota DROP COLUMN id;
ALTER TABLE quota RENAME COLUMN id_new TO id;
ALTER TABLE quota ALTER COLUMN id SET DEFAULT gen_random_uuid();

-- ============================================
-- Migrate user_config table
-- ============================================
ALTER TABLE user_config ADD COLUMN IF NOT EXISTS id_new UUID;
UPDATE user_config SET id_new = gen_random_uuid() WHERE id_new IS NULL;
ALTER TABLE user_config DROP COLUMN id;
ALTER TABLE user_config RENAME COLUMN id_new TO id;
ALTER TABLE user_config ALTER COLUMN id SET DEFAULT gen_random_uuid();

-- ============================================
-- Migrate email_addresses table
-- ============================================
ALTER TABLE email_addresses ADD COLUMN IF NOT EXISTS id_new UUID;
UPDATE email_addresses SET id_new = gen_random_uuid() WHERE id_new IS NULL;
ALTER TABLE email_addresses DROP COLUMN id;
ALTER TABLE email_addresses RENAME COLUMN id_new TO id;
ALTER TABLE email_addresses ALTER COLUMN id SET DEFAULT gen_random_uuid();

-- ============================================
-- Create analytics table
-- ============================================
CREATE TABLE IF NOT EXISTS analytics (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    event_type TEXT NOT NULL UNIQUE,
    event_count BIGINT NOT NULL DEFAULT 0,
    last_updated TEXT NOT NULL DEFAULT (now()::text)
);
CREATE INDEX IF NOT EXISTS analytics_event_type_idx ON analytics(event_type);

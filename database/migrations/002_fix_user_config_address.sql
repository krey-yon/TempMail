-- Migration: Fix user_config.address to store full email address
-- This makes the FK user_config.address -> quota(address) actually work
-- Run after deploying the code fix that stores full emails in new inserts

UPDATE user_config
SET address = mail
WHERE address != mail;

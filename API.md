# TempMail API Specification

## Overview

TempMail is a temporary email service with two components:
- **HTTP API** (port 3000) - REST API for email management
- **SMTP Server** (port 25) - Email receiving server

Base URL:
- Production: `https://api.xelio.me` (or `https://void.kreyon.in`)
- Local: `http://localhost:3000`

---

## Architecture

### Services

| Service | Port | Purpose |
|---------|------|---------|
| HTTP API | 3000 | REST API for email address and email management |
| SMTP Server | 25 | Receives incoming emails |

### Database Schema

**Tables:**

1. `email_addresses` - Stores created email addresses
   - `id` (BIGSERIAL PRIMARY KEY)
   - `address` (TEXT, UNIQUE) - Full email address like `user@xelio.me`
   - `created_at` (TEXT) - Timestamp

2. `mail` - Stores received emails
   - `id` (BIGSERIAL PRIMARY KEY)
   - `date` (TEXT) - Timestamp
   - `sender` (TEXT) - Sender email address
   - `recipients` (TEXT) - Recipient email address
   - `data` (TEXT) - Raw RFC 5322 email content

3. `quota` - Email limits per address
   - `id` (SERIAL PRIMARY KEY)
   - `address` (TEXT, UNIQUE) - Email address
   - `quota_limit` (INTEGER) - Max emails allowed (default: 1000)
   - `completed` (INTEGER) - Current email count

4. `user_config` - User settings with webhook URLs
   - `id` (SERIAL PRIMARY KEY)
   - `mail` (TEXT, UNIQUE) - Email address
   - `address` (TEXT) - Username part
   - `web_hook_address` (TEXT) - Webhook URL for notifications

---

## HTTP API Endpoints

### Health Check

```
GET /
```

**Response (200 OK):**
```json
{
    "success": true,
    "data": "Temp Mail HTTP API is running",
    "error": null
}
```

---

### Create Email Address

Create a new temporary email address.

```
POST /api/emails
Content-Type: application/json
```

**Request Body:**
```json
{
    "username": "testuser"
}
```

| Field | Type | Validation | Description |
|-------|------|------------|-------------|
| username | string | 3-32 chars, alphanumeric/hyphen/underscore, lowercase | Unique username for the email address |

**Success Response (201 Created):**
```json
{
    "success": true,
    "data": {
        "address": "testuser@xelio.me",
        "created_at": "2026-04-25 09:36:38.040"
    },
    "error": null
}
```

**Error Response (400 Bad Request):**
```json
{
    "success": false,
    "data": null,
    "error": "Username must be 3-32 characters"
}
```

**Error Response (409 Conflict):**
```json
{
    "success": false,
    "data": null,
    "error": "Email address already exists"
}
```

---

### List All Email Addresses

Get a list of all created email addresses with email count.

```
GET /api/emails
```

**Success Response (200 OK):**
```json
{
    "success": true,
    "data": [
        {
            "address": "testuser@xelio.me",
            "created_at": "2026-04-25 09:36:38.040",
            "email_count": 5
        }
    ],
    "error": null
}
```

---

### Delete Email Address

Delete an email address and all its emails.

```
DELETE /api/emails/:address
```

| Parameter | Type | Description |
|-----------|------|-------------|
| address | string | Full email address (URL encoded, e.g., `testuser%40xelio.me`) |

**Success Response (200 OK):**
```json
{
    "success": true,
    "data": {
        "message": "Email address deleted successfully",
        "address": "testuser@xelio.me"
    },
    "error": null
}
```

**Error Response (404 Not Found):**
```json
{
    "success": false,
    "data": null,
    "error": "Email address not found"
}
```

---

### Get Emails for Address

Retrieve all emails sent to an address.

```
GET /api/emails/:address
```

| Parameter | Type | Description |
|-----------|------|-------------|
| address | string | Full email address (URL encoded) |

**Success Response (200 OK):**
```json
{
    "success": true,
    "data": [
        {
            "id": 1,
            "date": "2026-04-25 09:40:00.000",
            "sender": "<sender@example.com>",
            "recipients": "<testuser@xelio.me>",
            "data": "From: sender@example.com\r\nTo: testuser@xelio.me\r\nSubject: Hello\r\n\r\nHello World!"
        }
    ],
    "error": null
}
```

| Field | Type | Description |
|-------|------|-------------|
| id | integer | Unique email ID |
| date | string | Timestamp when email was received |
| sender | string | Sender's email address (with `<>`) |
| recipients | string | Recipient's email address (with `<>`) |
| data | string | Raw email content (RFC 5322 format) |

---

### Get Single Email

Retrieve a specific email by ID.

```
GET /api/emails/:address/:id
```

| Parameter | Type | Description |
|-----------|------|-------------|
| address | string | Full email address (URL encoded) |
| id | integer | Email ID |

**Success Response (200 OK):**
```json
{
    "success": true,
    "data": {
        "id": 1,
        "date": "2026-04-25 09:40:00.000",
        "sender": "<sender@example.com>",
        "recipients": "<testuser@xelio.me>",
        "data": "From: sender@example.com\r\nTo: testuser@xelio.me\r\nSubject: Hello\r\n\r\nHello World!"
    },
    "error": null
}
```

**Error Response (404 Not Found):**
```json
{
    "success": false,
    "data": null,
    "error": "Email not found"
}
```

---

### Delete Single Email

Delete a specific email by ID.

```
DELETE /api/emails/:address/:id
```

| Parameter | Type | Description |
|-----------|------|-------------|
| address | string | Full email address (URL encoded) |
| id | integer | Email ID |

**Success Response (200 OK):**
```json
{
    "success": true,
    "data": null,
    "error": null
}
```

**Error Response (404 Not Found):**
```json
{
    "success": false,
    "data": null,
    "error": "Email not found"
}
```

---

## SMTP Server

The SMTP server receives emails sent to `@<MAIL_DOMAIN>` addresses.

### Connection

```
SMTP Server: 143.110.178.78:25
```

### SMTP Commands

Standard SMTP protocol:

```
EHLO <hostname>
MAIL FROM:<sender@example.com>
RCPT TO:<recipient@xelio.me>
DATA
<email content>
.
QUIT
```

### SMTP Response Codes

| Code | Meaning |
|------|---------|
| 220 | Service ready |
| 250 | Requested mail action okay, completed |
| 354 | Start mail input (send email content) |
| 421 | Service not available, closing transmission channel |
| 500 | Syntax error, command unrecognized |
| 501 | Syntax error in parameters or arguments |
| 550 | Requested action not taken (mailbox unavailable) |
| 452 | Requested action aborted (insufficient system storage) |
| 552 | Requested mail action aborted (message size exceeds limit) |
| 554 | Transaction failed |

### Limits

- **Max email size:** 10 MB (10,485,760 bytes)
- **Max recipients per email:** 100
- **Connection timeout:** 30 seconds
- **Max transaction time:** 5 minutes

### Email Validation

The SMTP server validates email addresses:
- Must contain `@`
- Cannot contain `..` (double dots)
- Must be less than 254 characters

---

## Webhook Notifications

When an email is received and the address has a webhook configured, a POST request is sent to the webhook URL.

### Payload

```json
{
    "version": 1,
    "otp": "123456",
    "mail": "testuser@xelio.me"
}
```

| Field | Type | Description |
|-------|------|-------------|
| version | integer | Payload version (always 1) |
| otp | string | 6-digit OTP extracted from email content (empty if none found) |
| mail | string | Recipient email address |

### OTP Extraction

The following patterns are searched in email content:
- `otp: 123456`
- `verification: 123456`
- `code: 123456`
- `passcode: 123456`
- Any standalone 6-digit number

### Webhook Configuration

Webhooks are stored in the `user_config` table. They can be set when creating email addresses via the SMTP server's configuration system (not via HTTP API currently).

---

## Background Jobs

### Email Cleanup

- **Schedule:** Daily at 2:00 AM UTC
- **Action:** Deletes all emails older than 7 days
- **Runs in:** HTTP API service

### Old Mail Cleanup (SMTP)

- **Schedule:** Every hour
- **Action:** Deletes emails older than 7 days
- **Runs in:** SMTP service

---

## Quota System

Each email address has a quota (default: 1000 emails).

- When quota is reached, new emails are rejected (silently dropped)
- Quota is tracked in the `quota` table
- Default quota limit: 1000 per address

---

## Response Format

All HTTP API responses follow this structure:

```json
{
    "success": true|false,
    "data": <result_data_or_null>,
    "error": <error_message_or_null>
}
```

| Field | Type | Description |
|-------|------|-------------|
| success | boolean | Whether the request succeeded |
| data | any | Response data (null on error) |
| error | string | Error message (null on success) |

---

## Rate Limiting

- **Limit:** 5 requests per hour per IP address
- **Status:** Currently disabled for local testing
- **Implementation:** tower-governor with SmartIpKeyExtractor

Note: Rate limiting requires proxy headers (X-Forwarded-For, X-Real-IP) in production.

---

## CORS

The API allows requests from:
- `https://void.kreyon.in`

---

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| DB_HOST | (required) | PostgreSQL database host |
| DB_USER | (required) | PostgreSQL database user |
| DB_PASSWORD | (required) | PostgreSQL database password |
| DB_NAME | (required) | PostgreSQL database name |
| DB_PORT | 5432 | PostgreSQL database port |
| DB_SSLMODE | require | PostgreSQL SSL mode |
| MAIL_DOMAIN | mail.kreyon.in | Domain for email addresses (xelio.me in production) |
| SMTP_PORT | 25 | SMTP server port |
| RUST_LOG | info | Logging level |

---

## Email Address Format

```
{username}@{MAIL_DOMAIN}
```

Examples:
- `testuser@xelio.me`
- `john-doe@xelio.me`
- `user_123@xelio.me`

Username rules:
- 3-32 characters
- Only lowercase letters (a-z), numbers (0-9), hyphens (-), and underscores (_)
- Automatically converted to lowercase

---

## Email Content Format

The `data` field contains raw email content in RFC 5322 format:

```
From: sender@example.com
To: user@xelio.me
Subject: Your Code
MIME-Version: 1.0
Content-Type: text/plain; charset=utf-8

Your verification code is 123456.
```

To parse:
1. Split by `\r\n\r\n` to separate headers from body
2. Parse headers individually
3. Use a library like `mailparser` for full parsing

---

## Usage Examples

### JavaScript/TypeScript

```javascript
// Create email address
const res = await fetch('http://localhost:3000/api/emails', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username: 'myuser' })
});
const { data } = await res.json();
// data.address = "myuser@xelio.me"

// Fetch emails
const emails = await fetch('http://localhost:3000/api/emails/myuser%40xelio.me');
const { data: emailList } = await emails.json();

// Delete email
await fetch('http://localhost:3000/api/emails/myuser%40xelio.me/123', {
    method: 'DELETE'
});
```

### cURL

```bash
# Create email
curl -X POST http://localhost:3000/api/emails \
  -H "Content-Type: application/json" \
  -d '{"username": "testuser"}'

# List emails
curl http://localhost:3000/api/emails

# Get emails for address
curl http://localhost:3000/api/emails/testuser%40xelio.me

# Delete email address
curl -X DELETE "http://localhost:3000/api/emails/testuser%40xelio.me"
```

### Python

```python
import requests

# Create email
res = requests.post('http://localhost:3000/api/emails', json={'username': 'testuser'})
address = res.json()['data']['address']

# Get emails
emails = requests.get(f'http://localhost:3000/api/emails/{address}')
```

---

## Troubleshooting

### SMTP Connection Refused
- Verify port 25 is exposed in Coolify
- Check if container is using `network_mode: host`
- Verify firewall allows outbound port 25

### Database Connection Errors
- Check DB_HOST, DB_USER, DB_PASSWORD environment variables
- Verify Neon PostgreSQL is accessible
- Check SSL mode (requires `require` for Neon)

### Emails Not Received
- Check if sender MX records point to your SMTP server
- Verify DNS A record for SMTP server IP
- Check quota limits (emails silently dropped when exceeded)

### Webhook Not Firing
- Verify webhook URL is correctly stored in `user_config` table
- Check SMTP logs for webhook delivery errors
- Ensure webhook endpoint accepts POST requests
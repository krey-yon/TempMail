# TempMail

A temporary email service with SMTP server for receiving emails and HTTP API for management.

## Features

- **Temporary Email Addresses**: Create disposable email addresses instantly
- **SMTP Server**: Receive emails on port 25
- **HTTP API**: Manage email addresses and view emails on port 3000
- **Webhooks**: Get notified when emails arrive
- **Rate Limiting**: DDoS protection with 100 requests/second per IP
- **Analytics**: Track usage statistics via `/api/stats`
- **UUID-based IDs**: Secure, non-sequential identifiers for all entities
- **Automatic Cleanup**: Emails deleted after 1 day

## Architecture

| Service | Port | Purpose |
|---------|------|---------|
| HTTP API | 3000 | REST API for email management |
| SMTP Server | 25 | Receives incoming emails |

## Quick Start

### Prerequisites

- Rust (latest stable)
- PostgreSQL database
- `.env` file with database credentials

### Environment Variables

```env
DB_HOST=localhost
DB_USER=postgres
DB_PASSWORD=your_password
DB_NAME=tempmail
DB_PORT=5432
MAIL_DOMAIN=mail.kreyon.in
SMTP_PORT=25
RUST_LOG=info
```

### Build and Run

```bash
# Build all services
cargo build --release

# Run HTTP API
cargo run --package http

# Run SMTP server (in separate terminal)
cargo run --package smtp
```

## API Endpoints

### Health Check
```
GET /
```

### Create Email Address
```
POST /api/emails
Content-Type: application/json

{"username": "myuser"}
```

### List All Addresses
```
GET /api/emails
```

### Get Emails for Address
```
GET /api/emails/:address
```

### Delete Email Address
```
DELETE /api/emails/:address
```

### Get Single Email
```
GET /api/emails/:address/:id
```

### Delete Single Email
```
DELETE /api/emails/:address/:id
```

### Get Statistics (Analytics)
```
GET /api/stats
```

Response:
```json
{
    "success": true,
    "data": {
        "total_addresses": 100,
        "total_emails": 500,
        "total_webhooks": 25,
        "events": [
            {"event_type": "emails_received", "event_count": 450, "last_updated": "2026-04-26 10:00:00"},
            {"event_type": "email_address_created", "event_count": 100, "last_updated": "2026-04-26 10:00:00"}
        ]
    }
}
```

## SMTP Usage

Connect to port 25 and send emails:

```
EHLO localhost
MAIL FROM:<sender@example.com>
RCPT TO:<recipient@mail.kreyon.in>
DATA
From: sender@example.com
To: recipient@mail.kreyon.in
Subject: Hello

Hello World!
.
QUIT
```

## Rate Limiting

- **Limit**: 100 requests per second per IP
- **Burst**: Up to 150 requests
- **Implementation**: tower-governor with SmartIpKeyExtractor

Requires proxy headers (`X-Forwarded-For`, `X-Real-IP`) when behind a reverse proxy.

## Cleanup Schedule

| Task | Schedule | Action |
|------|----------|--------|
| Email Cleanup (HTTP) | Daily at 2:00 AM UTC | Deletes emails older than 1 day |
| Email Cleanup (SMTP) | Every hour | Deletes emails older than 1 day |

## Quota System

Each email address has a default quota of 1000 emails. When exceeded, new emails are silently dropped.

## Response Format

All API responses follow this structure:

```json
{
    "success": true,
    "data": <result>,
    "error": null
}
```

## Database Schema

### Tables

- `mail` - Stores received emails (UUID primary key)
- `email_addresses` - Created email addresses (UUID primary key)
- `quota` - Email limits per address (UUID primary key)
- `user_config` - User settings and webhooks (UUID primary key)
- `analytics` - Usage tracking counters

## Project Structure

```
TempMail/
├── database/          # Database client and schema
├── http/              # HTTP API server
├── smtp/              # SMTP server
└── README.md
```

## License

MIT

# Temp Mail

A self-hosted disposable email service built in Rust. It accepts inbound emails via a standards-compliant SMTP server, persists them to PostgreSQL, and exposes them through a simple REST API.

## Architecture

The workspace has three crates:

```
temp-mail/
├── smtp/       # Async SMTP server (port 25)
├── http/       # REST API server (port 3000)
└── database/   # Shared PostgreSQL client and schema
```

```
SMTP Client → smtp (port 25) → database → http (port 3000) → API Consumer
```

**SMTP state machine:** `Initial → Greeted → AwaitingRecipient → AwaitingData → DataReceived`

## Features

- Full SMTP handshake: `EHLO`, `HELO`, `MAIL FROM`, `RCPT TO`, `DATA`, `QUIT`, `RSET`, `AUTH`
- Up to 100 recipients per message
- 10 MB message size limit
- 30-second read timeout per command, 5-minute connection timeout
- Emails older than 7 days are pruned automatically
- REST API to fetch and delete emails by recipient address
- Auto-creates database schema on startup (no migrations needed)

## Requirements

- Rust (edition 2024)
- PostgreSQL

## Configuration

Create a `.env` file in the project root:

```env
DB_HOST=localhost
DB_USER=postgres
DB_PASSWORD=secret
DB_NAME=tempmail
```

## Running

```bash
# Build everything
cargo build --release

# Start the SMTP server (requires port 25, may need sudo)
cargo run -p smtp

# Start the HTTP API server
cargo run -p http
```

Both servers connect to the same PostgreSQL database and can run independently.

## Database Schema

```sql
mail (id, date, sender, recipients, data)
quota (id, address, quota_limit, completed)
user_config (id, mail, address, web_hook_address)
```

Indexes are created on `date`, `recipients`, and `(date, recipients)` for fast lookups.

## REST API

### List emails for an address

```
GET /api/emails/:address
```

```json
{
  "success": true,
  "data": [
    {
      "id": 1,
      "date": "2026-02-23 12:00:00.000",
      "sender": "someone@example.com",
      "recipients": "you@mail.jasscodes.in",
      "data": "..."
    }
  ],
  "error": null
}
```

### Get a single email

```
GET /api/emails/:address/:id
```

### Delete an email

```
DELETE /api/emails/:address/:id
```

Address ownership is verified before returning or deleting an email — you can only access emails sent to the address in the URL.

## SMTP Session Example

```
S: 220 Temp Mail Service Ready
C: EHLO sender.example.com
S: 250-mail.jasscodes.in greets mail.jasscodes.in
   250-SIZE 10485760
   250 8BITMIME
C: MAIL FROM:alice@example.com
S: 250 Ok
C: RCPT TO:inbox@mail.jasscodes.in
S: 250 Ok
C: DATA
S: 354 End data with <CR><LF>.<CR><LF>
C: Subject: Hello
   ...
   .
S: 250 Ok
C: QUIT
S: 221 Goodbye
```

## Project Structure

| File | Responsibility |
|---|---|
| `smtp/src/main.rs` | Loads `.env`, binds to `0.0.0.0:25`, starts SMTP server |
| `smtp/src/lib.rs` | `start_smtp_server()`, `is_email_valid()` |
| `smtp/src/server.rs` | TCP connection handler, read/write loop |
| `smtp/src/smtp.rs` | SMTP state machine (`HandleCurrentState`) |
| `smtp/src/types.rs` | `Email`, `CurrentStates`, `SMTPResult` |
| `smtp/src/errors.rs` | `SmtpErrorCode`, `SmtpResponseError` |
| `http/src/main.rs` | Axum router, REST handlers |
| `database/src/database.rs` | `DatabaseClient`, schema init, query methods |

## Tech Stack

- **[Tokio](https://tokio.rs/)** — async runtime
- **[Axum](https://github.com/tokio-rs/axum)** — HTTP framework
- **[tokio-postgres](https://docs.rs/tokio-postgres)** — async PostgreSQL driver
- **[tracing](https://docs.rs/tracing)** — structured logging
- **[chrono](https://docs.rs/chrono)** — date/time handling

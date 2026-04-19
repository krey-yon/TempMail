# TempMail API Specification

Base URL: `https://void.kreyon.in` (when deployed) or `http://localhost:3000` (local)

## Create Email Address

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
| username | string | 3-32 chars, alphanumeric/hyphen/underscore | Unique username for the email address |

**Success Response (201 Created):**
```json
{
    "success": true,
    "data": {
        "address": "testuser@mail.kreyon.in",
        "created_at": "2026-04-20 12:00:00.000"
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

## List All Email Addresses

Get a list of all created email addresses.

```
GET /api/emails
```

**Success Response (200 OK):**
```json
{
    "success": true,
    "data": [
        {
            "address": "testuser@mail.kreyon.in",
            "created_at": "2026-04-20 12:00:00.000",
            "email_count": 5
        }
    ],
    "error": null
}
```

---

## Delete Email Address

Delete an email address and all its emails.

```
DELETE /api/emails/:address
```

| Parameter | Type | Description |
|-----------|------|-------------|
| address | string | Full email address (URL encoded) |

**Success Response (200 OK):**
```json
{
    "success": true,
    "data": {
        "message": "Email address deleted successfully",
        "address": "testuser@mail.kreyon.in"
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

## Get Emails for Address

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
            "date": "2026-04-20 12:05:00.000",
            "sender": "<sender@example.com>",
            "recipients": "<testuser@mail.kreyon.in>",
            "data": "From: sender@example.com\r\nTo: testuser@mail.kreyon.in\r\nSubject: Hello\r\n\r\nHello World!"
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

## Get Single Email

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
        "date": "2026-04-20 12:05:00.000",
        "sender": "<sender@example.com>",
        "recipients": "<testuser@mail.kreyon.in>",
        "data": "From: sender@example.com\r\nTo: testuser@mail.kreyon.in\r\nSubject: Hello\r\n\r\nHello World!"
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

## Delete Single Email

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

## Health Check

```
GET /
```

**Success Response (200 OK):**
```json
{
    "success": true,
    "data": "Temp Mail HTTP API is running",
    "error": null
}
```

---

## Rate Limiting

The API is rate limited to **5 requests per hour per IP address**.

If you exceed this limit, you'll receive a `429 Too Many Requests` response.

---

## Response Format

All responses follow this structure:

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

## Usage Example

```javascript
// Create email address
const res = await fetch('http://localhost:3000/api/emails', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username: 'myuser' })
});
const { data } = await res.json();
// data.address = "myuser@mail.kreyon.in"

// Fetch emails
const emails = await fetch('http://localhost:3000/api/emails/myuser@mail.kreyon.in');
const { data: emailList } = await emails.json();
```

---

## Email Address Format

All email addresses follow the format:
```
{username}@mail.kreyon.in
```

Where `username` is 3-32 characters and contains only:
- Letters (a-z, A-Z)
- Numbers (0-9)
- Hyphens (-)
- Underscores (_)

Usernames are automatically converted to lowercase.

---

## Email Content Format

The `data` field in email responses contains raw email content in RFC 5322 format:

```
From: sender@example.com
To: user@mail.kreyon.in
Subject: Your Code
MIME-Version: 1.0
Content-Type: text/plain; charset=utf-8

Your verification code is 123456.
```

To parse the email content, you can:
1. Split by `\r\n\r\n` to separate headers from body
2. Parse headers individually
3. Use a library like `mailparser` for full parsing

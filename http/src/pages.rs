//! Static server-rendered pages that are part of operating the service.
//!
//! These exist so that a provider, host, or abuse reporter has somewhere to
//! land: an abuse contact page and a `security.txt`. Both are dependency-free
//! HTML/text and never touch the database.

use axum::{
    http::header,
    response::{Html, IntoResponse, Response},
};

const ABUSE_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Abuse — Xelio</title>
<style>
:root { color-scheme: dark; }
* { box-sizing: border-box; }
body {
  margin: 0 auto; max-width: 46rem; padding: 3rem 1.25rem 5rem;
  background: #0c0c0b; color: #edeae2;
  font: 16px/1.65 ui-sans-serif, -apple-system, "Segoe UI", sans-serif;
}
h1 { font-size: 1.75rem; letter-spacing: -0.02em; margin: 0 0 .35rem; }
h2 { font-size: .8rem; text-transform: uppercase; letter-spacing: .12em; color: #8b8880; margin: 2.5rem 0 .6rem; }
p, li { color: #c9c5bb; }
a { color: #d9f27e; }
code { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: .9em; color: #edeae2;
  background: #131312; border: 1px solid #262624; border-radius: 4px; padding: .1em .35em; }
.card { background: #131312; border: 1px solid #262624; border-radius: 10px; padding: 1rem 1.25rem; margin-top: 1rem; }
.lead { color: #8b8880; margin: 0; }
footer { margin-top: 3rem; padding-top: 1rem; border-top: 1px solid #262624; color: #8b8880; font-size: .85rem; }
</style>
</head>
<body>
<h1>Abuse</h1>
<p class="lead">Xelio is a free disposable email service. We do not tolerate use of it for spam, fraud, or evading verification on services that prohibit disposable addresses.</p>

<h2>Report abuse</h2>
<div class="card">
<p>Email <a href="mailto:abuse@xelio.me">abuse@xelio.me</a>. Reports are read by a human and acknowledged within two business days.</p>
</div>

<h2>What to include</h2>
<ul>
<li>The full recipient address, e.g. <code>abc123@xelio.me</code>.</li>
<li>The sending IP addresses and timestamps of the messages (UTC), if you have them.</li>
<li>Message headers, or the message itself, with any credentials removed.</li>
<li>A short description of the policy being violated and the requesting organisation.</li>
</ul>

<h2>What we can do</h2>
<ul>
<li>Block a specific address or a pattern of addresses from receiving mail.</li>
<li>Block sending IPs at the SMTP layer.</li>
<li>Preserve the message metadata needed to investigate.</li>
<li>Terminate addresses and delete stored mail.</li>
</ul>
<p>Mailboxes expire automatically after 24 hours, so a report about an old address may arrive after the data is gone.</p>

<h2>Law enforcement</h2>
<p>We can preserve records on request. Include a case number and a contact address. We cannot decrypt messages we never stored.</p>

<footer>Xelio · <a href="https://xelio.me">xelio.me</a> · machine-readable contact in <code>/.well-known/security.txt</code></footer>
</body>
</html>
"#;

const SECURITY_TXT_BODY: &str = "\
Contact: mailto:abuse@xelio.me
Expires: 2027-10-28T00:00:00.000Z
Preferred-Languages: en
Canonical: https://api.xelio.me/.well-known/security.txt
Policy: https://api.xelio.me/abuse
";

pub async fn abuse() -> Html<&'static str> {
    Html(ABUSE_HTML)
}

pub async fn security_txt() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        SECURITY_TXT_BODY,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abuse_page_is_self_contained() {
        assert!(ABUSE_HTML.contains("abuse@xelio.me"));
        assert!(!ABUSE_HTML.contains("<script"));
        assert!(!ABUSE_HTML.contains("http://"));
        assert!(!ABUSE_HTML.contains("https://cdn"));
    }

    #[tokio::test]
    async fn security_txt_has_required_fields() {
        let response = security_txt().await;
        assert_eq!(response.status(), 200);
        assert!(SECURITY_TXT_BODY.contains("Contact: mailto:abuse@xelio.me"));
        assert!(SECURITY_TXT_BODY.contains("Canonical:"));
    }
}

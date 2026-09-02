use std::{
    env,
    process::ExitCode,
    time::{Duration, Instant},
};

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{
        tcp::{OwnedReadHalf, OwnedWriteHalf},
        TcpStream,
    },
};

fn print_usage() {
    eprintln!(
        "smtp-bench
Concurrent SMTP load probe for the p95 greeting and DATA-accept claims.

Usage:
  smtp-bench [options]

Options:
  --host HOST          SMTP host (default: 127.0.0.1)
  --port PORT          SMTP port (default: 2525)
  --domain DOMAIN      RCPT domain; must match MAIL_DOMAIN (default: $MAIL_DOMAIN or xelio.me)
  --connections N      concurrent live connections (default: 1000)
  --metrics-url URL    server snapshot URL (default: http://127.0.0.1:9100/)
  --help               print this help

Run against a live smtp process, then:
  cargo run -p smtp --bin smtp-bench -- --port 2525 --connections 1000

Opens N connections and times connect to the first 220 (client greeting).
Holds all N open, fetches server metrics, then HELO/MAIL/RCPT/DATA/.
Times '.' write to 250 (client data-accept). QUITs only after every 250.
Exits 0 if client p95 greeting < 20ms and client p95 data-accept < 50ms.
If the server snapshot is fetched, inflight_connections must also be >= N during the hold.
"
    );
}

struct Args {
    host: String,
    port: u16,
    domain: String,
    connections: usize,
    metrics_url: String,
}

fn parse_args() -> Result<Args, String> {
    let mut host = "127.0.0.1".to_string();
    let mut port: u16 = 2525;
    let mut domain = env::var("MAIL_DOMAIN").unwrap_or_else(|_| "xelio.me".to_string());
    let mut connections: usize = 1000;
    let mut metrics_url = "http://127.0.0.1:9100/".to_string();

    let mut argv = env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            "--host" => {
                host = argv.next().ok_or_else(|| missing("--host"))?;
            }
            "--port" => {
                let raw = argv.next().ok_or_else(|| missing("--port"))?;
                port = raw
                    .parse()
                    .map_err(|_| format!("--port expects a port number, got {raw}"))?;
            }
            "--domain" => {
                domain = argv.next().ok_or_else(|| missing("--domain"))?;
            }
            "--connections" => {
                let raw = argv.next().ok_or_else(|| missing("--connections"))?;
                connections = raw
                    .parse()
                    .map_err(|_| format!("--connections expects an integer, got {raw}"))?;
            }
            "--metrics-url" => {
                metrics_url = argv.next().ok_or_else(|| missing("--metrics-url"))?;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    if connections == 0 {
        return Err("--connections must be > 0".to_string());
    }

    Ok(Args {
        host,
        port,
        domain,
        connections,
        metrics_url,
    })
}

fn missing(flag: &str) -> String {
    format!("{flag} requires a value")
}

struct Session {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    greeting: Duration,
}

async fn read_reply(reader: &mut BufReader<OwnedReadHalf>) -> Result<(u16, String), String> {
    let mut body = String::new();
    loop {
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .await
            .map_err(|e| format!("read: {e}"))?;
        if n == 0 {
            return Err("peer closed before SMTP reply".to_string());
        }
        body.push_str(&line);
        let bytes = line.as_bytes();
        if bytes.len() < 4 {
            return Err(format!("truncated SMTP line: {line:?}"));
        }
        let code: u16 = std::str::from_utf8(&bytes[0..3])
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| format!("non-numeric SMTP code: {line:?}"))?;
        if bytes[3] == b' ' {
            return Ok((code, body));
        }
        if bytes[3] != b'-' {
            return Err(format!("malformed SMTP line: {line:?}"));
        }
    }
}

async fn write_cmd(writer: &mut OwnedWriteHalf, cmd: &str) -> Result<(), String> {
    writer
        .write_all(cmd.as_bytes())
        .await
        .map_err(|e| format!("write: {e}"))?;
    writer.flush().await.map_err(|e| format!("flush: {e}"))
}

async fn expect_code(
    reader: &mut BufReader<OwnedReadHalf>,
    expected: u16,
) -> Result<(), String> {
    let (code, body) = read_reply(reader).await?;
    if code != expected {
        return Err(format!("expected {expected}, got {code}: {}", body.trim()));
    }
    Ok(())
}

async fn connect_one(host: String, port: u16) -> Result<Session, String> {
    let start = Instant::now();
    let stream = TcpStream::connect((host.as_str(), port))
        .await
        .map_err(|e| format!("connect {host}:{port}: {e}"))?;
    if let Err(e) = stream.set_nodelay(true) {
        return Err(format!("TCP_NODELAY: {e}"));
    }
    let (read_half, write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    expect_code(&mut reader, 220).await?;
    Ok(Session {
        reader,
        writer: write_half,
        greeting: start.elapsed(),
    })
}

async fn data_phase(mut session: Session, domain: String) -> Result<(Session, Duration), String> {
    write_cmd(&mut session.writer, "HELO bench\r\n").await?;
    expect_code(&mut session.reader, 250).await?;
    write_cmd(&mut session.writer, "MAIL FROM:<bench@example.com>\r\n").await?;
    expect_code(&mut session.reader, 250).await?;
    let rcpt = format!("RCPT TO:<bench@{domain}>\r\n");
    write_cmd(&mut session.writer, &rcpt).await?;
    expect_code(&mut session.reader, 250).await?;
    write_cmd(&mut session.writer, "DATA\r\n").await?;
    expect_code(&mut session.reader, 354).await?;
    write_cmd(&mut session.writer, "Subject: bench\r\n\r\nping\r\n").await?;
    let start = Instant::now();
    write_cmd(&mut session.writer, ".\r\n").await?;
    expect_code(&mut session.reader, 250).await?;
    Ok((session, start.elapsed()))
}

async fn quit_phase(mut session: Session) -> Result<(), String> {
    write_cmd(&mut session.writer, "QUIT\r\n").await?;
    expect_code(&mut session.reader, 221).await?;
    Ok(())
}

fn p95_ms(samples: &[Duration]) -> f64 {
    let mut ms: Vec<u128> = samples.iter().map(|d| d.as_millis()).collect();
    ms.sort_unstable();
    let n = ms.len();
    let idx = ((n as f64) * 0.95).ceil() as usize - 1;
    ms[idx.min(n - 1)] as f64
}

async fn fetch_metrics(url: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    response.text().await.map_err(|e| e.to_string())
}

fn inflight_from_json(body: &str) -> Option<u64> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    value.get("inflight_connections")?.as_u64()
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(e) => {
            eprintln!("{e}");
            print_usage();
            return ExitCode::from(2);
        }
    };

    let n = args.connections;
    println!(
        "connecting {n} sessions to {}:{} domain={}",
        args.host, args.port, args.domain
    );

    let mut joins = Vec::with_capacity(n);
    for _ in 0..n {
        let host = args.host.clone();
        let port = args.port;
        joins.push(tokio::spawn(async move { connect_one(host, port).await }));
    }

    let mut sessions = Vec::with_capacity(n);
    for join in joins {
        match join.await {
            Ok(Ok(session)) => sessions.push(session),
            Ok(Err(e)) => {
                eprintln!("connect failed: {e}");
                return ExitCode::from(1);
            }
            Err(e) => {
                eprintln!("connect task panicked: {e}");
                return ExitCode::from(1);
            }
        }
    }

    if sessions.len() != n {
        eprintln!("connected {} of {n} sessions", sessions.len());
        return ExitCode::from(1);
    }

    let greetings: Vec<Duration> = sessions.iter().map(|s| s.greeting).collect();
    let client_greeting_p95 = p95_ms(&greetings);
    println!("client_greeting_p95_ms={client_greeting_p95}");

    let hold_snapshot = fetch_metrics(&args.metrics_url).await;
    let mut hold_inflight: Option<u64> = None;
    match &hold_snapshot {
        Ok(body) => {
            println!("server_hold_snapshot={body}");
            hold_inflight = inflight_from_json(body);
        }
        Err(e) => {
            println!("server metrics were unreachable ({e})");
        }
    }

    let mut data_joins = Vec::with_capacity(n);
    for session in sessions {
        let domain = args.domain.clone();
        data_joins.push(tokio::spawn(async move { data_phase(session, domain).await }));
    }

    let mut after_data = Vec::with_capacity(n);
    let mut data_accepts = Vec::with_capacity(n);
    for join in data_joins {
        match join.await {
            Ok(Ok((session, elapsed))) => {
                data_accepts.push(elapsed);
                after_data.push(session);
            }
            Ok(Err(e)) => {
                eprintln!("DATA phase failed: {e}");
                return ExitCode::from(1);
            }
            Err(e) => {
                eprintln!("DATA task panicked: {e}");
                return ExitCode::from(1);
            }
        }
    }

    let client_data_accept_p95 = p95_ms(&data_accepts);
    println!("client_data_accept_p95_ms={client_data_accept_p95}");

    let mut quit_joins = Vec::with_capacity(after_data.len());
    for session in after_data {
        quit_joins.push(tokio::spawn(async move { quit_phase(session).await }));
    }
    for join in quit_joins {
        if let Err(e) = join.await {
            eprintln!("QUIT task panicked: {e}");
        }
    }

    if let Ok(body) = fetch_metrics(&args.metrics_url).await {
        println!("server_snapshot={body}");
    }

    let greeting_ok = client_greeting_p95 < 20.0;
    let data_ok = client_data_accept_p95 < 50.0;
    if !greeting_ok {
        eprintln!("client p95 greeting {client_greeting_p95}ms is not < 20ms");
    }
    if !data_ok {
        eprintln!("client p95 data-accept {client_data_accept_p95}ms is not < 50ms");
    }

    let inflight_ok = match hold_inflight {
        Some(inflight) if inflight >= n as u64 => true,
        Some(inflight) => {
            eprintln!("inflight_connections {inflight} is not >= {n} during hold");
            false
        }
        None => true,
    };

    if greeting_ok && data_ok && inflight_ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

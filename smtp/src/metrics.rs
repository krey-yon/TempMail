use std::{
    env,
    net::SocketAddr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use serde::Serialize;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};

const BUCKETS: usize = 64;

struct LatencyHistogram {
    buckets: [AtomicU64; BUCKETS],
    overflow: AtomicU64,
}

impl LatencyHistogram {
    fn new() -> Self {
        Self {
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            overflow: AtomicU64::new(0),
        }
    }

    fn record(&self, d: Duration) {
        let ms = d.as_millis();
        if ms < BUCKETS as u128 {
            self.buckets[ms as usize].fetch_add(1, Ordering::Relaxed);
        } else {
            self.overflow.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn count_and_p95(&self) -> (u64, Option<f64>) {
        let mut total = 0u64;
        let mut counts = [0u64; BUCKETS];
        for i in 0..BUCKETS {
            counts[i] = self.buckets[i].load(Ordering::Relaxed);
            total += counts[i];
        }
        let overflow = self.overflow.load(Ordering::Relaxed);
        total += overflow;
        if total == 0 {
            return (0, None);
        }
        let mut cumulative = 0u64;
        for (ms, count) in counts.iter().enumerate() {
            cumulative += count;
            if cumulative * 100 >= total * 95 {
                return (total, Some(ms as f64));
            }
        }
        (total, Some(BUCKETS as f64))
    }
}

pub struct SmtpMetrics {
    inflight: AtomicU64,
    greeting: LatencyHistogram,
    data_accept: LatencyHistogram,
}

#[derive(Debug, Serialize)]
pub struct SmtpSnapshot {
    pub inflight_connections: u64,
    pub greeting_count: u64,
    pub greeting_p95_ms: Option<f64>,
    pub data_accept_count: u64,
    pub data_accept_p95_ms: Option<f64>,
}

pub struct InflightGuard {
    metrics: Arc<SmtpMetrics>,
}

impl SmtpMetrics {
    pub fn new() -> Self {
        Self {
            inflight: AtomicU64::new(0),
            greeting: LatencyHistogram::new(),
            data_accept: LatencyHistogram::new(),
        }
    }

    pub fn session(self: &Arc<Self>) -> InflightGuard {
        self.inflight.fetch_add(1, Ordering::Relaxed);
        InflightGuard {
            metrics: Arc::clone(self),
        }
    }

    pub fn record_greeting(&self, d: Duration) {
        self.greeting.record(d);
    }

    pub fn record_data_accept(&self, d: Duration) {
        self.data_accept.record(d);
    }

    pub fn snapshot(&self) -> SmtpSnapshot {
        let (greeting_count, greeting_p95_ms) = self.greeting.count_and_p95();
        let (data_accept_count, data_accept_p95_ms) = self.data_accept.count_and_p95();
        SmtpSnapshot {
            inflight_connections: self.inflight.load(Ordering::Relaxed),
            greeting_count,
            greeting_p95_ms,
            data_accept_count,
            data_accept_p95_ms,
        }
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.metrics.inflight.fetch_sub(1, Ordering::Relaxed);
    }
}

pub fn spawn_listener(metrics: Arc<SmtpMetrics>) {
    let port: u16 = env::var("SMTP_METRICS_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(9100);

    tokio::spawn(async move {
        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        let listener = match TcpListener::bind(addr).await {
            Ok(listener) => listener,
            Err(e) => {
                tracing::error!("SMTP metrics listener bind failed on {addr}: {e}");
                return;
            }
        };
        tracing::info!("SMTP metrics listening on {addr}");
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    serve_snapshot(stream, metrics.snapshot()).await;
                }
                Err(e) => {
                    tracing::error!("SMTP metrics accept failed: {e}");
                }
            }
        }
    });
}

async fn serve_snapshot(mut stream: TcpStream, snapshot: SmtpSnapshot) {
    let mut req = [0u8; 1024];
    let _ = timeout(Duration::from_millis(200), stream.read(&mut req)).await;
    let body = match serde_json::to_string(&snapshot) {
        Ok(body) => body,
        Err(e) => {
            tracing::error!("SMTP metrics serialize failed: {e}");
            return;
        }
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn p95_lands_in_expected_bucket() {
        let metrics = SmtpMetrics::new();
        for _ in 0..95 {
            metrics.record_greeting(Duration::from_millis(1));
        }
        for _ in 0..5 {
            metrics.record_greeting(Duration::from_millis(50));
        }
        let snap = metrics.snapshot();
        assert_eq!(snap.greeting_count, 100, "recorded 100 greeting samples");
        assert_eq!(
            snap.greeting_p95_ms,
            Some(1.0),
            "p95 of 95x1ms + 5x50ms is the 1ms bucket"
        );
    }

    #[test]
    fn empty_histogram_p95_is_none() {
        let snap = SmtpMetrics::new().snapshot();
        assert_eq!(snap.greeting_count, 0, "no greeting samples");
        assert_eq!(snap.greeting_p95_ms, None, "empty greeting p95 is None");
        assert_eq!(snap.data_accept_count, 0, "no data-accept samples");
        assert_eq!(
            snap.data_accept_p95_ms, None,
            "empty data-accept p95 is None"
        );
        assert_eq!(snap.inflight_connections, 0, "no sessions");
    }

    #[test]
    fn inflight_guard_inc_dec() {
        let metrics = Arc::new(SmtpMetrics::new());
        let first = metrics.session();
        let second = metrics.session();
        assert_eq!(
            metrics.snapshot().inflight_connections,
            2,
            "two live InflightGuards"
        );
        drop(first);
        assert_eq!(
            metrics.snapshot().inflight_connections,
            1,
            "dropping one guard decrements inflight"
        );
        drop(second);
        assert_eq!(
            metrics.snapshot().inflight_connections,
            0,
            "dropping the last guard returns inflight to 0"
        );
    }
}

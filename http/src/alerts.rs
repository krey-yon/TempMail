//! Threshold alerting for mailbox-creation abuse.
//!
//! The service had no signal: a residential proxy farm minted thousands of
//! addresses and the first observable symptom was a user complaining. This
//! module polls once a minute, compares the recent window against configured
//! thresholds, and pushes a single message to a webhook.
//!
//! The payload carries both `text` (Slack, Mattermost) and `content`
//! (Discord) so one URL works for any of them. Alerting is best effort: a
//! failed delivery never affects request handling.

use std::{
    collections::HashMap,
    env,
    sync::Arc,
    time::{Duration, Instant},
};

use database::database::DatabaseClient;
use serde_json::json;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{error, info, warn};

use crate::access_log::AccessLog;

const TICK: Duration = Duration::from_secs(60);
const EVENT_MAILS_RECEIVED: &str = "emails_received";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlertKind {
    Creates,
    Rejections,
    Mails,
    RateLimited,
}

impl AlertKind {
    fn label(self) -> &'static str {
        match self {
            Self::Creates => "mailbox creations",
            Self::Rejections => "rejected creations",
            Self::Mails => "inbound messages",
            Self::RateLimited => "rate-limited requests",
        }
    }
}

pub struct Alerter {
    webhook: Option<String>,
    creates: u64,
    rejections: u64,
    mails: u64,
    rate_limited: u64,
    cooldown: Duration,
    panel_url: String,
    client: reqwest::Client,
    fired: HashMap<AlertKind, Instant>,
    mails_baseline: Option<i64>,
}

impl Alerter {
    pub fn from_env() -> Self {
        let webhook = env::var("ALERT_WEBHOOK_URL")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());

        if webhook.is_none() {
            info!("ALERT_WEBHOOK_URL is not set; abuse alerting is disabled");
        }

        Self {
            webhook,
            creates: env_u64("ALERT_CREATES_PER_MIN", 60),
            rejections: env_u64("ALERT_REJECTIONS_PER_MIN", 120),
            mails: env_u64("ALERT_MAILS_PER_MIN", 60),
            rate_limited: env_u64("ALERT_429_PER_MIN", 30),
            cooldown: Duration::from_secs(env_u64("ALERT_COOLDOWN_SECS", 30 * 60).max(60)),
            panel_url: env::var("PANEL_URL")
                .ok()
                .map(|value| value.trim().trim_end_matches('/').to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "https://api.xelio.me/admin-panel".to_string()),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
            fired: HashMap::new(),
            mails_baseline: None,
        }
    }

    pub fn enabled(&self) -> bool {
        self.webhook.is_some()
    }

    /// Spawn the once-a-minute evaluation loop. The task owns the alerter, so
    /// the cooldown map and mail baseline need no locking.
    pub fn spawn(mut self, db: Arc<DatabaseClient>, log: Arc<AccessLog>) {
        if !self.enabled() {
            return;
        }
        tokio::spawn(async move {
            let mut ticker = interval(TICK);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
            // Skip the immediate first tick: there is no window to measure yet.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                self.evaluate(&db, &log).await;
            }
        });
        info!("Abuse alerting started (evaluating every 60s)");
    }

    /// One evaluation pass. `window` stays at one minute so the numbers line up
    /// with the `*_PER_MIN` environment variables.
    pub async fn evaluate(&mut self, db: &Arc<DatabaseClient>, log: &Arc<AccessLog>) {
        let stats = log.stats_since(TICK);
        let mut breaches = Vec::new();

        if stats.creates >= self.creates {
            breaches.push((AlertKind::Creates, stats.creates, self.creates));
        }
        if stats.rejected_creates >= self.rejections {
            breaches.push((
                AlertKind::Rejections,
                stats.rejected_creates,
                self.rejections,
            ));
        }
        if stats.rate_limited >= self.rate_limited {
            breaches.push((
                AlertKind::RateLimited,
                stats.rate_limited,
                self.rate_limited,
            ));
        }

        match db.get_analytics().await {
            Ok(rows) => {
                let total = rows
                    .iter()
                    .find(|row| row.event_type == EVENT_MAILS_RECEIVED)
                    .map(|row| row.event_count)
                    .unwrap_or(0);
                if let Some(previous) = self.mails_baseline {
                    let delta = total.saturating_sub(previous).max(0) as u64;
                    if delta >= self.mails {
                        breaches.push((AlertKind::Mails, delta, self.mails));
                    }
                }
                self.mails_baseline = Some(total);
            }
            // Alerting must never be the reason a request path misbehaves.
            Err(error) => warn!("Alert evaluation could not read analytics: {}", error),
        }

        for (kind, observed, threshold) in breaches {
            self.fire(kind, observed, threshold, &stats, log).await;
        }
    }

    async fn fire(
        &mut self,
        kind: AlertKind,
        observed: u64,
        threshold: u64,
        stats: &crate::access_log::WindowStats,
        log: &AccessLog,
    ) {
        if let Some(last) = self.fired.get(&kind) {
            if last.elapsed() < self.cooldown {
                return;
            }
        }
        self.fired.insert(kind, Instant::now());

        let top_ips = top_creators(log, 5);
        let distinct_ips = log.distinct_ips_since(TICK);
        let body = json!({
            "text": format!(
                "Xelio alert: {label} at {observed}/min (threshold {threshold}) from {distinct_ips} IPs. Top IPs: {ips}. Panel: {panel}",
                label = kind.label(),
                ips = top_ips.join(", "),
                panel = self.panel_url,
            ),
            "content": format!(
                "**Xelio alert** — {label} at **{observed}/min** (threshold {threshold})\n\
                 Distinct IPs: {distinct_ips}\nTop IPs: {ips}\nCreations: {c} · rejected: {r} · 429s: {rl}\nPanel: {panel}",
                label = kind.label(),
                ips = top_ips.join(", "),
                c = stats.creates,
                r = stats.rejected_creates,
                rl = stats.rate_limited,
                panel = self.panel_url,
            ),
            "kind": format!("{kind:?}"),
            "observed": observed,
            "threshold": threshold,
        });

        self.post(body).await;
    }

    async fn post(&self, body: serde_json::Value) {
        let Some(webhook) = self.webhook.as_deref() else {
            return;
        };
        match self.client.post(webhook).json(&body).send().await {
            Ok(response) if response.status().is_success() => {
                info!("Alert delivered to webhook ({})", response.status());
            }
            Ok(response) => warn!("Alert webhook returned HTTP {}", response.status()),
            Err(error) => error!("Alert webhook request failed: {}", error),
        }
    }

    /// Send a synthetic alert so an operator can confirm wiring end to end.
    pub async fn send_test(&self) -> bool {
        if self.webhook.is_none() {
            return false;
        }
        self.post(json!({
            "text": format!("Xelio test alert. Panel: {}", self.panel_url),
            "content": format!("**Xelio test alert** — webhook wiring works.\nPanel: {}", self.panel_url),
        }))
        .await;
        true
    }
}

fn top_creators(log: &AccessLog, limit: usize) -> Vec<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for record in log.snapshot(500, true) {
        if record.status != 201 {
            continue;
        }
        *counts.entry(record.ip).or_default() += 1;
    }
    let mut rows: Vec<(String, usize)> = counts.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    rows.into_iter()
        .take(limit)
        .map(|(ip, count)| format!("{ip} ({count})"))
        .collect()
}

fn env_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access_log::Record;

    fn record(at: &str, ip: &str, method: &str, path: &str, status: u16) -> Record {
        Record {
            at: at.to_string(),
            ip: ip.to_string(),
            method: method.to_string(),
            path: path.to_string(),
            status,
            ms: 1,
            user_agent: "curl/8.7.1".to_string(),
            origin: None,
            referer: None,
        }
    }

    fn now(offset_secs: i64) -> String {
        (chrono::Utc::now() + chrono::Duration::seconds(offset_secs))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    #[test]
    fn window_counts_only_recent_creates() {
        let log = AccessLog::seeded(
            100,
            vec![
                record(&now(-3600), "10.0.0.1", "POST", "/api/emails", 201),
                record(&now(-5), "10.0.0.2", "POST", "/api/emails", 201),
                record(&now(-4), "10.0.0.3", "POST", "/api/emails", 403),
                record(&now(-3), "10.0.0.4", "GET", "/api/emails/a@xelio.me", 429),
            ],
        );
        let stats = log.stats_since(Duration::from_secs(60));
        assert_eq!(stats.creates, 1);
        assert_eq!(stats.rejected_creates, 1);
        assert_eq!(stats.rate_limited, 1);
        assert_eq!(log.distinct_ips_since(Duration::from_secs(60)), 3);
    }

    #[test]
    fn thresholds_default_sensibly() {
        assert_eq!(env_u64("XELIO_MISSING_ALERT_VAR", 60), 60);
        std::env::set_var("XELIO_TEST_ALERT_VAR", "0");
        assert_eq!(env_u64("XELIO_TEST_ALERT_VAR", 60), 60);
        std::env::set_var("XELIO_TEST_ALERT_VAR", "15");
        assert_eq!(env_u64("XELIO_TEST_ALERT_VAR", 60), 15);
        std::env::remove_var("XELIO_TEST_ALERT_VAR");
    }

    #[test]
    fn top_creators_ranks_by_volume() {
        let log = AccessLog::seeded(
            100,
            vec![
                record(&now(-10), "10.0.0.1", "POST", "/api/emails", 201),
                record(&now(-9), "10.0.0.1", "POST", "/api/emails", 201),
                record(&now(-8), "10.0.0.2", "POST", "/api/emails", 201),
                record(&now(-7), "10.0.0.3", "POST", "/api/emails", 409),
            ],
        );
        let top = top_creators(&log, 5);
        assert_eq!(top[0], "10.0.0.1 (2)");
        assert_eq!(top.len(), 2);
    }

    #[test]
    fn alerter_is_disabled_without_a_webhook() {
        std::env::remove_var("ALERT_WEBHOOK_URL");
        let alerter = Alerter::from_env();
        assert!(!alerter.enabled());
    }
}

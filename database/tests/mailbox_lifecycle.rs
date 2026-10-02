use database::database::{DatabaseClient, Email, MailDeliveryError, MailRecipient};

async fn expire(db: &DatabaseClient, address: &str) {
    db.pool.get().await.unwrap().execute(
        "UPDATE email_addresses SET created_at = ((clock_timestamp() AT TIME ZONE 'UTC') - INTERVAL '24 hours')::text WHERE address = $1",
        &[&address],
    ).await.unwrap();
}

fn message() -> Email {
    Email {
        sender: "sender@example.org".into(),
        content: "Subject: private\r\n\r\nsecret".into(),
        ..Default::default()
    }
}

async fn count(db: &DatabaseClient) -> i64 {
    db.get_total_stats().await.unwrap().total_addresses_created
}

#[tokio::test]
#[ignore = "requires an isolated PostgreSQL database configured through DB_* variables"]
async fn expiry_reuse_delivery_isolation_and_lifetime_count() {
    assert!(
        std::env::var("DB_NAME").unwrap().ends_with("_test"),
        "use an isolated test database"
    );
    let db = DatabaseClient::connect().await.unwrap();
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let username = format!("reuse{suffix}");
    let baseline = count(&db).await;
    let first = db.create_email_address(&username).await.unwrap();
    let first_token = first.access_token.as_deref().unwrap();
    let recipient = db
        .get_active_mailbox(&first.address)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(count(&db).await, baseline + 1);
    assert!(db.create_email_address(&username).await.is_err());
    assert_eq!(
        count(&db).await,
        baseline + 1,
        "failed creations must not count"
    );

    db.deliver_mail(message(), &[recipient.clone()])
        .await
        .unwrap();
    let mails = db
        .get_mails_by_recipient(&first.address, first_token)
        .await
        .unwrap();
    assert_eq!(mails.len(), 1);
    assert!(db
        .get_mail_by_id(&mails[0].id, &first.address, first_token)
        .await
        .unwrap()
        .is_some());
    assert_eq!(
        db.delete_mail(&mails[0].id, &first.address, "wrong-token")
            .await
            .unwrap(),
        0
    );
    db.pool.get().await.unwrap().execute(
        "INSERT INTO user_config (mail, address, web_hook_address) VALUES ($1, $1, 'https://old-owner.invalid')",
        &[&first.address],
    ).await.unwrap();

    expire(&db, &first.address).await;
    assert!(!db
        .mailbox_token_matches(&first.address, first_token)
        .await
        .unwrap());
    assert!(db
        .get_active_mailbox(&first.address)
        .await
        .unwrap()
        .is_none());
    assert!(db
        .get_mails_by_recipient(&first.address, first_token)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(db.get_total_stats().await.unwrap().total_email_addresses, 0);

    // Reclamation works at the deadline without waiting for a background sweep.
    let second = db.create_email_address(&username).await.unwrap();
    let second_token = second.access_token.as_deref().unwrap();
    assert_ne!(first_token, second_token);
    let new_recipient = db
        .get_active_mailbox(&second.address)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(recipient.mailbox_id, new_recipient.mailbox_id);
    assert_eq!(count(&db).await, baseline + 2);
    assert!(db
        .get_mails_by_recipient(&second.address, second_token)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        db.get_quota(&second.address).await.unwrap(),
        Some((1000, 0))
    );
    let configs: i64 = db
        .pool
        .get()
        .await
        .unwrap()
        .query_one(
            "SELECT COUNT(*) FROM user_config WHERE mail = $1",
            &[&second.address],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(configs, 0, "replacement must not inherit the old webhook");
    assert!(!db
        .delete_email_address(&second.address, first_token)
        .await
        .unwrap());
    let error = db.deliver_mail(message(), &[recipient]).await.unwrap_err();
    assert!(matches!(
        error.downcast_ref::<MailDeliveryError>(),
        Some(MailDeliveryError::MailboxUnavailable)
    ));
    assert_eq!(
        db.get_quota(&second.address).await.unwrap(),
        Some((1000, 0))
    );

    // All recipients and quotas roll back if any recipient became unavailable.
    let unknown = MailRecipient {
        address: "missing@xelio.me".into(),
        mailbox_id: "missing".into(),
    };
    assert!(db
        .deliver_mail(message(), &[new_recipient.clone(), unknown])
        .await
        .is_err());
    assert!(db
        .get_mails_by_recipient(&second.address, second_token)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        db.get_quota(&second.address).await.unwrap(),
        Some((1000, 0))
    );
    db.pool
        .get()
        .await
        .unwrap()
        .execute(
            "UPDATE quota SET quota_limit = 0 WHERE address = $1",
            &[&second.address],
        )
        .await
        .unwrap();
    let error = db
        .deliver_mail(message(), &[new_recipient.clone()])
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<MailDeliveryError>(),
        Some(MailDeliveryError::QuotaExceeded)
    ));
    db.pool
        .get()
        .await
        .unwrap()
        .execute(
            "UPDATE quota SET quota_limit = 1000 WHERE address = $1",
            &[&second.address],
        )
        .await
        .unwrap();
    db.deliver_mail(message(), &[new_recipient]).await.unwrap();

    expire(&db, &second.address).await;
    assert_eq!(db.delete_old_email_addresses().await.unwrap(), 1);
    assert_eq!(count(&db).await, baseline + 2);
    assert!(db
        .list_registered_usernames()
        .await
        .unwrap()
        .iter()
        .all(|r| r.username != username));
    let third = db.create_email_address(&username).await.unwrap();
    assert!(db
        .get_mails_by_recipient(&third.address, third.access_token.as_deref().unwrap())
        .await
        .unwrap()
        .is_empty());
    assert!(db
        .delete_email_address(&third.address, third.access_token.as_deref().unwrap())
        .await
        .unwrap());
    assert_eq!(count(&db).await, baseline + 3);

    // Concurrent claims have exactly one winner and one counted creation.
    let (left, right) = tokio::join!(
        db.create_email_address(&username),
        db.create_email_address(&username)
    );
    assert_ne!(left.is_ok(), right.is_ok());
    assert_eq!(count(&db).await, baseline + 4);
    let winner = left.or(right).unwrap();
    expire(&db, &winner.address).await;
    let (cleanup, reclaimed) = tokio::join!(
        db.delete_old_email_addresses(),
        db.create_email_address(&username)
    );
    cleanup.unwrap();
    let reclaimed = reclaimed.unwrap();
    assert!(db
        .mailbox_token_matches(
            &reclaimed.address,
            reclaimed.access_token.as_deref().unwrap()
        )
        .await
        .unwrap());
    assert_eq!(count(&db).await, baseline + 5);

    // Startup migration is idempotent and does not reset the lifetime counter.
    let restarted = DatabaseClient::connect().await.unwrap();
    assert_eq!(count(&restarted).await, baseline + 5);
    assert!(db
        .delete_email_address(
            &reclaimed.address,
            reclaimed.access_token.as_deref().unwrap()
        )
        .await
        .unwrap());

    // Exercise a pre-upgrade dataset without touching persistent tables.
    // Temporary tables shadow them only within this connection.
    let mut client = db.pool.get().await.unwrap();
    let tx = client.transaction().await.unwrap();
    tx.batch_execute(
        "CREATE TEMP TABLE analytics (event_type TEXT UNIQUE, event_count BIGINT, last_updated TEXT) ON COMMIT DROP;
         CREATE TEMP TABLE email_registry (username TEXT, created_at TEXT) ON COMMIT DROP;
         CREATE TEMP TABLE email_addresses (address TEXT) ON COMMIT DROP;
         INSERT INTO analytics VALUES ('email_address_created', 2, now()::text);
         INSERT INTO email_registry VALUES ('alice', now()::text), ('bob', now()::text), ('carol', now()::text);
         INSERT INTO email_addresses VALUES ('alice@xelio.me');",
    ).await.unwrap();
    tx.batch_execute(include_str!("../migrations/004_reusable_usernames.sql")).await.unwrap();
    let seeded: i64 = tx.query_one("SELECT event_count FROM analytics", &[]).await.unwrap().get(0);
    let reservations: i64 = tx.query_one("SELECT COUNT(*) FROM email_registry", &[]).await.unwrap().get(0);
    assert_eq!(seeded, 3);
    assert_eq!(reservations, 1);
    tx.batch_execute(include_str!("../migrations/004_reusable_usernames.sql")).await.unwrap();
    let reseeded: i64 = tx.query_one("SELECT event_count FROM analytics", &[]).await.unwrap().get(0);
    assert_eq!(reseeded, 3, "startup must not add active records to the lifetime count");
    tx.rollback().await.unwrap();

}

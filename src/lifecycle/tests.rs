use super::*;
use axum::{body::Body, http::Request};
use tower::ServiceExt;

fn intent() -> Intent {
    Intent {
        subject: "fixture-agent".into(),
        expected: Binding {
            host_id: "fixture-host".into(),
            boot_id: "fixture-boot".into(),
            pid: 42,
            start_ticks: 123,
            exe_sha256: "a".repeat(64),
            thread_id: "fixture-thread".into(),
        },
        target_sha256: "b".repeat(64),
        effect: Effect::Upgrade,
        forward_deadline_ms: chrono::Utc::now().timestamp_millis() + 60_000,
        recovery_deadline_ms: chrono::Utc::now().timestamp_millis() + 120_000,
        recovery_effects: vec![],
    }
}
fn app(pool: Pool, intent: &Intent) -> Router {
    app_with_header(pool, intent, "x-fixture-user")
}
fn app_with_header(pool: Pool, intent: &Intent, header: &str) -> Router {
    app_with_policy(
        pool,
        Policy {
            executors: vec![executor_grant(intent)],
            grants: vec![requester_grant(intent)],
        },
        header,
    )
}
fn requester_grant(intent: &Intent) -> Grant {
    Grant {
        requester: "alice".into(),
        subject: intent.subject.clone(),
        target_sha256: intent.target_sha256.clone(),
        effect: intent.effect.clone(),
    }
}
fn executor_grant(intent: &Intent) -> ExecutorGrant {
    ExecutorGrant {
        requester: "executor".into(),
        host_id: intent.expected.host_id.clone(),
        subject: intent.subject.clone(),
        target_sha256: intent.target_sha256.clone(),
        effect: intent.effect.clone(),
    }
}
fn app_with_policy(pool: Pool, policy: Policy, header: &str) -> Router {
    let (events_tx, _) = tokio::sync::broadcast::channel(16);
    crate::api::lifecycle_router(
        crate::api::AppState {
            pool,
            events_tx,
            ipfs_api_url: None,
            shutdown: tokio_util::sync::CancellationToken::new(),
            db_path: String::new(),
            db_snapshot: crate::api::DbSnapshotCfg::disabled(),
            host_auth: Arc::new(std::collections::HashMap::from([(
                "fixture.test".into(),
                Some(header.into()),
            )])),
            link_rules: Default::default(),
        },
        policy,
    )
}

/// Explicitly invoked integration fixture only. Never selected by ordinary test runs.
/// The test runner owns the config/ready file and process; no production settings are read.
#[tokio::test]
#[ignore = "bounded loopback server for task_146 integration"]
async fn tcp_fixture_server() -> anyhow::Result<()> {
    use std::future::IntoFuture;
    use std::io::Write;
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Config {
        intent: Intent,
        ready_path: String,
        lifetime_ms: u64,
        identity_header: String,
    }
    let path = std::env::var("LIFECYCLE_FIXTURE_CONFIG")?;
    let config: Config = serde_json::from_slice(&std::fs::read(path)?)?;
    anyhow::ensure!(
        (100..=300_000).contains(&config.lifetime_ms),
        "fixture lifetime out of bounds"
    );
    anyhow::ensure!(
        matches!(
            config.identity_header.as_str(),
            "x-fixture-user" | "x-fleet-agent"
        ),
        "unsupported fixture identity header"
    );
    config
        .intent
        .validate()
        .map_err(|e| anyhow::anyhow!("{}", e.1))?;
    let temp = tempfile::tempdir()?;
    let pool = crate::db::init(temp.path().join("fixture.db").to_str().unwrap()).await?;
    install(&pool).await?;
    let router = app_with_header(pool.clone(), &config.intent, &config.identity_header);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let mut ready = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(config.ready_path)?;
    writeln!(
        ready,
        "{}",
        json!({"base_url":format!("http://{addr}"),"host":"fixture.test","identity_header":config.identity_header,"requester":"alice","executor":"executor","intent":config.intent})
    )?;
    ready.sync_all()?;
    let _ = tokio::time::timeout(
        std::time::Duration::from_millis(config.lifetime_ms),
        axum::serve(listener, router).into_future(),
    )
    .await;
    pool.close().await;
    Ok(())
}

async fn post(app: &Router, id: i64, suffix: &str, who: &str, body: Value) -> (StatusCode, Value) {
    request(
        app.clone(),
        "POST",
        &format!("/lifecycle/operations/{id}/{suffix}"),
        Some(who),
        "fixture.test",
        body,
    )
    .await
}
async fn created(app: &Router, intent: &Intent) -> Value {
    let (status, value) = request(
        app.clone(),
        "POST",
        "/lifecycle/operations",
        Some("alice"),
        "fixture.test",
        json!({"request_key":"operation","intent":intent}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    value
}
fn facts(state: &str, intent: &Intent) -> Value {
    let mut v = json!({"kind":state});
    let keys: &[&str] = match state {
        "draining" => &["capable", "local_lock", "launch_excluded", "admission_ack"],
        "stop_intent" => &[
            "quiet",
            "queue_empty",
            "children_empty",
            "writers_settled",
            "journal_durable",
            "no_user_stop",
        ],
        "stopped" => &[
            "exit_observed",
            "children_empty",
            "writers_settled",
            "lock_released",
        ],
        "launch_intent" => &[
            "absence_verified",
            "local_lock",
            "launch_excluded",
            "journal_durable",
            "artifact_verified",
        ],
        "verifying" => &[
            "new_birth_verified",
            "artifact_verified",
            "thread_verified",
            "history_preserved",
        ],
        "completed" => &[
            "ordinary_attempt_completed",
            "admission_verified",
            "history_preserved",
            "user_stop_preserved",
        ],
        _ => panic!("unknown fixture state"),
    };
    for k in keys {
        v[k] = json!(true);
    }
    if state == "verifying" || state == "completed" {
        let mut binding = intent.expected.clone();
        binding.start_ticks += 1;
        binding.pid += 1;
        binding.exe_sha256 = intent.target_sha256.clone();
        v["new_binding"] = json!(binding);
    }
    v
}
fn receipt_body(op: &Value, intent: &Intent, state: &str) -> Value {
    json!({"receipt_key":state,"effect_id":state,"expected_revision":op["revision"],"claim_token":op["claim_token"],
        "expected":intent.expected,"target_sha256":intent.target_sha256,"observed_at_ms":chrono::Utc::now().timestamp_millis(),
        "clock_certain":true,"evidence_sha256":"c".repeat(64),"evidence":facts(state,intent)})
}

#[tokio::test]
async fn claims_are_executor_scoped_fenced_and_idempotent() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let pool = crate::db::init(tmp.path().join("fixture.db").to_str().unwrap()).await?;
    install(&pool).await?;
    let intent = intent();
    let app = app(pool.clone(), &intent);
    let op = created(&app, &intent).await;
    let id = op["operation_id"].as_i64().unwrap();
    let claim = json!({"claim_key":"claim","expected_revision":0,"expected":intent.expected});
    assert_eq!(
        post(&app, id, "claim", "alice", claim.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    let mut wrong = claim.clone();
    wrong["expected"]["start_ticks"] = json!(999);
    assert_eq!(
        post(&app, id, "claim", "executor", wrong).await.0,
        StatusCode::CONFLICT
    );
    let mut hs = vec![];
    for _ in 0..8 {
        let a = app.clone();
        let b = claim.clone();
        hs.push(tokio::spawn(async move {
            post(&a, id, "claim", "executor", b).await
        }));
    }
    let mut first = Value::Null;
    for h in hs {
        let (s, v) = h.await?;
        assert_eq!(s, StatusCode::OK);
        if first.is_null() {
            first = v.clone();
        }
        assert_eq!(v, first);
    }
    let mut other = claim.clone();
    other["claim_key"] = json!("takeover");
    assert_eq!(
        post(&app, id, "claim", "executor", other).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        post(&app, id, "cancel", "alice", json!({"expected_revision":1}))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM events WHERE type='lifecycle.transition'"
        )
        .fetch_one(&pool)
        .await?,
        1
    );
    Ok(())
}

#[tokio::test]
async fn receipt_edges_validate_facts_replay_and_exact_replacement() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let pool = crate::db::init(tmp.path().join("fixture.db").to_str().unwrap()).await?;
    install(&pool).await?;
    let intent = intent();
    let app = app(pool.clone(), &intent);
    let mut op = created(&app, &intent).await;
    let id = op["operation_id"].as_i64().unwrap();
    op = post(
        &app,
        id,
        "claim",
        "executor",
        json!({"claim_key":"claim","expected_revision":0,"expected":intent.expected}),
    )
    .await
    .1;
    let skip = receipt_body(&op, &intent, "completed");
    assert_eq!(
        post(&app, id, "receipts", "executor", skip).await.0,
        StatusCode::CONFLICT
    );
    for state in [
        "draining",
        "stop_intent",
        "stopped",
        "launch_intent",
        "verifying",
        "completed",
    ] {
        let body = receipt_body(&op, &intent, state);
        for (key, val) in body["evidence"].as_object().unwrap() {
            if val == &json!(true) {
                let mut bad = body.clone();
                bad["evidence"][key] = json!(false);
                assert_eq!(
                    post(&app, id, "receipts", "executor", bad).await.0,
                    StatusCode::BAD_REQUEST
                );
            }
        }
        let mut bad = body.clone();
        bad["claim_token"] = json!("d".repeat(64));
        assert_eq!(
            post(&app, id, "receipts", "executor", bad).await.0,
            StatusCode::CONFLICT
        );
        let mut bad = body.clone();
        bad["clock_certain"] = json!(false);
        assert_eq!(
            post(&app, id, "receipts", "executor", bad).await.0,
            StatusCode::CONFLICT
        );
        if state == "completed" {
            let mut bad = body.clone();
            bad["evidence"]["new_binding"]["pid"] = json!(100);
            assert_eq!(
                post(&app, id, "receipts", "executor", bad).await.0,
                StatusCode::CONFLICT
            );
        }
        let (status, value) = post(&app, id, "receipts", "executor", body.clone()).await;
        assert_eq!(status, StatusCode::OK, "{value}");
        op = value;
        assert_eq!(
            post(&app, id, "receipts", "executor", body.clone()).await.1,
            op
        );
        let mut bad = body;
        bad["evidence_sha256"] = json!("e".repeat(64));
        assert_eq!(
            post(&app, id, "receipts", "executor", bad).await.0,
            StatusCode::CONFLICT
        );
    }
    assert_eq!(op["active"], false);
    assert_eq!(op["revision"], 7);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM lifecycle_receipts")
            .fetch_one(&pool)
            .await?,
        6
    );
    Ok(())
}

#[tokio::test]
async fn unknown_retains_exclusion_and_receipt_survives_reopen() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let path = tmp.path().join("fixture.db");
    let pool = crate::db::init(path.to_str().unwrap()).await?;
    install(&pool).await?;
    let intent = intent();
    let router = app(pool.clone(), &intent);
    let op = created(&router, &intent).await;
    let id = op["operation_id"].as_i64().unwrap();
    let op = post(
        &router,
        id,
        "claim",
        "executor",
        json!({"claim_key":"claim","expected_revision":0,"expected":intent.expected}),
    )
    .await
    .1;
    let mut body = receipt_body(&op, &intent, "draining");
    body["evidence"] = json!({"kind":"held_unknown","reason":"fixture outcome unknown"});
    body["clock_certain"] = json!(false);
    let (status, value) = post(&router, id, "receipts", "executor", body.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["active"], true);
    drop(router);
    pool.close().await;
    let pool = crate::db::init(path.to_str().unwrap()).await?;
    install(&pool).await?;
    let router = app(pool.clone(), &intent);
    assert_eq!(
        post(&router, id, "receipts", "executor", body).await.1,
        value
    );
    assert_eq!(
        post(
            &router,
            id,
            "receipts",
            "executor",
            receipt_body(&value, &intent, "draining")
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(
            router,
            "POST",
            "/lifecycle/operations",
            Some("alice"),
            "fixture.test",
            json!({"request_key":"new","intent":intent})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    Ok(())
}

#[tokio::test]
async fn cancel_claim_race_has_one_winner() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let pool = crate::db::init(tmp.path().join("fixture.db").to_str().unwrap()).await?;
    install(&pool).await?;
    let intent = intent();
    let router = app(pool.clone(), &intent);
    let op = created(&router, &intent).await;
    let id = op["operation_id"].as_i64().unwrap();
    let (a, b) = tokio::join!(
        post(
            &router,
            id,
            "claim",
            "executor",
            json!({"claim_key":"claim","expected_revision":0,"expected":intent.expected})
        ),
        post(
            &router,
            id,
            "cancel",
            "alice",
            json!({"expected_revision":0})
        )
    );
    assert!(matches!(
        (a.0, b.0),
        (StatusCode::OK, StatusCode::CONFLICT) | (StatusCode::CONFLICT, StatusCode::OK)
    ));
    Ok(())
}

#[tokio::test]
async fn failed_event_insert_rolls_back_receipt_and_revision() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let pool = crate::db::init(tmp.path().join("fixture.db").to_str().unwrap()).await?;
    install(&pool).await?;
    let intent = intent();
    let router = app(pool.clone(), &intent);
    let op = created(&router, &intent).await;
    let id = op["operation_id"].as_i64().unwrap();
    let op = post(
        &router,
        id,
        "claim",
        "executor",
        json!({"claim_key":"claim","expected_revision":0,"expected":intent.expected}),
    )
    .await
    .1;
    sqlx::query("CREATE TRIGGER fixture_fail_event BEFORE INSERT ON events WHEN NEW.type='lifecycle.transition' BEGIN SELECT RAISE(ABORT,'fixture event failure'); END").execute(&pool).await?;
    assert_eq!(
        post(
            &router,
            id,
            "receipts",
            "executor",
            receipt_body(&op, &intent, "draining")
        )
        .await
        .0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    let row = sqlx::query("SELECT state,revision FROM lifecycle_operations WHERE id=?")
        .bind(id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(row.get::<String, _>("state"), "claimed");
    assert_eq!(row.get::<i64, _>("revision"), 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM lifecycle_receipts")
            .fetch_one(&pool)
            .await?,
        0
    );
    Ok(())
}

#[tokio::test]
async fn expired_claim_refuses_and_unclaimed_cancel_replays() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let pool = crate::db::init(tmp.path().join("fixture.db").to_str().unwrap()).await?;
    install(&pool).await?;
    let mut intent = intent();
    intent.forward_deadline_ms = chrono::Utc::now().timestamp_millis() + 200;
    let router = app(pool.clone(), &intent);
    let op = created(&router, &intent).await;
    let id = op["operation_id"].as_i64().unwrap();
    let delay =
        (intent.forward_deadline_ms - chrono::Utc::now().timestamp_millis()).max(0) as u64 + 5;
    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
    assert_eq!(
        post(
            &router,
            id,
            "claim",
            "executor",
            json!({"claim_key":"claim","expected_revision":0,"expected":intent.expected})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (status, cancelled) = post(
        &router,
        id,
        "cancel",
        "alice",
        json!({"expected_revision":0}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cancelled["active"], false);
    assert_eq!(
        post(
            &router,
            id,
            "cancel",
            "alice",
            json!({"expected_revision":0})
        )
        .await
        .1,
        cancelled
    );
    let (status, replay) = request(
        router,
        "POST",
        "/lifecycle/operations",
        Some("alice"),
        "fixture.test",
        json!({"request_key":"operation","intent":intent}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay, cancelled);
    Ok(())
}

async fn list(app: &Router, query: &str, who: Option<&str>) -> (StatusCode, Value) {
    request(
        app.clone(),
        "GET",
        &format!("/lifecycle/operations?{query}"),
        who,
        "fixture.test",
        Value::Null,
    )
    .await
}

#[tokio::test]
async fn discovery_is_executor_only_closed_and_bounded() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let pool = crate::db::init(tmp.path().join("fixture.db").to_str().unwrap()).await?;
    install(&pool).await?;
    let intent = intent();
    let app = app(pool.clone(), &intent);
    let op = created(&app, &intent).await;
    for who in [None, Some("alice"), Some("mallory")] {
        assert_eq!(
            list(&app, "host_id=fixture-host", who).await.0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        list(&app, "host_id=another-host", Some("executor")).await.0,
        StatusCode::FORBIDDEN
    );
    for query in [
        "host_id=fixture-host&limit=0",
        "host_id=fixture-host&limit=101",
        "host_id=fixture-host&after_id=-1",
        "host_id=fixture-host&after_id=2&through_id=1",
        "host_id=fixture-host&principal=executor",
        "host_id=fixture-host&state=claimed",
    ] {
        assert_eq!(
            list(&app, query, Some("executor")).await.0,
            StatusCode::BAD_REQUEST,
            "{query}"
        );
    }
    assert_eq!(
        request(
            app.clone(),
            "GET",
            "/lifecycle/operations?host_id=fixture-host",
            Some("executor"),
            "untrusted.test",
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let before = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM events")
        .fetch_one(&pool)
        .await?;
    let (status, page) = list(&app, "host_id=fixture-host", Some("executor")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["operations"], json!([op]));
    assert_eq!(page["has_more"], false);
    assert_eq!(page["next_after_id"], Value::Null);
    assert_eq!(
        before,
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM events")
            .fetch_one(&pool)
            .await?
    );
    Ok(())
}

#[tokio::test]
async fn discovery_filters_before_limit_and_freezes_visible_ceiling() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let pool = crate::db::init(tmp.path().join("fixture.db").to_str().unwrap()).await?;
    install(&pool).await?;
    let mut intents: Vec<Intent> = (0..7)
        .map(|n| {
            let mut i = intent();
            i.subject = format!("subject-{n}");
            i
        })
        .collect();
    intents[3].expected.host_id = "other-host".into();
    let mut grants = vec![
        executor_grant(&intents[1]),
        executor_grant(&intents[4]),
        executor_grant(&intents[6]),
    ];
    let mut wrong_build = executor_grant(&intents[2]);
    wrong_build.target_sha256 = "d".repeat(64);
    grants.push(wrong_build);
    let mut wrong_host = executor_grant(&intents[3]);
    wrong_host.host_id = "fixture-host".into();
    grants.push(wrong_host);
    let router = app_with_policy(
        pool.clone(),
        Policy {
            grants: intents.iter().map(requester_grant).collect(),
            executors: grants,
        },
        "x-fixture-user",
    );
    let mut ops = vec![];
    for i in &intents[..6] {
        ops.push(created(&router, i).await);
    }
    let (status, first) = list(&router, "host_id=fixture-host&limit=1", Some("executor")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["operations"], json!([ops[1]]));
    assert_eq!(first["has_more"], true);
    assert_eq!(
        first["through_id"], ops[4]["operation_id"],
        "unauthorized final row must not contribute a ceiling"
    );
    let late = created(&router, &intents[6]).await;
    let claim = json!({"claim_key":"claim","expected_revision":0,"expected":intents[1].expected});
    assert_eq!(
        post(
            &router,
            ops[1]["operation_id"].as_i64().unwrap(),
            "claim",
            "executor",
            claim
        )
        .await
        .0,
        StatusCode::OK
    );
    let query = format!(
        "host_id=fixture-host&limit=1&after_id={}&through_id={}",
        first["next_after_id"], first["through_id"]
    );
    let (_, second) = list(&router, &query, Some("executor")).await;
    assert_eq!(second["operations"], json!([ops[4]]));
    assert_eq!(second["has_more"], false);
    let (_, fresh) = list(&router, "host_id=fixture-host", Some("executor")).await;
    assert_eq!(fresh["operations"], json!([ops[4], late]));
    Ok(())
}

#[tokio::test]
async fn mandatory_host_scope_also_protects_get_claim_and_receipts() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let pool = crate::db::init(tmp.path().join("fixture.db").to_str().unwrap()).await?;
    install(&pool).await?;
    let intent = intent();
    let permitted = app(pool.clone(), &intent);
    let op = created(&permitted, &intent).await;
    let id = op["operation_id"].as_i64().unwrap();
    let mut grant = executor_grant(&intent);
    grant.host_id = "other-host".into();
    let wrong = app_with_policy(
        pool.clone(),
        Policy {
            grants: vec![requester_grant(&intent)],
            executors: vec![grant],
        },
        "x-fixture-user",
    );
    assert_eq!(
        request(
            wrong.clone(),
            "GET",
            &format!("/lifecycle/operations/{id}"),
            Some("executor"),
            "fixture.test",
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let claim = json!({"claim_key":"claim","expected_revision":0,"expected":intent.expected});
    assert_eq!(
        post(&wrong, id, "claim", "executor", claim.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    let (status, claimed) = post(&permitted, id, "claim", "executor", claim).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        post(
            &wrong,
            id,
            "receipts",
            "executor",
            receipt_body(&claimed, &intent, "draining")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        list(&wrong, "host_id=other-host", Some("executor")).await.1["operations"],
        json!([])
    );
    Ok(())
}

#[tokio::test]
async fn discovery_expiry_does_not_release_or_authorize_stale_claim() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let pool = crate::db::init(tmp.path().join("fixture.db").to_str().unwrap()).await?;
    install(&pool).await?;
    let mut intent = intent();
    intent.forward_deadline_ms = chrono::Utc::now().timestamp_millis() + 200;
    let app = app(pool.clone(), &intent);
    let op = created(&app, &intent).await;
    let id = op["operation_id"].as_i64().unwrap();
    assert_eq!(
        list(&app, "host_id=fixture-host", Some("executor")).await.1["operations"],
        json!([op])
    );
    let wait =
        (intent.forward_deadline_ms - chrono::Utc::now().timestamp_millis()).max(0) as u64 + 5;
    tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
    assert_eq!(
        list(&app, "host_id=fixture-host", Some("executor")).await.1["operations"],
        json!([])
    );
    let (_, read) = request(
        app.clone(),
        "GET",
        &format!("/lifecycle/operations/{id}"),
        Some("executor"),
        "fixture.test",
        Value::Null,
    )
    .await;
    assert_eq!(read["active"], true);
    assert_eq!(read["state"], "requested");
    assert_eq!(
        post(
            &app,
            id,
            "claim",
            "executor",
            json!({"claim_key":"stale","expected_revision":0,"expected":intent.expected})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    Ok(())
}

async fn guard(app: &Router, subject: &str, host: &str, who: Option<&str>) -> (StatusCode, Value) {
    request(
        app.clone(),
        "GET",
        &format!("/lifecycle/subjects/{subject}/guard?host_id={host}"),
        who,
        "fixture.test",
        Value::Null,
    )
    .await
}

#[tokio::test]
async fn subject_guard_requires_exact_executor_subject_and_host() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let pool = crate::db::init(tmp.path().join("fixture.db").to_str().unwrap()).await?;
    install(&pool).await?;
    let intent = intent();
    let router = app(pool.clone(), &intent);
    for who in [None, Some("alice"), Some("mallory")] {
        assert_eq!(
            guard(&router, &intent.subject, "fixture-host", who).await.0,
            StatusCode::FORBIDDEN
        );
    }
    for (subject, host) in [
        ("other-subject", "fixture-host"),
        ("fixture-agent", "other-host"),
    ] {
        assert_eq!(
            guard(&router, subject, host, Some("executor")).await.0,
            StatusCode::FORBIDDEN
        );
    }
    for suffix in ["", "?host_id=", "?host_id=fixture-host&principal=executor"] {
        let path = format!("/lifecycle/subjects/fixture-agent/guard{suffix}");
        assert_eq!(
            request(
                router.clone(),
                "GET",
                &path,
                Some("executor"),
                "fixture.test",
                Value::Null
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        request(
            router.clone(),
            "GET",
            "/lifecycle/subjects/fixture-agent/guard?host_id=fixture-host",
            Some("executor"),
            "untrusted.test",
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        guard(&router, &intent.subject, "fixture-host", Some("executor")).await,
        (StatusCode::OK, json!({"guard":"clear"}))
    );
    Ok(())
}

#[tokio::test]
async fn subject_guard_blocks_every_forward_state_and_clears_only_inactive() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let pool = crate::db::init(tmp.path().join("fixture.db").to_str().unwrap()).await?;
    install(&pool).await?;
    let intent = intent();
    let router = app(pool.clone(), &intent);
    let mut op = created(&router, &intent).await;
    let id = op["operation_id"].as_i64().unwrap();
    let (_, g) = guard(&router, &intent.subject, "fixture-host", Some("executor")).await;
    assert_eq!(
        g,
        json!({"guard":"blocked","operation":{"operation_id":id,"state":"requested"}})
    );
    op = post(
        &router,
        id,
        "claim",
        "executor",
        json!({"claim_key":"claim","expected_revision":0,"expected":intent.expected}),
    )
    .await
    .1;
    for state in [
        "claimed",
        "draining",
        "stop_intent",
        "stopped",
        "launch_intent",
        "verifying",
        "completed",
    ] {
        if state != "claimed" {
            let (s, v) = post(
                &router,
                id,
                "receipts",
                "executor",
                receipt_body(&op, &intent, state),
            )
            .await;
            assert_eq!(s, StatusCode::OK);
            op = v;
        }
        let before = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM events")
            .fetch_one(&pool)
            .await?;
        let (status, g) = guard(&router, &intent.subject, "fixture-host", Some("executor")).await;
        assert_eq!(status, StatusCode::OK);
        if state == "completed" {
            assert_eq!(g, json!({"guard":"clear"}));
        } else {
            assert_eq!(
                g,
                json!({"guard":"blocked","operation":{"operation_id":id,"state":state}})
            );
        }
        assert_eq!(
            before,
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM events")
                .fetch_one(&pool)
                .await?
        );
    }
    Ok(())
}

#[tokio::test]
async fn subject_guard_keeps_expired_and_held_operations_blocked() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let pool = crate::db::init(tmp.path().join("fixture.db").to_str().unwrap()).await?;
    install(&pool).await?;
    let mut intent = intent();
    intent.forward_deadline_ms = chrono::Utc::now().timestamp_millis() + 300;
    let router = app(pool.clone(), &intent);
    let op = created(&router, &intent).await;
    let id = op["operation_id"].as_i64().unwrap();
    let wait =
        (intent.forward_deadline_ms - chrono::Utc::now().timestamp_millis()).max(0) as u64 + 5;
    tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
    assert_eq!(
        guard(&router, &intent.subject, "fixture-host", Some("executor"))
            .await
            .1["guard"],
        "blocked"
    );
    assert_eq!(
        post(
            &router,
            id,
            "cancel",
            "alice",
            json!({"expected_revision":0})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        guard(&router, &intent.subject, "fixture-host", Some("executor"))
            .await
            .1,
        json!({"guard":"clear"})
    );
    intent.forward_deadline_ms = chrono::Utc::now().timestamp_millis() + 60_000;
    let (status, new) = request(
        router.clone(),
        "POST",
        "/lifecycle/operations",
        Some("alice"),
        "fixture.test",
        json!({"request_key":"held","intent":intent}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = new["operation_id"].as_i64().unwrap();
    let claimed = post(
        &router,
        id,
        "claim",
        "executor",
        json!({"claim_key":"claim","expected_revision":0,"expected":intent.expected}),
    )
    .await
    .1;
    let mut b = receipt_body(&claimed, &intent, "draining");
    b["evidence"] = json!({"kind":"held_unknown","reason":"uncertain fixture result"});
    assert_eq!(
        post(&router, id, "receipts", "executor", b).await.0,
        StatusCode::OK
    );
    assert_eq!(
        guard(&router, &intent.subject, "fixture-host", Some("executor"))
            .await
            .1,
        json!({"guard":"blocked","operation":{"operation_id":id,"state":"held_unknown"}})
    );
    Ok(())
}

#[tokio::test]
async fn subject_guard_hides_mismatched_build_effect_and_host_details() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let pool = crate::db::init(tmp.path().join("fixture.db").to_str().unwrap()).await?;
    install(&pool).await?;
    let intent = intent();
    let allowed = app(pool.clone(), &intent);
    created(&allowed, &intent).await;
    for mismatch in ["build", "effect", "host"] {
        let mut grant = executor_grant(&intent);
        match mismatch {
            "build" => grant.target_sha256 = "d".repeat(64),
            "effect" => grant.effect = Effect::Restart,
            _ => grant.host_id = "other-host".into(),
        }
        let host = grant.host_id.clone();
        let router = app_with_policy(
            pool.clone(),
            Policy {
                grants: vec![],
                executors: vec![grant],
            },
            "x-fixture-user",
        );
        assert_eq!(
            guard(&router, &intent.subject, &host, Some("executor")).await,
            (StatusCode::OK, json!({"guard":"blocked"})),
            "{mismatch}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn subject_guard_failures_and_races_never_masquerade_as_clear() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let pool = crate::db::init(tmp.path().join("fixture.db").to_str().unwrap()).await?;
    install(&pool).await?;
    let intent = intent();
    let router = app(pool.clone(), &intent);
    let (before, op) = tokio::join!(
        guard(&router, &intent.subject, "fixture-host", Some("executor")),
        created(&router, &intent)
    );
    assert_eq!(before.0, StatusCode::OK);
    assert!(matches!(
        before.1["guard"].as_str(),
        Some("clear" | "blocked")
    ));
    let id = op["operation_id"].as_i64().unwrap();
    assert_eq!(
        guard(&router, &intent.subject, "fixture-host", Some("executor"))
            .await
            .1["guard"],
        "blocked"
    );
    let (during, cancelled) = tokio::join!(
        guard(&router, &intent.subject, "fixture-host", Some("executor")),
        post(
            &router,
            id,
            "cancel",
            "alice",
            json!({"expected_revision":0})
        )
    );
    assert_eq!(during.0, StatusCode::OK);
    assert!(matches!(
        during.1["guard"].as_str(),
        Some("clear" | "blocked")
    ));
    assert_eq!(cancelled.0, StatusCode::OK);
    assert_eq!(
        guard(&router, &intent.subject, "fixture-host", Some("executor"))
            .await
            .1,
        json!({"guard":"clear"})
    );
    // Failure injection is confined to this temporary database. Corrupt active intent must
    // return an error, never silently fall through to no matching active operation.
    sqlx::query(
        "UPDATE lifecycle_operations SET active=1,state='requested',intent='invalid' WHERE id=?",
    )
    .bind(id)
    .execute(&pool)
    .await?;
    let (status, body) = guard(&router, &intent.subject, "fixture-host", Some("executor")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_ne!(body, json!({"guard":"clear"}));
    sqlx::query("DROP TABLE lifecycle_receipts")
        .execute(&pool)
        .await?;
    sqlx::query("DROP TABLE lifecycle_operations")
        .execute(&pool)
        .await?;
    assert_eq!(
        guard(&router, &intent.subject, "fixture-host", Some("executor"))
            .await
            .0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    Ok(())
}

async fn verifying_fixture(router: &Router, intent: &Intent) -> (Value, Value, Value) {
    let mut op = created(router, intent).await;
    assert!(op.as_object().unwrap().contains_key("new_binding"));
    assert!(op["new_binding"].is_null());
    let id = op["operation_id"].as_i64().unwrap();
    let claim = json!({"claim_key":"claim","expected_revision":0,"expected":intent.expected});
    let (status, value) = post(router, id, "claim", "executor", claim.clone()).await;
    assert_eq!(status, StatusCode::OK);
    op = value;
    let mut last = Value::Null;
    for state in [
        "draining",
        "stop_intent",
        "stopped",
        "launch_intent",
        "verifying",
    ] {
        last = receipt_body(&op, intent, state);
        let (status, value) = post(router, id, "receipts", "executor", last.clone()).await;
        assert_eq!(status, StatusCode::OK);
        op = value;
        if state != "verifying" {
            assert!(op["new_binding"].is_null());
        }
    }
    (op, claim, last)
}

#[tokio::test]
async fn replacement_projection_is_persistent_authorized_and_replay_is_historical(
) -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let path = tmp.path().join("fixture.db");
    let pool = crate::db::init(path.to_str().unwrap()).await?;
    install(&pool).await?;
    let intent = intent();
    let router = app(pool.clone(), &intent);
    let (verified, claim, verify_receipt) = verifying_fixture(&router, &intent).await;
    let id = verified["operation_id"].as_i64().unwrap();
    let expected = facts("verifying", &intent)["new_binding"].clone();
    assert_eq!(verified["new_binding"], expected);
    let route = format!("/lifecycle/operations/{id}");
    for who in ["alice", "executor"] {
        let (s, v) = request(
            router.clone(),
            "GET",
            &route,
            Some(who),
            "fixture.test",
            Value::Null,
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v["new_binding"], expected);
    }
    assert_eq!(
        request(
            router.clone(),
            "GET",
            &route,
            Some("mallory"),
            "fixture.test",
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let mut wrong = executor_grant(&intent);
    wrong.host_id = "elsewhere".into();
    let other = app_with_policy(
        pool.clone(),
        Policy {
            grants: vec![],
            executors: vec![wrong],
        },
        "x-fixture-user",
    );
    assert_eq!(
        request(
            other,
            "GET",
            &route,
            Some("executor"),
            "fixture.test",
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    drop(router);
    pool.close().await;
    let pool = crate::db::init(path.to_str().unwrap()).await?;
    install(&pool).await?;
    let router = app(pool.clone(), &intent);
    let (s, reopened) = request(
        router.clone(),
        "GET",
        &route,
        Some("executor"),
        "fixture.test",
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(reopened, verified);
    let (s, completed) = post(
        &router,
        id,
        "receipts",
        "executor",
        receipt_body(&verified, &intent, "completed"),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(completed["new_binding"], expected);
    assert_eq!(
        post(&router, id, "receipts", "executor", verify_receipt)
            .await
            .1,
        verified,
        "receipt replay is its original snapshot"
    );
    let original_claim = post(&router, id, "claim", "executor", claim).await.1;
    assert_eq!(original_claim["state"], "claimed");
    assert!(original_claim["new_binding"].is_null());
    assert_eq!(
        request(
            router,
            "GET",
            &route,
            Some("executor"),
            "fixture.test",
            Value::Null
        )
        .await
        .1,
        completed
    );
    Ok(())
}

#[tokio::test]
async fn replacement_projection_rejects_malformed_or_inconsistent_storage() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let pool = crate::db::init(tmp.path().join("fixture.db").to_str().unwrap()).await?;
    install(&pool).await?;
    let intent = intent();
    let router = app(pool.clone(), &intent);
    let (verified, _, _) = verifying_fixture(&router, &intent).await;
    let id = verified["operation_id"].as_i64().unwrap();
    let mut bad_values: Vec<Option<String>> = vec![
        None,
        Some("invalid".into()),
        Some("null".into()),
        Some("{}".into()),
    ];
    for (key, value) in [
        ("pid", json!(0)),
        ("start_ticks", json!(0)),
        ("start_ticks", json!(intent.expected.start_ticks)),
        ("host_id", json!("wrong-host")),
        ("boot_id", json!("wrong-boot")),
        ("thread_id", json!("wrong-thread")),
        ("exe_sha256", json!("wrong")),
        ("exe_sha256", json!("d".repeat(64))),
        ("unexpected", json!(true)),
    ] {
        let mut invalid = verified["new_binding"].clone();
        invalid[key] = value;
        bad_values.push(Some(invalid.to_string()));
    }
    for value in bad_values {
        sqlx::query("UPDATE lifecycle_operations SET new_binding=? WHERE id=?")
            .bind(value)
            .bind(id)
            .execute(&pool)
            .await?;
        let (status, body) = request(
            router.clone(),
            "GET",
            &format!("/lifecycle/operations/{id}"),
            Some("executor"),
            "fixture.test",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(body.get("new_binding").is_none());
    }
    sqlx::query("UPDATE lifecycle_operations SET state='claimed',new_binding=? WHERE id=?")
        .bind(verified["new_binding"].to_string())
        .bind(id)
        .execute(&pool)
        .await?;
    assert_eq!(
        request(
            router,
            "GET",
            &format!("/lifecycle/operations/{id}"),
            Some("executor"),
            "fixture.test",
            Value::Null
        )
        .await
        .0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    Ok(())
}

#[tokio::test]
async fn held_unknown_retains_valid_replacement_without_release_permission() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let pool = crate::db::init(tmp.path().join("fixture.db").to_str().unwrap()).await?;
    install(&pool).await?;
    let intent = intent();
    let router = app(pool.clone(), &intent);
    let (verified, _, _) = verifying_fixture(&router, &intent).await;
    let id = verified["operation_id"].as_i64().unwrap();
    let mut b = receipt_body(&verified, &intent, "completed");
    b["receipt_key"] = json!("uncertain");
    b["effect_id"] = json!("uncertain");
    b["evidence"] =
        json!({"kind":"held_unknown","reason":"fixture uncertainty after verification"});
    let (status, held) = post(&router, id, "receipts", "executor", b).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(held["state"], "held_unknown");
    assert_eq!(held["new_binding"], verified["new_binding"]);
    assert_eq!(held["active"], true);
    assert_eq!(
        guard(&router, &intent.subject, "fixture-host", Some("executor"))
            .await
            .1["guard"],
        "blocked"
    );
    Ok(())
}
async fn request(
    app: Router,
    method: &str,
    path: &str,
    who: Option<&str>,
    host: &str,
    body: Value,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("host", host)
        .header("content-type", "application/json");
    if let Some(who) = who {
        req = req.header("x-fixture-user", who);
    }
    let response = app
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(json!({"text":String::from_utf8_lossy(&bytes)})),
    )
}

#[tokio::test]
async fn actual_router_binds_identity_and_rejects_scope_and_unknown_fields() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let pool = crate::db::init(temp.path().join("fixture.db").to_str().unwrap()).await?;
    install(&pool).await?;
    let intent = intent();
    let app = app(pool.clone(), &intent);
    let body = json!({"request_key":"one","intent":intent});
    for (who, host, expected) in [
        (None, "fixture.test", StatusCode::UNAUTHORIZED),
        (Some("alice"), "untrusted.test", StatusCode::FORBIDDEN),
        (Some("mallory"), "fixture.test", StatusCode::FORBIDDEN),
    ] {
        assert_eq!(
            request(
                app.clone(),
                "POST",
                "/lifecycle/operations",
                who,
                host,
                body.clone()
            )
            .await
            .0,
            expected
        );
    }
    let mut forged = body.clone();
    forged["principal"] = json!("alice");
    assert_eq!(
        request(
            app.clone(),
            "POST",
            "/lifecycle/operations",
            Some("alice"),
            "fixture.test",
            forged
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let mut outside = body.clone();
    outside["intent"]["target_sha256"] = json!("d".repeat(64));
    assert_eq!(
        request(
            app.clone(),
            "POST",
            "/lifecycle/operations",
            Some("alice"),
            "fixture.test",
            outside
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let mut invalid = body.clone();
    invalid["intent"]["expected"]["pid"] = json!(0);
    assert_eq!(
        request(
            app.clone(),
            "POST",
            "/lifecycle/operations",
            Some("alice"),
            "fixture.test",
            invalid
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let (status, created) = request(
        app.clone(),
        "POST",
        "/lifecycle/operations",
        Some("alice"),
        "fixture.test",
        body,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(created["requester"], "alice");
    assert_eq!(created["state"], "requested");
    let path = format!("/lifecycle/operations/{}", created["operation_id"]);
    assert_eq!(
        request(
            app.clone(),
            "GET",
            &path,
            Some("mallory"),
            "fixture.test",
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            app,
            "GET",
            &path,
            Some("alice"),
            "fixture.test",
            Value::Null
        )
        .await
        .1,
        created
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM lifecycle_operations")
            .fetch_one(&pool)
            .await?,
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM events WHERE type='lifecycle.requested'"
        )
        .fetch_one(&pool)
        .await?,
        1
    );
    Ok(())
}

#[tokio::test]
async fn concurrent_http_replays_and_subject_conflicts_persist_after_reopen() -> anyhow::Result<()>
{
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("fixture.db");
    let pool = crate::db::init(path.to_str().unwrap()).await?;
    install(&pool).await?;
    let intent = intent();
    let router = app(pool.clone(), &intent);
    let body = json!({"request_key":"same","intent":intent});
    let mut handles = vec![];
    for _ in 0..8 {
        let app = router.clone();
        let b = body.clone();
        handles.push(tokio::spawn(async move {
            request(
                app,
                "POST",
                "/lifecycle/operations",
                Some("alice"),
                "fixture.test",
                b,
            )
            .await
        }));
    }
    let mut id = Value::Null;
    for h in handles {
        let (status, value) = h.await?;
        assert_eq!(status, StatusCode::OK);
        if id.is_null() {
            id = value["operation_id"].clone();
        }
        assert_eq!(id, value["operation_id"]);
    }
    let mut changed = body.clone();
    changed["intent"]["expected"]["start_ticks"] = json!(124);
    assert_eq!(
        request(
            router.clone(),
            "POST",
            "/lifecycle/operations",
            Some("alice"),
            "fixture.test",
            changed
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let mut distinct = body.clone();
    distinct["request_key"] = json!("other");
    assert_eq!(
        request(
            router.clone(),
            "POST",
            "/lifecycle/operations",
            Some("alice"),
            "fixture.test",
            distinct
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    drop(router);
    pool.close().await;
    let reopened = crate::db::init(path.to_str().unwrap()).await?;
    install(&reopened).await?;
    let (status, value) = request(
        app(reopened.clone(), &intent),
        "POST",
        "/lifecycle/operations",
        Some("alice"),
        "fixture.test",
        body,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["operation_id"], id);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM events WHERE type='lifecycle.requested'"
        )
        .fetch_one(&reopened)
        .await?,
        1
    );
    Ok(())
}

#[tokio::test]
async fn distinct_concurrent_keys_allow_exactly_one_active_subject() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let pool = crate::db::init(temp.path().join("fixture.db").to_str().unwrap()).await?;
    install(&pool).await?;
    let intent = intent();
    let router = app(pool.clone(), &intent);
    let mut handles = vec![];
    for i in 0..8 {
        let r = router.clone();
        let b = json!({"request_key":format!("key-{i}"),"intent":intent});
        handles.push(tokio::spawn(async move {
            request(
                r,
                "POST",
                "/lifecycle/operations",
                Some("alice"),
                "fixture.test",
                b,
            )
            .await
            .0
        }));
    }
    let mut wins = 0;
    for h in handles {
        match h.await? {
            StatusCode::OK => wins += 1,
            StatusCode::CONFLICT => {}
            s => panic!("unexpected {s}"),
        }
    }
    assert_eq!(wins, 1);
    Ok(())
}

//! Additive, isolated lifecycle API. Production startup does not install or mount it.
//! Header provenance is supplied by the existing trusted front door, not this module.
use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::Row;

use crate::{api::ForcedViewer, db::Pool};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Binding {
    pub host_id: String,
    pub boot_id: String,
    pub pid: u32,
    pub start_ticks: u64,
    pub exe_sha256: String,
    pub thread_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Effect {
    Restart,
    Upgrade,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Intent {
    pub subject: String,
    pub expected: Binding,
    pub target_sha256: String,
    pub effect: Effect,
    pub forward_deadline_ms: i64,
    pub recovery_deadline_ms: i64,
    // Recovery dispatch is deliberately unsupported in the initial contract.
    pub recovery_effects: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {
    request_key: String,
    intent: Intent,
}

#[derive(Clone)]
pub(crate) struct Grant {
    pub requester: String,
    pub subject: String,
    pub target_sha256: String,
    pub effect: Effect,
}

/// Executor scope always names a host; absent host is never a wildcard.
#[derive(Clone)]
pub(crate) struct ExecutorGrant {
    pub requester: String,
    pub host_id: String,
    pub subject: String,
    pub target_sha256: String,
    pub effect: Effect,
}

/// Server-owned configuration, never supplied in an API body. Empty denies all.
#[derive(Clone, Default)]
pub(crate) struct Policy {
    pub grants: Vec<Grant>,
    pub executors: Vec<ExecutorGrant>,
}

#[derive(Clone)]
struct Service {
    pool: Pool,
    policy: Arc<Policy>,
}

#[derive(Debug)]
struct Error(StatusCode, &'static str);
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}
impl From<sqlx::Error> for Error {
    fn from(_: sqlx::Error) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, "storage failure")
    }
}
type Result<T> = std::result::Result<T, Error>;
fn bad(message: &'static str) -> Error {
    Error(StatusCode::BAD_REQUEST, message)
}
fn conflict(message: &'static str) -> Error {
    Error(StatusCode::CONFLICT, message)
}
fn forbidden() -> Error {
    Error(
        StatusCode::FORBIDDEN,
        "trusted caller and explicit grant required",
    )
}
fn text_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 256 && s.trim() == s && !s.chars().any(char::is_control)
}
fn hash_ok(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Binding {
    fn validate(&self) -> Result<()> {
        if !text_ok(&self.host_id)
            || !text_ok(&self.boot_id)
            || !text_ok(&self.thread_id)
            || self.pid == 0
            || self.start_ticks == 0
            || !hash_ok(&self.exe_sha256)
        {
            return Err(bad("invalid process identity"));
        }
        Ok(())
    }
}
impl Intent {
    fn validate(&self) -> Result<()> {
        self.expected.validate()?;
        if !text_ok(&self.subject)
            || !hash_ok(&self.target_sha256)
            || !self.recovery_effects.is_empty()
            || self.forward_deadline_ms <= 0
            || self.recovery_deadline_ms < self.forward_deadline_ms
        {
            return Err(bad("invalid intent or unsupported recovery effects"));
        }
        if self.effect == Effect::Restart && self.expected.exe_sha256 != self.target_sha256 {
            return Err(bad("restart must retain the executable digest"));
        }
        Ok(())
    }
}
impl Policy {
    fn permits(&self, caller: &str, intent: &Intent) -> bool {
        self.grants.iter().any(|g| {
            g.requester == caller
                && g.subject == intent.subject
                && g.target_sha256 == intent.target_sha256
                && g.effect == intent.effect
        })
    }
    fn executor(&self, caller: &str, intent: &Intent) -> bool {
        self.executors.iter().any(|g| {
            g.requester == caller
                && g.host_id == intent.expected.host_id
                && g.subject == intent.subject
                && g.target_sha256 == intent.target_sha256
                && g.effect == intent.effect
        })
    }
}

/// Explicit schema installation for an isolated DB; not called by production open/startup.
#[allow(dead_code)]
pub(crate) async fn install(pool: &Pool) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    for sql in [
        "CREATE TABLE IF NOT EXISTS lifecycle_operations(id INTEGER PRIMARY KEY AUTOINCREMENT, requester TEXT NOT NULL, subject TEXT NOT NULL, request_key TEXT NOT NULL, intent TEXT NOT NULL, state TEXT NOT NULL, revision INTEGER NOT NULL, active INTEGER NOT NULL CHECK(active IN(0,1)), created_at_ms INTEGER NOT NULL, UNIQUE(requester,subject,request_key))",
        "CREATE UNIQUE INDEX IF NOT EXISTS lifecycle_active_subject ON lifecycle_operations(subject) WHERE active=1",
        "CREATE TABLE IF NOT EXISTS lifecycle_receipts(operation_id INTEGER NOT NULL REFERENCES lifecycle_operations(id), receipt_key TEXT NOT NULL, effect_id TEXT NOT NULL, payload TEXT NOT NULL, result TEXT NOT NULL, PRIMARY KEY(operation_id,receipt_key), UNIQUE(operation_id,effect_id))",
    ] { sqlx::query(sql).execute(&mut *tx).await?; }
    let columns = sqlx::query("PRAGMA table_info(lifecycle_operations)")
        .fetch_all(&mut *tx)
        .await?;
    for (name, definition) in [
        ("executor", "TEXT"),
        ("claim_token", "TEXT"),
        ("claim_key", "TEXT"),
        ("claim_payload", "TEXT"),
        ("claim_result", "TEXT"),
        ("last_observed_ms", "INTEGER"),
        ("updated_at_ms", "INTEGER"),
        ("new_binding", "TEXT"),
    ] {
        if !columns.iter().any(|r| r.get::<String, _>("name") == name) {
            sqlx::query(&format!(
                "ALTER TABLE lifecycle_operations ADD COLUMN {name} {definition}"
            ))
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

#[allow(dead_code)]
pub(crate) fn router(pool: Pool, policy: Policy) -> Router {
    Router::new()
        .route("/lifecycle/operations", post(create).get(pending))
        .route("/lifecycle/operations/{id}", get(read))
        .route("/lifecycle/operations/{id}/claim", post(claim))
        .route("/lifecycle/operations/{id}/receipts", post(receipt))
        .route("/lifecycle/operations/{id}/cancel", post(cancel))
        .route("/lifecycle/subjects/{subject}/guard", get(subject_guard))
        .with_state(Service {
            pool,
            policy: Arc::new(policy),
        })
}

fn caller(identity: Option<Extension<ForcedViewer>>) -> Result<String> {
    identity
        .map(|Extension(ForcedViewer(who))| who)
        .filter(|s| text_ok(s))
        .ok_or_else(forbidden)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingQuery {
    host_id: String,
    #[serde(default)]
    after_id: i64,
    #[serde(default = "pending_limit")]
    limit: i64,
    through_id: Option<i64>,
}
fn pending_limit() -> i64 {
    20
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GuardQuery {
    host_id: String,
}

/// An authorized current-state guard, NOT a launch reservation or effect permit.
/// Every globally active operation on this subject blocks, including an expired
/// request or an operation the caller is not allowed to inspect in full.
async fn subject_guard(
    State(s): State<Service>,
    identity: Option<Extension<ForcedViewer>>,
    Path(subject): Path<String>,
    Query(query): Query<GuardQuery>,
) -> Result<Json<Value>> {
    let caller = caller(identity)?;
    if !text_ok(&subject) || !text_ok(&query.host_id) {
        return Err(bad("invalid subject or host"));
    }
    if !s
        .policy
        .executors
        .iter()
        .any(|g| g.requester == caller && g.host_id == query.host_id && g.subject == subject)
    {
        return Err(forbidden());
    }
    // Do not filter active rows by deadline, state, host, build or effect: the
    // unique active-subject constraint spans them all. One SELECT is the snapshot.
    let row = sqlx::query("SELECT * FROM lifecycle_operations WHERE subject=? AND active=1")
        .bind(&subject)
        .fetch_optional(&s.pool)
        .await?;
    let Some(row) = row else {
        return Ok(Json(json!({"guard":"clear"})));
    };
    let intent = stored_intent(&row)?;
    if s.policy.executor(&caller, &intent) {
        Ok(Json(json!({"guard":"blocked","operation":{
            "operation_id":row.try_get::<i64,_>("id")?,"state":row.try_get::<String,_>("state")?
        }})))
    } else {
        Ok(Json(json!({"guard":"blocked"})))
    }
}

async fn pending(
    State(s): State<Service>,
    identity: Option<Extension<ForcedViewer>>,
    Query(query): Query<PendingQuery>,
) -> Result<Json<Value>> {
    let caller = caller(identity)?;
    if !text_ok(&query.host_id)
        || query.after_id < 0
        || !(1..=100).contains(&query.limit)
        || query.through_id.is_some_and(|id| id < query.after_id)
    {
        return Err(bad("invalid host or pagination bounds"));
    }
    let grants: Vec<Value> = s
        .policy
        .executors
        .iter()
        .filter(|g| g.requester == caller && g.host_id == query.host_id)
        .map(|g| json!({"subject":g.subject,"target_sha256":g.target_sha256,"effect":g.effect}))
        .collect();
    if grants.is_empty() {
        return Err(forbidden());
    }
    let scope = serde_json::to_string(&grants).map_err(|_| bad("invalid executor scope"))?;
    // Apply exact server-owned scope in SQL BEFORE pagination; neither rows nor the
    // first-page ceiling are selected from another executor's scope.
    const FILTER: &str = "o.active=1 AND o.state='requested' AND json_extract(o.intent,'$.expected.host_id')=? AND json_extract(o.intent,'$.forward_deadline_ms')>? AND EXISTS (SELECT 1 FROM json_each(?) AS g WHERE json_extract(g.value,'$.subject')=o.subject AND json_extract(g.value,'$.target_sha256')=json_extract(o.intent,'$.target_sha256') AND json_extract(g.value,'$.effect')=json_extract(o.intent,'$.effect'))";
    let now = chrono::Utc::now().timestamp_millis();
    let mut tx = s.pool.begin().await?;
    let through = match query.through_id {
        Some(id) => id,
        None => sqlx::query_scalar::<_, i64>(&format!(
            "SELECT COALESCE(MAX(o.id),0) FROM lifecycle_operations o WHERE {FILTER}"
        ))
        .bind(&query.host_id)
        .bind(now)
        .bind(&scope)
        .fetch_one(&mut *tx)
        .await?
        .max(query.after_id),
    };
    let mut rows=sqlx::query(&format!("SELECT o.* FROM lifecycle_operations o WHERE {FILTER} AND o.id>? AND o.id<=? ORDER BY o.id ASC LIMIT ?"))
        .bind(&query.host_id).bind(now).bind(&scope).bind(query.after_id).bind(through).bind(query.limit+1)
        .fetch_all(&mut *tx).await?;
    let has_more = rows.len() > query.limit as usize;
    rows.truncate(query.limit as usize);
    let next_after_id = if has_more {
        rows.last().map(|r| r.try_get::<i64, _>("id")).transpose()?
    } else {
        None
    };
    let operations: Vec<Value> = rows.iter().map(view).collect::<Result<_>>()?;
    tx.commit().await?;
    Ok(Json(
        json!({"operations":operations,"through_id":through,"next_after_id":next_after_id,"has_more":has_more}),
    ))
}
fn view(row: &sqlx::sqlite::SqliteRow) -> Result<Value> {
    let intent = stored_intent(row)?;
    let state: String = row.try_get("state")?;
    let new_binding = validated_stored_replacement(row, &intent, &state)?;
    Ok(
        json!({"operation_id":row.try_get::<i64,_>("id")?, "requester":row.try_get::<String,_>("requester")?,
        "intent":intent,"state":row.try_get::<String,_>("state")?,"revision":row.try_get::<i64,_>("revision")?,
        "active":row.try_get::<i64,_>("active")? == 1,"created_at_ms":row.try_get::<i64,_>("created_at_ms")?,
        "executor":row.try_get::<Option<String>,_>("executor")?,"claim_token":row.try_get::<Option<String>,_>("claim_token")?,
        "new_binding":new_binding}),
    )
}

/// Project persisted replacement identity only after validating it against immutable
/// intent. Corruption is a server failure, never a null or caller-supplied fallback.
fn validated_stored_replacement(
    row: &sqlx::sqlite::SqliteRow,
    intent: &Intent,
    state: &str,
) -> Result<Option<Binding>> {
    let invalid = || {
        Error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "invalid stored replacement binding",
        )
    };
    let raw: Option<String> = row.try_get("new_binding")?;
    let Some(raw) = raw else {
        if matches!(state, "verifying" | "completed") {
            return Err(invalid());
        }
        return Ok(None);
    };
    if !matches!(state, "verifying" | "completed" | "held_unknown") {
        return Err(invalid());
    }
    intent.validate().map_err(|_| invalid())?;
    let binding: Binding = serde_json::from_str(&raw).map_err(|_| invalid())?;
    binding.validate().map_err(|_| invalid())?;
    if binding.host_id != intent.expected.host_id
        || binding.boot_id != intent.expected.boot_id
        || binding.thread_id != intent.expected.thread_id
        || binding.start_ticks == intent.expected.start_ticks
        || binding.exe_sha256 != intent.target_sha256
    {
        return Err(invalid());
    }
    Ok(Some(binding))
}

async fn create(
    State(s): State<Service>,
    identity: Option<Extension<ForcedViewer>>,
    Json(body): Json<Create>,
) -> Result<Json<Value>> {
    let caller = caller(identity)?;
    body.intent.validate()?;
    if !text_ok(&body.request_key) {
        return Err(bad("invalid request key"));
    }
    if !s.policy.permits(&caller, &body.intent) {
        return Err(forbidden());
    }
    let encoded = serde_json::to_string(&body.intent).map_err(|_| bad("invalid intent"))?;
    let mut tx = s.pool.begin().await?;
    // Existing Board pool is one connection. The conditional insert and unique indexes also
    // prevent duplicate rows if another writer exists; any contention fails without an effect.
    if let Some(row) = sqlx::query(
        "SELECT * FROM lifecycle_operations WHERE requester=? AND subject=? AND request_key=?",
    )
    .bind(&caller)
    .bind(&body.intent.subject)
    .bind(&body.request_key)
    .fetch_optional(&mut *tx)
    .await?
    {
        if row.try_get::<String, _>("intent")? != encoded {
            return Err(conflict("idempotency key reused with changed intent"));
        }
        let value = view(&row)?;
        tx.commit().await?;
        return Ok(Json(value));
    }
    let now = chrono::Utc::now().timestamp_millis();
    if body.intent.forward_deadline_ms <= now {
        return Err(conflict("forward deadline expired"));
    }
    if sqlx::query("SELECT 1 FROM lifecycle_operations WHERE subject=? AND active=1")
        .bind(&body.intent.subject)
        .fetch_optional(&mut *tx)
        .await?
        .is_some()
    {
        return Err(conflict("subject already has an active operation"));
    }
    let row = sqlx::query("INSERT INTO lifecycle_operations(requester,subject,request_key,intent,state,revision,active,created_at_ms) VALUES(?,?,?,?,'requested',0,1,?) RETURNING *")
        .bind(&caller).bind(&body.intent.subject).bind(&body.request_key).bind(encoded).bind(now)
        .fetch_one(&mut *tx).await?;
    let value = view(&row)?;
    let mut hooks = Vec::new();
    crate::events::emit(
        &mut tx,
        &mut hooks,
        "lifecycle.requested",
        Some(&caller),
        None,
        None,
        None,
        None,
        json!({"operation_id":value["operation_id"],"subject":body.intent.subject,"revision":0}),
        crate::events::Recipients::Explicit(std::collections::BTreeSet::from([caller.clone()])),
    )
    .await
    .map_err(|_| {
        Error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "event persistence failed",
        )
    })?;
    tx.commit().await?;
    // Normal Board event tailing can observe the committed record. No external effects or
    // webhook requests are dispatched by this isolated slice.
    Ok(Json(value))
}

async fn read(
    State(s): State<Service>,
    identity: Option<Extension<ForcedViewer>>,
    Path(id): Path<i64>,
) -> Result<Json<Value>> {
    let caller = caller(identity)?;
    let row = sqlx::query("SELECT * FROM lifecycle_operations WHERE id=?")
        .bind(id)
        .fetch_optional(&s.pool)
        .await?
        .ok_or(Error(StatusCode::NOT_FOUND, "no operation"))?;
    let intent: Intent = serde_json::from_str(&row.try_get::<String, _>("intent")?)
        .map_err(|_| Error(StatusCode::INTERNAL_SERVER_ERROR, "invalid stored intent"))?;
    if !(row.try_get::<String, _>("requester")? == caller && s.policy.permits(&caller, &intent))
        && !s.policy.executor(&caller, &intent)
    {
        return Err(forbidden());
    }
    Ok(Json(view(&row)?))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Claim {
    claim_key: String,
    expected_revision: i64,
    expected: Binding,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Evidence {
    Draining {
        capable: bool,
        local_lock: bool,
        launch_excluded: bool,
        admission_ack: bool,
    },
    StopIntent {
        quiet: bool,
        queue_empty: bool,
        children_empty: bool,
        writers_settled: bool,
        journal_durable: bool,
        no_user_stop: bool,
    },
    Stopped {
        exit_observed: bool,
        children_empty: bool,
        writers_settled: bool,
        lock_released: bool,
    },
    LaunchIntent {
        absence_verified: bool,
        local_lock: bool,
        launch_excluded: bool,
        journal_durable: bool,
        artifact_verified: bool,
    },
    Verifying {
        new_birth_verified: bool,
        artifact_verified: bool,
        thread_verified: bool,
        history_preserved: bool,
        new_binding: Binding,
    },
    Completed {
        ordinary_attempt_completed: bool,
        admission_verified: bool,
        history_preserved: bool,
        user_stop_preserved: bool,
        new_binding: Binding,
    },
    HeldUnknown {
        reason: String,
    },
}
impl Evidence {
    fn state(&self) -> &'static str {
        match self {
            Self::Draining { .. } => "draining",
            Self::StopIntent { .. } => "stop_intent",
            Self::Stopped { .. } => "stopped",
            Self::LaunchIntent { .. } => "launch_intent",
            Self::Verifying { .. } => "verifying",
            Self::Completed { .. } => "completed",
            Self::HeldUnknown { .. } => "held_unknown",
        }
    }
    fn valid(&self) -> bool {
        match self {
            Self::Draining {
                capable,
                local_lock,
                launch_excluded,
                admission_ack,
            } => *capable && *local_lock && *launch_excluded && *admission_ack,
            Self::StopIntent {
                quiet,
                queue_empty,
                children_empty,
                writers_settled,
                journal_durable,
                no_user_stop,
            } => {
                *quiet
                    && *queue_empty
                    && *children_empty
                    && *writers_settled
                    && *journal_durable
                    && *no_user_stop
            }
            Self::Stopped {
                exit_observed,
                children_empty,
                writers_settled,
                lock_released,
            } => *exit_observed && *children_empty && *writers_settled && *lock_released,
            Self::LaunchIntent {
                absence_verified,
                local_lock,
                launch_excluded,
                journal_durable,
                artifact_verified,
            } => {
                *absence_verified
                    && *local_lock
                    && *launch_excluded
                    && *journal_durable
                    && *artifact_verified
            }
            Self::Verifying {
                new_birth_verified,
                artifact_verified,
                thread_verified,
                history_preserved,
                ..
            } => {
                *new_birth_verified && *artifact_verified && *thread_verified && *history_preserved
            }
            Self::Completed {
                ordinary_attempt_completed,
                admission_verified,
                history_preserved,
                user_stop_preserved,
                ..
            } => {
                *ordinary_attempt_completed
                    && *admission_verified
                    && *history_preserved
                    && *user_stop_preserved
            }
            Self::HeldUnknown { reason } => text_ok(reason),
        }
    }
    fn replacement(&self) -> Option<&Binding> {
        match self {
            Self::Verifying { new_binding, .. } | Self::Completed { new_binding, .. } => {
                Some(new_binding)
            }
            _ => None,
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    receipt_key: String,
    effect_id: String,
    expected_revision: i64,
    claim_token: String,
    expected: Binding,
    target_sha256: String,
    observed_at_ms: i64,
    clock_certain: bool,
    evidence_sha256: String,
    evidence: Evidence,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Cancel {
    expected_revision: i64,
}
fn stored_intent(row: &sqlx::sqlite::SqliteRow) -> Result<Intent> {
    serde_json::from_str(&row.try_get::<String, _>("intent")?)
        .map_err(|_| Error(StatusCode::INTERNAL_SERVER_ERROR, "invalid stored intent"))
}
async fn operation(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: i64,
) -> Result<sqlx::sqlite::SqliteRow> {
    sqlx::query("SELECT * FROM lifecycle_operations WHERE id=?")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(Error(StatusCode::NOT_FOUND, "no operation"))
}
async fn event(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    actor: &str,
    value: &Value,
) -> Result<()> {
    let mut hooks = Vec::new();
    crate::events::emit(tx,&mut hooks,"lifecycle.transition",Some(actor),None,None,None,None,
        json!({"operation_id":value["operation_id"],"revision":value["revision"],"state":value["state"]}),
        crate::events::Recipients::Explicit(std::collections::BTreeSet::from([actor.to_string(),value["requester"].as_str().unwrap_or_default().to_string()])))
        .await.map_err(|_|Error(StatusCode::INTERNAL_SERVER_ERROR,"event persistence failed"))?;
    Ok(())
}
async fn claim(
    State(s): State<Service>,
    identity: Option<Extension<ForcedViewer>>,
    Path(id): Path<i64>,
    Json(body): Json<Claim>,
) -> Result<Json<Value>> {
    let caller = caller(identity)?;
    body.expected.validate()?;
    if !text_ok(&body.claim_key) || body.expected_revision < 0 {
        return Err(bad("invalid claim"));
    }
    let encoded = serde_json::to_string(&body).map_err(|_| bad("invalid claim"))?;
    let mut tx = s.pool.begin().await?;
    let row = operation(&mut tx, id).await?;
    let intent = stored_intent(&row)?;
    if !s.policy.executor(&caller, &intent) {
        return Err(forbidden());
    }
    if row.try_get::<Option<String>, _>("executor")?.as_deref() == Some(&caller)
        && row.try_get::<Option<String>, _>("claim_key")?.as_deref() == Some(&body.claim_key)
    {
        if row
            .try_get::<Option<String>, _>("claim_payload")?
            .as_deref()
            != Some(&encoded)
        {
            return Err(conflict("claim key changed"));
        }
        let result: Value = serde_json::from_str(&row.try_get::<String, _>("claim_result")?)
            .map_err(|_| Error(StatusCode::INTERNAL_SERVER_ERROR, "invalid stored claim"))?;
        tx.commit().await?;
        return Ok(Json(result));
    }
    if row.try_get::<String, _>("state")? != "requested"
        || row.try_get::<i64, _>("revision")? != body.expected_revision
    {
        return Err(conflict("claimed or stale revision"));
    }
    let now = chrono::Utc::now().timestamp_millis();
    if now < row.try_get::<i64, _>("created_at_ms")?
        || now >= intent.forward_deadline_ms
        || body.expected != intent.expected
    {
        return Err(conflict("stale identity or deadline"));
    }
    let changed=sqlx::query("UPDATE lifecycle_operations SET state='claimed',revision=revision+1,executor=?,claim_token=lower(hex(randomblob(32))),claim_key=?,claim_payload=?,updated_at_ms=? WHERE id=? AND revision=? AND state='requested'")
        .bind(&caller).bind(&body.claim_key).bind(&encoded).bind(now).bind(id).bind(body.expected_revision).execute(&mut *tx).await?;
    if changed.rows_affected() != 1 {
        return Err(conflict("claim raced"));
    }
    let value = view(&operation(&mut tx, id).await?)?;
    sqlx::query("UPDATE lifecycle_operations SET claim_result=? WHERE id=?")
        .bind(value.to_string())
        .bind(id)
        .execute(&mut *tx)
        .await?;
    event(&mut tx, &caller, &value).await?;
    tx.commit().await?;
    Ok(Json(value))
}
async fn receipt(
    State(s): State<Service>,
    identity: Option<Extension<ForcedViewer>>,
    Path(id): Path<i64>,
    Json(body): Json<Receipt>,
) -> Result<Json<Value>> {
    let caller = caller(identity)?;
    body.expected.validate()?;
    if !text_ok(&body.receipt_key)
        || !text_ok(&body.effect_id)
        || !hash_ok(&body.claim_token)
        || !hash_ok(&body.target_sha256)
        || !hash_ok(&body.evidence_sha256)
        || body.expected_revision < 0
        || body.observed_at_ms < 0
        || !body.evidence.valid()
    {
        return Err(bad("invalid receipt"));
    }
    let encoded = serde_json::to_string(&body).map_err(|_| bad("invalid receipt"))?;
    let mut tx = s.pool.begin().await?;
    let row = operation(&mut tx, id).await?;
    let intent = stored_intent(&row)?;
    if !s.policy.executor(&caller, &intent)
        || row.try_get::<Option<String>, _>("executor")?.as_deref() != Some(&caller)
    {
        return Err(forbidden());
    }
    if row.try_get::<Option<String>, _>("claim_token")?.as_deref() != Some(&body.claim_token) {
        return Err(conflict("stale claim token"));
    }
    if let Some(prior) = sqlx::query(
        "SELECT payload,result FROM lifecycle_receipts WHERE operation_id=? AND receipt_key=?",
    )
    .bind(id)
    .bind(&body.receipt_key)
    .fetch_optional(&mut *tx)
    .await?
    {
        if prior.try_get::<String, _>("payload")? != encoded {
            return Err(conflict("receipt key changed"));
        }
        let value = serde_json::from_str(&prior.try_get::<String, _>("result")?)
            .map_err(|_| Error(StatusCode::INTERNAL_SERVER_ERROR, "invalid stored receipt"))?;
        tx.commit().await?;
        return Ok(Json(value));
    }
    let state = row.try_get::<String, _>("state")?;
    let next = body.evidence.state();
    if row.try_get::<i64, _>("revision")? != body.expected_revision
        || row.try_get::<i64, _>("active")? != 1
        || state == "held_unknown"
        || state == "requested"
    {
        return Err(conflict("stale or held operation"));
    }
    if body.expected != intent.expected || body.target_sha256 != intent.target_sha256 {
        return Err(conflict("receipt identity mismatch"));
    }
    let now = chrono::Utc::now().timestamp_millis();
    let last = row
        .try_get::<Option<i64>, _>("updated_at_ms")?
        .unwrap_or(row.try_get("created_at_ms")?);
    if body.observed_at_ms < row.try_get::<i64, _>("created_at_ms")?
        || body.observed_at_ms > now
        || body.observed_at_ms
            < row
                .try_get::<Option<i64>, _>("last_observed_ms")?
                .unwrap_or(0)
    {
        return Err(bad("invalid observation time"));
    }
    if next != "held_unknown" {
        if !body.clock_certain || now < last || now >= intent.forward_deadline_ms {
            return Err(conflict("deadline or uncertain clock"));
        }
        let allowed = matches!(
            (state.as_str(), next),
            ("claimed", "draining")
                | ("draining", "stop_intent")
                | ("stop_intent", "stopped")
                | ("stopped", "launch_intent")
                | ("launch_intent", "verifying")
                | ("verifying", "completed")
        );
        if !allowed {
            return Err(conflict("invalid transition"));
        }
    }
    if sqlx::query("SELECT 1 FROM lifecycle_receipts WHERE operation_id=? AND effect_id=?")
        .bind(id)
        .bind(&body.effect_id)
        .fetch_optional(&mut *tx)
        .await?
        .is_some()
    {
        return Err(conflict("effect id already used"));
    }
    let mut replacement = None;
    if let Some(new) = body.evidence.replacement() {
        new.validate()?;
        if new.host_id != intent.expected.host_id
            || new.boot_id != intent.expected.boot_id
            || new.thread_id != intent.expected.thread_id
            || new.start_ticks == intent.expected.start_ticks
            || new.exe_sha256 != intent.target_sha256
        {
            return Err(conflict("replacement identity mismatch"));
        }
        let serialized = serde_json::to_string(new).map_err(|_| bad("invalid replacement"))?;
        if next == "completed"
            && row.try_get::<Option<String>, _>("new_binding")?.as_deref() != Some(&serialized)
        {
            return Err(conflict("replacement changed after verification"));
        }
        replacement = Some(serialized);
    }
    let changed=sqlx::query("UPDATE lifecycle_operations SET state=?,revision=revision+1,active=?,last_observed_ms=?,updated_at_ms=?,new_binding=COALESCE(?,new_binding) WHERE id=? AND revision=? AND claim_token=?")
        .bind(next).bind(i64::from(next!="completed")).bind(body.observed_at_ms).bind(now).bind(replacement).bind(id).bind(body.expected_revision).bind(&body.claim_token).execute(&mut *tx).await?;
    if changed.rows_affected() != 1 {
        return Err(conflict("receipt raced"));
    }
    let value = view(&operation(&mut tx, id).await?)?;
    sqlx::query("INSERT INTO lifecycle_receipts(operation_id,receipt_key,effect_id,payload,result) VALUES(?,?,?,?,?)").bind(id).bind(&body.receipt_key).bind(&body.effect_id).bind(encoded).bind(value.to_string()).execute(&mut *tx).await?;
    event(&mut tx, &caller, &value).await?;
    tx.commit().await?;
    Ok(Json(value))
}
async fn cancel(
    State(s): State<Service>,
    identity: Option<Extension<ForcedViewer>>,
    Path(id): Path<i64>,
    Json(body): Json<Cancel>,
) -> Result<Json<Value>> {
    let caller = caller(identity)?;
    if body.expected_revision < 0 {
        return Err(bad("invalid revision"));
    }
    let mut tx = s.pool.begin().await?;
    let row = operation(&mut tx, id).await?;
    let intent = stored_intent(&row)?;
    if row.try_get::<String, _>("requester")? != caller || !s.policy.permits(&caller, &intent) {
        return Err(forbidden());
    }
    let state = row.try_get::<String, _>("state")?;
    let revision = row.try_get::<i64, _>("revision")?;
    if state == "cancelled" && body.expected_revision.checked_add(1) == Some(revision) {
        let value = view(&row)?;
        tx.commit().await?;
        return Ok(Json(value));
    }
    if state != "requested" || revision != body.expected_revision {
        return Err(conflict("only an unclaimed operation may cancel"));
    }
    let changed=sqlx::query("UPDATE lifecycle_operations SET state='cancelled',active=0,revision=revision+1,updated_at_ms=? WHERE id=? AND revision=? AND state='requested'")
        .bind(chrono::Utc::now().timestamp_millis()).bind(id).bind(revision).execute(&mut *tx).await?;
    if changed.rows_affected() != 1 {
        return Err(conflict("cancel raced"));
    }
    let value = view(&operation(&mut tx, id).await?)?;
    event(&mut tx, &caller, &value).await?;
    tx.commit().await?;
    Ok(Json(value))
}

#[cfg(test)]
mod tests;

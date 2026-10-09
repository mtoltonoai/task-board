# Isolated lifecycle API implementation

This work belongs to task_145 under task_142. It is additive source and temporary-database test code; production startup neither installs its tables nor mounts its routes.

## Implemented first slice

`api::lifecycle_router(AppState, lifecycle::Policy)` builds a router through the existing configured trusted-front-door identity middleware. `lifecycle::install(&Pool)` explicitly installs the isolated tables. Neither is called by production startup. The normal task queue, lifecycle intent, runtime controls and credentials are unchanged.

`POST /lifecycle/operations` accepts a closed `{request_key, intent}` object. Intent contains `subject`, `expected` (`host_id`, `boot_id`, `pid`, `start_ticks`, `exe_sha256`, `thread_id`), `target_sha256`, `effect` (`restart` or `upgrade`), integer Unix-millisecond `forward_deadline_ms`, `recovery_deadline_ms`, and `recovery_effects` (empty only). Restart must retain the executable digest. Identity strings are bounded and process identifiers positive. Unknown fields, invalid digests and recovery effects refuse.

The server-owned policy grants an exact requester, subject, target digest and effect. The acting caller comes only from `ForcedViewer`; client attribution, registration, a permissive host or a missing trusted header cannot substitute. Deployment must still establish actual trusted front-door provenance and prevent direct header spoofing before enabling this surface. Test headers exercise middleware binding, not an authentication deployment proof. This is not a dependency on Codex per-action approval.

The returned object is `{operation_id, requester, intent, state, revision, active, created_at_ms}`. Creation returns requested/revision0/active true. `GET /lifecycle/operations/{id}` returns the same durable status and rechecks caller ownership and policy. Missing operation is404, untrusted/out-of-scope caller403, invalid semantic input400, invalid JSON shape422 and idempotency/subject/deadline conflict409.

An identical caller/subject/key and typed intent returns the original operation without a second event, including after the forward deadline. Changed intent under the same key conflicts. A unique active-subject index and transactional create exclude a second operation. The request and the normal Board event/inbox entry commit together. This isolated slice intentionally dispatches no external webhooks or host effects.

Tests use actual Axum routes and existing identity middleware with temporary SQLite databases. They cover denied caller/scope/forged attribution/invalid identity, same-key concurrency, different-key subject races and persistence after database reopen. They do not demonstrate a live trusted transport or process observation.

## Claim, receipts and cancellation

Server policy has separate `grants` and `executors` lists. Requester grants remain unchanged; executor entries now use mandatory-host `ExecutorGrant {requester, host_id, subject, target_sha256, effect}`. There is no missing-host wildcard. The same executor predicate applies to discovery, get, claim and receipts. A requester grant never permits discovery, claim or receipt submission. Get permits the original requester under its current grant, or an executor with the exact current operation/host grant. Responses add nullable `executor` and `claim_token`; the token is a compare-and-set fence, not a substitute for caller authorization.

`POST /lifecycle/operations/{id}/claim` accepts `{claim_key, expected_revision, expected}`. The expected binding must equal immutable intent. Only requested operations before their forward deadline can be claimed. The transaction assigns one executor and token, advances revision, and emits one transition event. Same executor/key/exact claim payload returns the original claim result, even after subsequent progress. A different key never takes over an existing claim. Use get for current status after a retried claim.

`POST /lifecycle/operations/{id}/receipts` accepts `{receipt_key,effect_id,expected_revision,claim_token,expected,target_sha256,observed_at_ms,clock_certain,evidence_sha256,evidence}`. All objects are closed; identity, digests, integer nonnegative timestamps and required booleans are validated. Evidence is a tagged union using `kind`. Exact definitions are in `Evidence` in src/lifecycle.rs. Stages are draining, stop_intent, stopped, launch_intent, verifying, completed, and held_unknown. Each forward stage requires its predecessor and all typed positive facts. Verification/completion bind a new process birth on the same host/boot/thread with exact target executable; completion must retain the stored verified replacement. Held-unknown requires a bounded reason and retains exclusion; no forward or recovery edge leaves that state in this increment.

Server time enforces deadlines, client observation times must lie between creation and server now and never regress from the prior receipt. Explicit clock certainty is required for forward edges; server-clock regression refuses them. These checks do not independently prove a host clock or observation is authentic. The authorized executor still owns observation truth and local process fencing.

Receipt replay with the same key and exact payload returns the original committed transition result without another event. Different content under that key conflicts. Effect ids cannot be reused for another transition. Receipt, operation revision/state and event commit atomically. After a lost response, read/replay the same request; never infer that a process effect failed.

`POST /lifecycle/operations/{id}/cancel` accepts `{expected_revision}` from the original authorized requester. Only requested/unclaimed state cancels and releases exclusion. Its identical retry returns the cancelled result. Claim/cancel races admit one winner. Claimed cancellation is refused, including after expiry. Recovery dispatch remains unsupported; recovery_effects must be empty.

The module remains isolated and production-unmounted. Integration and independent review remain required before task acceptance. No live caller/transport, executable, host-drain or process-effect validation is claimed by API fixtures.

Database crash durability inherits existing Board write-ahead logging with synchronous normal. Power-loss durability is not claimed by these tests. No existing database is opened or migrated for validation.

## Bounded loopback integration server

The ignored test `lifecycle::tests::tcp_fixture_server` builds the real router and middleware against its own temporary database. Run the compiled test binary with `--exact lifecycle::tests::tcp_fixture_server --ignored --nocapture` and set `LIFECYCLE_FIXTURE_CONFIG` to a runner-owned JSON file containing `{intent, ready_path, lifetime_ms, identity_header}`. Intent is the exact shape above; identity_header is `x-fixture-user` or `x-fleet-agent`. Lifetime must be between100 and300000 milliseconds. The ready path must not exist; the fixture creates it once, containing base_url, host, identity_header, requester, executor and intent. Request Host must be `fixture.test`; requester identity is `alice`, executor identity is `executor`. The configured header carries one of those values. No additional `/api` prefix is mounted.

The server binds only `127.0.0.1` on a kernel-selected port, stops at its fixed lifetime and drops its temporary database. The invoking runner owns process waiting/cleanup and any retained fixture files. This is a deliberately synthetic front-door configuration for component tests, not a production credential or authenticated deployment proof. There is no background recurrence or live service dependency.

## Executor-only pending discovery

`GET /lifecycle/operations?host_id=fixture-host&after_id=0&limit=20` is an executor-only read. Host is required and is only a narrowing selector within an exact server-owned executor grant. Identity still comes from ForcedViewer, never query/body attribution. Unknown query fields refuse. after_id defaults0 and must be nonnegative; limit defaults20 and must be1..100. An optional through_id must be at least after_id.

Response is `{operations:[existing operation projection], through_id, next_after_id, has_more}`. Only active, requested, unexpired operations matching caller/host/subject/target/effect are eligible. Authorization is applied in SQL before limit. On a first page, the transaction computes the largest eligible authorized id and returns it as through_id (at least the supplied after_id). No other executor's rows contribute that ceiling. Send the returned through_id and next_after_id for another page. A limit+1 query determines has_more; next_after_id is the last returned operation id only when another eligible page exists, otherwise null. No total count is returned.

The ceiling excludes later insertions from that traversal. Claim, cancellation and expiry can remove rows between pages; this is bounded current-state discovery, not a stable historical snapshot, complete event replay or proof of uninterrupted readiness. The ordinary reconciliation owner consumes one bounded page per invocation initially and starts subsequent invocations from a new first page. No new loop, timer or model-visible list tool is added.

Listing emits no events, claims nothing and releases no exclusion. Expired requested operations are omitted but remain durable and active, visible through authorized get and eligible for existing pre-claim cancellation. Existing claim checks remain authoritative after any list/state race. Synthetic fixture setup binds the executor grant to intent.expected.host_id. Production startup, trusted-provenance limits and accepted previous9612102 fixture/source remain separate and unchanged by this isolated extension.

## Active-subject launch guard

`GET /lifecycle/subjects/{subject}/guard?host_id=<configured-host>` is a separate executor-only read. Exact trusted caller/host/subject grant is required before lookup; requester-only access is refused. The host query only establishes the caller's authorized subject scope. The lookup covers every active operation for the subject globally, regardless of state, deadline, operation host, target build or effect. In particular, expired requests and held_unknown operations block.

The successful response is exactly `{guard:"clear"}` only when no active operation exists. An active operation returns `{guard:"blocked"}`. Only if the caller's full existing executor grant also authorizes the actual active operation may the response additionally contain `operation:{operation_id,state}`; other-host/build/effect details are opaque. No token, caller, full intent or unauthorized artifact information is returned. Invalid input400, insufficient identity/grant403, storage or malformed stored state500, transport errors, unavailable responses and unknown/malformed variants must never be interpreted as clear. A missing route404 is not a clear result.

This read emits no event and changes no state. It is a snapshot, not a reservation. Runtime must hold the same configured canonical local subject lock across guard and ordinary launch, and across lifecycle operation effects. A Board request can still be created after a clear response: the lifecycle consumer must acquire that lock and freshly re-get/revalidate operation state and actual process birth before acting. This endpoint does not atomically fence remote request creation or noncooperating processes; a stronger distributed launch reservation would require a separate contract. Existing production mounting and trusted-provenance restrictions remain unchanged.

## Current replacement binding projection

The authorized operation projection now includes `new_binding`, either null before verification or the validated stored `Binding` object accepted by the verifying receipt. It remains present at completed, or held_unknown entered after verification. Current verifying/completed rows must have a binding. A stored replacement in an earlier state, malformed JSON/unknown fields, invalid identity/digest or mismatch against immutable expected host/boot/thread, new start_ticks and target executable returns500 rather than null or a guessed identity. No new read authority is granted.

Host release must freshly get current state and require active verifying plus exact operation/executor/token/revision and its actual new process identity matching this value. Presence of a binding in completed/held_unknown is not release permission. It remains an executor-reported observation, not independent process attestation by the Board.

Idempotent claim/receipt replays retain their original committed snapshots, not current state; old snapshots created before this field existed may omit it. Consumers must use fresh get for release admission and must not treat an old claim/verification response as a new permit. No historical response rewriting or live migration is included.

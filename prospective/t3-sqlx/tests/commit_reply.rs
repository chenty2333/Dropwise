//! Real SQLx/Postgres with a loopback TCP gate delaying server-to-client bytes.
//! The application retries an operation after abandoning its acknowledgement.
//! This is an application-policy experiment, not a SQLx/Postgres defect claim.

use std::error::Error;
use std::future::{Future, poll_fn};
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::task::Poll;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgSslMode};
use sqlx::{AssertSqlSafe, Connection, Executor, PgConnection, SqlSafeStr};
// No Dropwise cut here: cancellation is a manual `select!` while the reply gate
// holds the acknowledgement, so `explore_observed` does not apply. `Calibration`
// only judges that manually driven run against two completed runs.
#[path = "../../support/allowed.rs"]
mod allowed;
#[path = "support/reply_gate.rs"]
mod reply_gate;
use allowed::{Calibration, Expect, Verdict};
use reply_gate::ReplyGate;
#[derive(Debug, PartialEq, Eq)]
struct Observation {
    effects: i32,
    acknowledgement_delta: i32,
    successors: Vec<i32>,
}

type TestError = Box<dyn Error + Send + Sync>;
static NEXT_TABLE: AtomicUsize = AtomicUsize::new(0);
const LIMIT: Duration = Duration::from_secs(5);

async fn visible_effects(observer: &mut PgConnection, table: &str) -> Result<i32, sqlx::Error> {
    // table is generated internally from the process ID and an integer counter.
    sqlx::query_scalar(AssertSqlSafe(format!(
        "SELECT COALESCE((SELECT executions FROM {table} WHERE request_key = 'one-request'), 0)"
    )))
    .fetch_one(observer)
    .await
}

async fn committed_with_reply_held(
    observer: &mut PgConnection,
    table: &str,
    withheld: &AtomicUsize,
) -> Result<(), TestError> {
    tokio::time::timeout(LIMIT, async {
        loop {
            // A separate connection sees only committed state. The reply gate
            // independently confirms that actual response bytes were withheld.
            if visible_effects(observer, table).await? > 0 && withheld.load(SeqCst) > 0 {
                return Ok::<_, TestError>(());
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await?
}

async fn scenario(cancel: bool, deduplicate: bool) -> Result<Observation, TestError> {
    let direct: PgConnectOptions = std::env::var("DATABASE_URL")?.parse()?;
    if direct.get_host() != "127.0.0.1" {
        return Err("experiment only supports the disposable loopback PostgreSQL service".into());
    }
    let direct = direct.ssl_mode(PgSslMode::Disable);
    let mut observer = PgConnection::connect_with(&direct).await?;
    // Generated solely from numbers; distinct tests never share a table.
    let table = format!(
        "dw_reply_{}_{}",
        std::process::id(),
        NEXT_TABLE.fetch_add(1, SeqCst)
    );
    sqlx::query(AssertSqlSafe(format!(
        "CREATE TABLE {table} (request_key TEXT PRIMARY KEY, executions INT NOT NULL)"
    )))
    .execute(&mut observer)
    .await?;

    let mut gate = ReplyGate::start(direct.get_port()).await?;
    let result: Result<Observation, TestError> = async {
        let mut client = PgConnection::connect_with(&direct.clone().port(gate.port)).await?;
        let first_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut client)
            .await?;
        let update = if deduplicate {
            format!("{table}.executions")
        } else {
            format!("{table}.executions + 1")
        };
        // Both versions use the same logical request key. The unsafe application
        // treats every attempt as another effect; the control retains one effect.
        // RETURNING is attempt-specific to detect old/new response confusion.
        let operation = format!(
            "INSERT INTO {table} VALUES ('one-request', 1) \
             ON CONFLICT (request_key) DO UPDATE SET executions = {update} \
             RETURNING $1::int4"
        );
        // Complete Parse/Describe first. Otherwise a byte-level reply gate can
        // block statement preparation, before the server ever sees the UPDATE.
        client
            .prepare(AssertSqlSafe(operation.clone()).into_sql_str())
            .await?;
        gate.hold(cancel);
        let mut acknowledgement_delta = 0;
        {
            let first = sqlx::query_scalar::<_, i32>(AssertSqlSafe(operation.as_str()))
                .bind(111_i32)
                .fetch_one(&mut client);
            tokio::pin!(first);
            if cancel {
                tokio::select! {
                    biased;
                    response = &mut first => {
                        return Err(format!("first reply escaped the gate: {response:?}").into());
                    }
                    confirmed = committed_with_reply_held(&mut observer, &table, &gate.withheld) => {
                        confirmed?;
                    }
                }
                // Leaving this scope actually drops the original query future.
                // We do not cancel the server or pretend its commit disappeared.
            } else {
                let reply = tokio::time::timeout(LIMIT, first).await??;
                acknowledgement_delta = reply - 111;
            }
        }
        if cancel {
            let retry = sqlx::query_scalar::<_, i32>(AssertSqlSafe(operation.as_str()))
                .bind(222_i32)
                .fetch_one(&mut client);
            tokio::pin!(retry);
            // Begin the successor while the old reply is still withheld. One
            // poll, not an arbitrary sleep, establishes this ordering.
            if let Poll::Ready(reply) = poll_fn(|cx| Poll::Ready(retry.as_mut().poll(cx))).await {
                return Err(format!("retry completed before gate release: {reply:?}").into());
            }
            gate.hold(false);
            let reply = tokio::time::timeout(LIMIT, retry).await??;
            acknowledgement_delta = reply - 222;
        }
        // Ensure all queued protocol messages are consumed and the SAME backend
        // is reusable, rather than quietly obtaining a fresh connection.
        let next_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut client)
            .await?;
        if next_pid != first_pid {
            return Err("backend changed during retry".into());
        }
        let mut successors = Vec::new();
        for expected in 1000_i32..1004 {
            let actual: i32 = sqlx::query_scalar("SELECT $1::int4")
                .bind(expected)
                .fetch_one(&mut client)
                .await?;
            successors.push(actual);
        }
        let effects = visible_effects(&mut observer, &table).await?;
        client.close().await?;
        println!(
            "cancel={cancel} deduplicate={deduplicate} committed_effects={effects} held_bytes={}",
            gate.withheld.load(SeqCst)
        );
        Ok(Observation { effects, acknowledgement_delta, successors })
    }
    .await;

    // Stop this test's proxy even when the oracle or database operation failed.
    gate.stop().await;
    let cleanup = sqlx::query(AssertSqlSafe(format!("DROP TABLE {table}")))
        .execute(&mut observer)
        .await;
    let effects = result?;
    cleanup?;
    Ok(effects)
}

#[tokio::test]
async fn automatic_successor_comparison() -> Result<(), TestError> {
    for deduplicate in [false, true] {
        let first =
            tokio::time::timeout(Duration::from_secs(20), scenario(false, deduplicate)).await??;
        let second =
            tokio::time::timeout(Duration::from_secs(20), scenario(false, deduplicate)).await??;
        let actual =
            tokio::time::timeout(Duration::from_secs(20), scenario(true, deduplicate)).await??;
        // The application retries until done, so only completion is allowed:
        // "never happened" is not an acceptable end state here.
        let verdict = Calibration::new(Vec::new(), vec![first, second])
            .check(Expect::Completed, &actual);
        println!(
            "deduplicate={deduplicate}: actual={actual:?}, verdict={verdict:?}"
        );
        if deduplicate {
            assert_eq!(verdict, Ok(Verdict::Post));
        } else {
            assert!(verdict.is_err());
        }
    }
    Ok(())
}

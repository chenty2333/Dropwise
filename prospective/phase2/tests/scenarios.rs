use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_nats::jetstream::{self, consumer, AckKind};
use dropwise::Ctx;
use dropwise_phase2::{
    add_metric, execute, observe_pending, run_target, Invocation, Metrics, Mode, SharedCall,
    TargetEnd,
};
use futures_util::StreamExt;
use moka::future::Cache;
use tokio::sync::{mpsc, oneshot};

const VALUE: &str = "same-key-value";
const SCENARIO_TIMEOUT: Duration = Duration::from_secs(10);
static NATS_RESOURCE_COUNTER: AtomicU64 = AtomicU64::new(0);

struct ActiveLoader {
    active: Arc<AtomicUsize>,
}

impl ActiveLoader {
    fn enter(active: Arc<AtomicUsize>, max_active: Arc<AtomicUsize>) -> Self {
        let now = active.fetch_add(1, Ordering::SeqCst) + 1;
        max_active.fetch_max(now, Ordering::SeqCst);
        Self { active }
    }
}

impl Drop for ActiveLoader {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}

fn note(
    metrics: &Metrics,
    run: &dropwise_phase2::TargetRun<impl Sized>,
    detail: impl Into<String>,
) {
    add_metric(
        metrics,
        Invocation {
            pending_count: run.pending_count,
            cancelled: run.end == TargetEnd::Cancelled,
            detail: detail.into(),
        },
    );
}

fn require_cancelled(
    mode: Mode,
    run: &dropwise_phase2::TargetRun<impl Sized>,
) -> Result<(), String> {
    if mode == Mode::Baseline && run.end != TargetEnd::Completed {
        return Err("uncancelled baseline did not complete the marked target".into());
    }
    if mode == Mode::Simple && run.end != TargetEnd::Cancelled {
        return Err(format!(
            "{} did not actually cancel the marked target",
            dropwise_phase2::mode_name(mode)
        ));
    }
    Ok(())
}

async fn scenario_moka_leader_cancel(
    mode: Mode,
    ctx: Option<Ctx>,
    metrics: Metrics,
) -> Result<(), String> {
    let cache: Cache<String, String> = Cache::new(8);
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let loader_calls = Arc::new(AtomicUsize::new(0));

    let (release_tx, release_rx) = oneshot::channel::<()>();
    let (pending_tx, mut pending_rx) = mpsc::unbounded_channel::<usize>();
    let gate_driver = tokio::spawn(async move {
        if pending_rx.recv().await.is_some() {
            let _ = release_tx.send(());
        }
    });

    let (loader_started_tx, mut loader_started_rx) = oneshot::channel::<()>();
    let leader_cache = cache.clone();
    let leader_active = active.clone();
    let leader_peak = max_active.clone();
    let leader_calls = loader_calls.clone();
    let leader = async move {
        leader_cache
            .get_with("product:42".to_owned(), async move {
                leader_calls.fetch_add(1, Ordering::SeqCst);
                let _active = ActiveLoader::enter(leader_active, leader_peak);
                let _ = loader_started_tx.send(());
                let _ = release_rx.await;
                VALUE.to_owned()
            })
            .await
    };

    // These two futures remain owned by the caller if the leader loses its
    // select branch; the branches only borrow them while driving the race.
    let waiter1_cache = cache.clone();
    let waiter2_cache = cache.clone();
    let waiter1_calls = loader_calls.clone();
    let waiter2_calls = loader_calls.clone();
    let waiter1_active = active.clone();
    let waiter2_active = active.clone();
    let waiter1_peak = max_active.clone();
    let waiter2_peak = max_active.clone();
    let mut waiter1 = SharedCall::new(async move {
        waiter1_cache
            .get_with("product:42".to_owned(), async move {
                waiter1_calls.fetch_add(1, Ordering::SeqCst);
                let _active = ActiveLoader::enter(waiter1_active, waiter1_peak);
                VALUE.to_owned()
            })
            .await
    });
    let mut waiter2 = SharedCall::new(async move {
        waiter2_cache
            .get_with("product:42".to_owned(), async move {
                waiter2_calls.fetch_add(1, Ordering::SeqCst);
                let _active = ActiveLoader::enter(waiter2_active, waiter2_peak);
                VALUE.to_owned()
            })
            .await
    });
    let drive_waiters = async {
        let _ = tokio::join!(&mut waiter1, &mut waiter2);
    };

    let run = run_target(mode, ctx, leader, drive_waiters, vec![pending_tx]).await;
    let loader_started = loader_started_rx.try_recv().is_ok();
    note(
        &metrics,
        &run,
        format!("leader_loader_started_before_end={loader_started}"),
    );
    require_cancelled(mode, &run)?;

    // After a leader cancellation, remaining callers wait or promote a loader;
    // a fresh lookup is the same caller-side recovery path in every run.
    let leader_value = match run.output {
        Some(value) => value,
        None => {
            cache
                .get_with("product:42".to_owned(), async { VALUE.to_owned() })
                .await
        }
    };
    let (waiter1_value, waiter2_value) = tokio::join!(&mut waiter1, &mut waiter2);
    let cached = cache.get("product:42").await;
    let _ = gate_driver.await;

    if !loader_started {
        return Err("marked leader ended before its initializer was polled; this run did not exercise an in-flight leader".into());
    }
    if leader_value != VALUE
        || waiter1_value != VALUE
        || waiter2_value != VALUE
        || cached.as_deref() != Some(VALUE)
    {
        return Err(format!(
            "same-key calls disagreed: leader={leader_value:?}, waiter1={waiter1_value:?}, waiter2={waiter2_value:?}, cache={cached:?}"
        ));
    }
    if max_active.load(Ordering::SeqCst) > 1 {
        return Err(format!(
            "more than one same-key initializer active: peak={}",
            max_active.load(Ordering::SeqCst)
        ));
    }
    if active.load(Ordering::SeqCst) != 0 {
        return Err("initializer remained active after all callers completed".into());
    }
    if loader_calls.load(Ordering::SeqCst) == 0 {
        return Err("no initializer ran".into());
    }
    add_metric(
        &metrics,
        Invocation {
            pending_count: run.pending_count,
            cancelled: run.end == TargetEnd::Cancelled,
            detail: format!(
                "final leader={leader_value:?}; waiters=[{waiter1_value:?},{waiter2_value:?}]; cache={cached:?}; loader_calls={}; peak_active={}",
                loader_calls.load(Ordering::SeqCst),
                max_active.load(Ordering::SeqCst)
            ),
        },
    );
    Ok(())
}

async fn scenario_moka_waiter_cancel(
    mode: Mode,
    ctx: Option<Ctx>,
    metrics: Metrics,
) -> Result<(), String> {
    let cache: Cache<String, String> = Cache::new(8);
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let loader_calls = Arc::new(AtomicUsize::new(0));
    let leader_cache = cache.clone();
    let leader_active = active.clone();
    let leader_peak = max_active.clone();
    let leader_calls = loader_calls.clone();

    let (gate_tx, gate_rx) = oneshot::channel::<()>();
    let (leader_started_tx, leader_started_rx) = oneshot::channel::<()>();
    let (leader_pending_tx, mut leader_pending_rx) = mpsc::unbounded_channel::<usize>();
    let leader = tokio::spawn(async move {
        let call = leader_cache.get_with("config:main".to_owned(), async move {
            leader_calls.fetch_add(1, Ordering::SeqCst);
            let _active = ActiveLoader::enter(leader_active, leader_peak);
            let _ = leader_started_tx.send(());
            let _ = gate_rx.await;
            VALUE.to_owned()
        });
        // The leader is deliberately outside the selected request: cancellation
        // of a follower must leave this shared initializer alive.
        let target = observe_pending(call, leader_pending_tx);
        target.await
    });
    tokio::time::timeout(SCENARIO_TIMEOUT, leader_started_rx)
        .await
        .map_err(|_| "leader initializer did not start".to_string())?
        .map_err(|_| "leader task ended before starting its initializer".to_string())?;
    tokio::time::timeout(SCENARIO_TIMEOUT, leader_pending_rx.recv())
        .await
        .map_err(|_| "leader initializer did not reach Pending".to_string())?
        .ok_or_else(|| "leader Pending observer closed".to_string())?;

    let target_cache = cache.clone();
    let target_calls = loader_calls.clone();
    let target_active = active.clone();
    let target_peak = max_active.clone();
    let target = async move {
        target_cache
            .get_with("config:main".to_owned(), async move {
                target_calls.fetch_add(1, Ordering::SeqCst);
                let _active = ActiveLoader::enter(target_active, target_peak);
                VALUE.to_owned()
            })
            .await
    };

    let remaining_cache = cache.clone();
    let remaining_calls = loader_calls.clone();
    let remaining_active = active.clone();
    let remaining_peak = max_active.clone();
    let mut remaining = SharedCall::new(async move {
        remaining_cache
            .get_with("config:main".to_owned(), async move {
                remaining_calls.fetch_add(1, Ordering::SeqCst);
                let _active = ActiveLoader::enter(remaining_active, remaining_peak);
                VALUE.to_owned()
            })
            .await
    });

    let (release_leader_tx, mut release_leader_rx) = mpsc::unbounded_channel::<usize>();
    let gate_driver = tokio::spawn(async move {
        if release_leader_rx.recv().await.is_some() {
            let _ = gate_tx.send(());
        }
    });

    let target_pending = mpsc::unbounded_channel::<usize>();
    let (target_pending_tx, mut target_pending_rx) = target_pending;
    let release_gate = release_leader_tx.clone();
    let target_gate_driver = tokio::spawn(async move {
        if target_pending_rx.recv().await.is_some() {
            let _ = release_gate.send(1);
        }
    });

    let drive_remaining = async {
        let _ = (&mut remaining).await;
    };
    let run = run_target(mode, ctx, target, drive_remaining, vec![target_pending_tx]).await;
    note(
        &metrics,
        &run,
        "leader task remains independent; marked target is one coalesced waiter",
    );
    require_cancelled(mode, &run)?;

    // Canceling the selected waiter must not cancel the task that owns the
    // initializer or the other waiter. Releasing the same event gate lets both
    // surviving calls finish and then verifies the shared cache entry.
    let _ = target_gate_driver.await;
    let _ = gate_driver.await;
    let leader_value = tokio::time::timeout(SCENARIO_TIMEOUT, leader)
        .await
        .map_err(|_| "shared leader did not finish".to_string())?
        .map_err(|e| format!("shared leader task failed: {e}"))?;
    let remaining_value = tokio::time::timeout(SCENARIO_TIMEOUT, &mut remaining)
        .await
        .map_err(|_| "remaining waiter did not finish".to_string())?;
    let target_value = match run.output {
        Some(value) => value,
        None => String::new(),
    };
    let cached = cache.get("config:main").await;
    if run.end == TargetEnd::Cancelled {
        if !target_value.is_empty() {
            return Err("cancelled waiter unexpectedly returned a value".into());
        }
    } else if target_value != VALUE {
        return Err(format!("uncancelled waiter returned {target_value:?}"));
    }
    if leader_value != VALUE || remaining_value != VALUE || cached.as_deref() != Some(VALUE) {
        return Err(format!(
            "surviving callers disagreed: leader={leader_value:?}, waiter={remaining_value:?}, cache={cached:?}"
        ));
    }
    if loader_calls.load(Ordering::SeqCst) != 1 {
        return Err(format!(
            "expected one coalesced loader, got {}",
            loader_calls.load(Ordering::SeqCst)
        ));
    }
    if max_active.load(Ordering::SeqCst) > 1 || active.load(Ordering::SeqCst) != 0 {
        return Err(format!(
            "initializer activity inconsistent after recovery: peak={}, active={}",
            max_active.load(Ordering::SeqCst),
            active.load(Ordering::SeqCst)
        ));
    }
    add_metric(
        &metrics,
        Invocation {
            pending_count: run.pending_count,
            cancelled: run.end == TargetEnd::Cancelled,
            detail: format!(
                "final leader={leader_value:?}; surviving_waiter={remaining_value:?}; cache={cached:?}; loader_calls={}",
                loader_calls.load(Ordering::SeqCst)
            ),
        },
    );
    Ok(())
}

fn fresh_nats_names(scene: &str) -> (String, String, String) {
    let mode = std::env::var("PHASE2_MODE").unwrap_or_else(|_| "missing".into());
    let run = std::env::var("RUN").unwrap_or_else(|_| "0".into());
    let serial = NATS_RESOURCE_COUNTER.fetch_add(1, Ordering::SeqCst);
    let stem = format!(
        "P2_{}_{}_{}_{}_{}",
        scene.to_uppercase(),
        mode.to_uppercase(),
        run,
        std::process::id(),
        serial
    );
    let stream = stem
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect::<String>();
    let consumer_name = format!("C{serial:08}");
    let subject = format!(
        "dropwise.phase2.{}.{}.{}.{}.{}",
        scene.replace('_', "."),
        mode.replace('-', "_"),
        std::process::id(),
        run,
        serial
    );
    (stream, consumer_name, subject)
}

async fn setup_pull_consumer(
    scene: &str,
    max_ack_pending: i64,
) -> Result<(jetstream::Context, consumer::PullConsumer, String), String> {
    let url = std::env::var("NATS_URL")
        .map_err(|_| "NATS_URL is required; use prospective/phase2/run.sh".to_string())?;
    let (stream_name, consumer_name, subject) = fresh_nats_names(scene);
    let client = async_nats::connect(url)
        .await
        .map_err(|e| format!("connect NATS: {e}"))?;
    let js = jetstream::new(client);
    let stream = js
        .get_or_create_stream(jetstream::stream::Config {
            name: stream_name,
            subjects: vec![subject.clone()],
            max_messages: 32,
            ..Default::default()
        })
        .await
        .map_err(|e| format!("create JetStream stream: {e}"))?;
    let consumer = stream
        .get_or_create_consumer(
            &consumer_name,
            consumer::pull::Config {
                durable_name: Some(consumer_name.clone()),
                ack_policy: consumer::AckPolicy::Explicit,
                ack_wait: Duration::from_secs(2),
                max_deliver: 5,
                max_ack_pending,
                ..Default::default()
            },
        )
        .await
        .map_err(|e| format!("create durable pull consumer: {e}"))?;
    Ok((js, consumer, subject))
}

async fn publish_id(js: &jetstream::Context, subject: &str, id: &str) -> Result<(), String> {
    js.publish(subject.to_owned(), id.to_owned().into())
        .await
        .map_err(|e| format!("publish {id}: {e}"))?;
    Ok(())
}

async fn next_delivery(
    messages: &mut consumer::pull::Stream,
) -> Result<jetstream::Message, String> {
    messages
        .next()
        .await
        .ok_or_else(|| "pull message stream ended unexpectedly".to_string())?
        .map_err(|e| format!("pull delivery: {e}"))
}

fn payload_id(message: &jetstream::Message) -> Result<String, String> {
    String::from_utf8(message.payload.to_vec())
        .map_err(|e| format!("non-UTF8 message payload: {e}"))
}

async fn scenario_nats_stream_resume(
    mode: Mode,
    ctx: Option<Ctx>,
    metrics: Metrics,
) -> Result<(), String> {
    let (js, consumer, subject) = setup_pull_consumer("nats_stream_resume", 8).await?;
    publish_id(&js, &subject, "prefix").await?;
    let mut messages = consumer
        .messages()
        .await
        .map_err(|e| format!("start pull stream: {e}"))?;
    let prefix = next_delivery(&mut messages).await?;
    if payload_id(&prefix)? != "prefix" {
        return Err("prefix delivery had the wrong payload".into());
    }
    prefix
        .double_ack()
        .await
        .map_err(|e| format!("ack prefix: {e}"))?;

    // Publish the next pair only after the marked read is known to be pending.
    // This event-controlled input is identical in all three methods.
    let (publish_after_pending_tx, mut publish_after_pending_rx) =
        mpsc::unbounded_channel::<usize>();
    let (published_tx, published_rx) = oneshot::channel::<Result<(), String>>();
    let publisher_js = js.clone();
    let publisher_subject = subject.clone();
    let publisher = tokio::spawn(async move {
        if publish_after_pending_rx.recv().await.is_none() {
            let _ = published_tx.send(Err("marked next() never returned Pending".into()));
            return;
        }
        let result = async {
            publish_id(&publisher_js, &publisher_subject, "event-1").await?;
            publish_id(&publisher_js, &publisher_subject, "event-2").await?;
            Ok(())
        }
        .await;
        let _ = published_tx.send(result);
    });

    let target_stream = &mut messages;
    let target = async move { target_stream.next().await };
    let run = run_target(
        mode,
        ctx,
        target,
        std::future::pending::<()>(),
        vec![publish_after_pending_tx],
    )
    .await;
    note(
        &metrics,
        &run,
        "same pull stream retained across the selected next() future",
    );
    require_cancelled(mode, &run)?;

    let published = tokio::time::timeout(SCENARIO_TIMEOUT, published_rx)
        .await
        .map_err(|_| "producer did not observe the Pending event".to_string())?
        .map_err(|_| "producer event channel closed".to_string())?;
    published?;
    publisher
        .await
        .map_err(|e| format!("publisher task failed: {e}"))?;

    let sink = Arc::new(Mutex::new(HashSet::<String>::new()));
    let mut logical_order = Vec::new();
    if let Some(item) = run.output {
        let delivery = item
            .ok_or_else(|| "marked next() returned end-of-stream".to_string())?
            .map_err(|e| format!("marked next() error: {e}"))?;
        let id = payload_id(&delivery)?;
        if id != "event-1" && id != "event-2" {
            return Err(format!("marked read returned unrelated payload {id:?}"));
        }
        sink.lock().unwrap().insert(id.clone());
        logical_order.push(id);
        delivery
            .double_ack()
            .await
            .map_err(|e| format!("ack marked delivery: {e}"))?;
    }

    // Resume the original caller loop on the same pull stream. Duplicate
    // deliveries are tolerated as at-least-once behavior; the logical sink is
    // idempotent and the first-seen order must match the two published IDs.
    let mut physical_deliveries = 0;
    while sink.lock().unwrap().len() < 2 && physical_deliveries < 6 {
        let delivery = tokio::time::timeout(SCENARIO_TIMEOUT, next_delivery(&mut messages))
            .await
            .map_err(|_| "continuing the same pull stream timed out".to_string())??;
        let id = payload_id(&delivery)?;
        if id != "event-1" && id != "event-2" {
            return Err(format!("unexpected payload on resumed stream: {id:?}"));
        }
        let inserted = sink.lock().unwrap().insert(id.clone());
        if inserted {
            logical_order.push(id);
        }
        delivery
            .double_ack()
            .await
            .map_err(|e| format!("ack resumed delivery: {e}"))?;
        physical_deliveries += 1;
    }
    if logical_order != vec!["event-1".to_string(), "event-2".to_string()] {
        return Err(format!(
            "logical delivery order/loss after resume: {logical_order:?}"
        ));
    }
    let info = consumer
        .get_info()
        .await
        .map_err(|e| format!("read consumer state: {e}"))?;
    if info.num_ack_pending != 0 {
        return Err(format!(
            "consumer retained {} unacknowledged message(s)",
            info.num_ack_pending
        ));
    }
    add_metric(
        &metrics,
        Invocation {
            pending_count: run.pending_count,
            cancelled: run.end == TargetEnd::Cancelled,
            detail: format!(
                "prefix=acknowledged; logical_order={logical_order:?}; unique_ids={:?}; physical_followup_deliveries={physical_deliveries}; ack_pending={}",
                sink.lock().unwrap(),
                info.num_ack_pending
            ),
        },
    );
    Ok(())
}

fn commit_once(sink: &Arc<Mutex<Vec<String>>>, id: &str) -> bool {
    let mut sink = sink.lock().unwrap();
    if sink.iter().any(|existing| existing == id) {
        false
    } else {
        sink.push(id.to_owned());
        true
    }
}

async fn scenario_nats_process_cancel(
    mode: Mode,
    ctx: Option<Ctx>,
    metrics: Metrics,
) -> Result<(), String> {
    let (js, consumer, subject) = setup_pull_consumer("nats_process_cancel", 1).await?;
    publish_id(&js, &subject, "work-1").await?;
    publish_id(&js, &subject, "work-2").await?;
    let mut messages = consumer
        .messages()
        .await
        .map_err(|e| format!("start pull stream: {e}"))?;
    let delivery = next_delivery(&mut messages).await?;
    let id = payload_id(&delivery)?;
    if id != "work-1" {
        return Err(format!("expected first work item, got {id:?}"));
    }

    let sink = Arc::new(Mutex::new(Vec::<String>::new()));
    let (gate1_tx, gate1_rx) = oneshot::channel::<()>();
    let (gate2_tx, gate2_rx) = oneshot::channel::<()>();
    let (commit_tx, mut commit_rx) = mpsc::unbounded_channel::<()>();
    let (pending_tx, mut pending_rx) = mpsc::unbounded_channel::<usize>();
    let gate_driver = tokio::spawn(async move {
        if pending_rx.recv().await.is_some() {
            let _ = gate1_tx.send(());
        }
        if commit_rx.recv().await.is_some() {
            let _ = gate2_tx.send(());
        }
    });

    let target_sink = sink.clone();
    let target_id = id.clone();
    let target = async move {
        let _ = gate1_rx.await;
        commit_once(&target_sink, &target_id);
        let _ = commit_tx.send(());
        let _ = gate2_rx.await;
        Ok::<(), String>(())
    };
    let run = run_target(
        mode,
        ctx,
        target,
        std::future::pending::<()>(),
        vec![pending_tx],
    )
    .await;
    note(
        &metrics,
        &run,
        "gate 1 precedes the idempotent effect; gate 2 is after commit and before ACK",
    );
    require_cancelled(mode, &run)?;

    let mut physical_deliveries = 1usize;
    if let Some(result) = run.output {
        result?;
        delivery
            .double_ack()
            .await
            .map_err(|e| format!("confirm work-1 ack: {e}"))?;
    } else {
        // Recovery preserves the held Delivery and makes the responsibility
        // explicit. Redelivery is allowed; the local sink is idempotent.
        delivery
            .ack_with(AckKind::Nak(None))
            .await
            .map_err(|e| format!("negative-ack cancelled work-1: {e}"))?;
        let redelivery = tokio::time::timeout(SCENARIO_TIMEOUT, next_delivery(&mut messages))
            .await
            .map_err(|_| {
                "NAKed work-1 was not redelivered within the observation bound".to_string()
            })??;
        physical_deliveries += 1;
        if payload_id(&redelivery)? != "work-1" {
            return Err("recovery received a different work item instead of work-1".into());
        }
        commit_once(&sink, "work-1");
        redelivery
            .double_ack()
            .await
            .map_err(|e| format!("confirm redelivered work-1 ack: {e}"))?;
    }
    let _ = gate_driver.await;

    let second = tokio::time::timeout(SCENARIO_TIMEOUT, next_delivery(&mut messages))
        .await
        .map_err(|_| "consumer did not advance to work-2".to_string())??;
    physical_deliveries += 1;
    if payload_id(&second)? != "work-2" {
        return Err(format!(
            "consumer advanced to the wrong item: {:?}",
            payload_id(&second)?
        ));
    }
    commit_once(&sink, "work-2");
    second
        .double_ack()
        .await
        .map_err(|e| format!("confirm work-2 ack: {e}"))?;

    let mut committed = sink.lock().unwrap().clone();
    committed.sort();
    if committed != vec!["work-1".to_string(), "work-2".to_string()] {
        return Err(format!(
            "idempotent sink has wrong logical effects: {committed:?}"
        ));
    }
    let info = consumer
        .get_info()
        .await
        .map_err(|e| format!("read consumer state: {e}"))?;
    if info.num_ack_pending != 0 {
        return Err(format!(
            "consumer retained {} unacknowledged message(s)",
            info.num_ack_pending
        ));
    }
    if physical_deliveries < 2 {
        return Err("expected to consume the first message and then make forward progress".into());
    }
    add_metric(
        &metrics,
        Invocation {
            pending_count: run.pending_count,
            cancelled: run.end == TargetEnd::Cancelled,
            detail: format!(
                "logical_effects={committed:?}; physical_deliveries={physical_deliveries}; ack_pending={}",
                info.num_ack_pending
            ),
        },
    );
    Ok(())
}

#[test]
fn moka_leader_cancel() {
    execute("moka_leader_cancel", scenario_moka_leader_cancel);
}

#[test]
fn moka_waiter_cancel() {
    execute("moka_waiter_cancel", scenario_moka_waiter_cancel);
}

#[test]
fn nats_stream_resume() {
    execute("nats_stream_resume", scenario_nats_stream_resume);
}

#[test]
fn nats_process_cancel() {
    execute("nats_process_cancel", scenario_nats_process_cancel);
}

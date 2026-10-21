//! Round-3 skytable ladder: B1/B2/B3/M1 plus preflight and recovery contrast.
//!
//! Usage:
//!   sky-ladder preflight <pooled|direct>
//!   sky-ladder ladder <pooled|direct> <rep>
//!   sky-ladder recovery <pooled|direct>

use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

use dropwise::{Expect, explore, explore_observed};
use p4e_skytable::{
    Env, BOUND, SURVIVOR_V, TARGET_V, Variant, bounded, calibration, config, cut, observed_after_cut,
};

#[path = "../../../../../../../support/ladder.rs"]
mod ladder;

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn run_id() -> usize {
    NEXT.fetch_add(1, SeqCst)
}

fn summary(level: &str, variant: Variant, report: &dropwise::Report) {
    let id = format!("{}/{}", variant.name(), level);
    match level {
        "M1" => ladder::observed(level, &id, report),
        _ => ladder::simple(level, &id, report),
    }
    for trial in &report.trials {
        println!(
            "SKY-TRIAL variant={} level={level} plan={} sites={:?} outcomes={:?} \
             cancelled={} verdict={:?} settled={} realized={} invariant={:?}",
            variant.name(),
            trial.plan_label(),
            trial.sites,
            trial.outcomes,
            trial.cancelled,
            trial.verdict,
            trial.settled,
            trial.is_realized(),
            trial.invariant
        );
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("");
    let variant = if mode == "wait" {
        Variant::Pooled
    } else {
        Variant::from_arg(args.get(2).map(String::as_str).unwrap_or(""))
            .unwrap_or_else(|err| panic!("{err}"))
    };
    match mode {
        // The ladder calls `explore`/`explore_observed`, which block on their
        // own runtime, so this process must not already be inside one.
        "ladder" => {
            let rep = args.get(3).and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
            ladder_main(variant, rep);
        }
        "wait" => {
            if !runtime().block_on(wait_ready()) {
                std::process::exit(1);
            }
        }
        "preflight" => runtime().block_on(preflight(variant)),
        "recovery" => runtime().block_on(recovery(variant)),
        other => panic!("unknown mode {other:?}"),
    }
}

async fn wait_ready() -> bool {
    let server = match p4e_skytable::Server::from_env() {
        Ok(server) => server,
        Err(err) => {
            eprintln!("wait: {err}");
            return false;
        }
    };
    match server.config().connect_async().await {
        Ok(mut conn) => match conn.query(&p4e_skytable::sysctl_query()).await {
            Ok(skytable::response::Response::Empty) => true,
            Ok(other) => {
                eprintln!("wait: unexpected sysctl response {other:?}");
                false
            }
            Err(err) => {
                eprintln!("wait: sysctl {err:?}");
                false
            }
        },
        Err(err) => {
            eprintln!("wait: connect {err:?}");
            false
        }
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("tokio runtime")
}

async fn preflight(variant: Variant) {
    println!(
        "SKY-PREFLIGHT variant={} server={} proxy_reply_delay_ms={}",
        variant.name(),
        std::env::var("DW_SKY_SERVER").unwrap_or_default(),
        p4e_skytable::REPLY_DELAY.as_millis()
    );

    // 1. One uncancelled run of the exact scenario.
    let mut env = Env::setup(variant, 9001).await.expect("preflight setup");
    env.write(TARGET_V).await.expect("uncancelled target");
    let after_target = env.observer_read().await.expect("observer after target");
    let write = env.write(SURVIVOR_V).await;
    let own = env.read().await;
    let visible = env.observer_read().await;
    env.snapshot("uncancelled");
    println!(
        "SKY-PREFLIGHT-NORMAL variant={} after_target_observer={after_target} \
         survivor_write={write:?} own={own:?} visible={visible:?}",
        variant.name()
    );
    env.cleanup().await.expect("cleanup");

    // 2. Reuse/reconnect diagnostic: three sequential writes with no
    // cancellation. Pool size is 1, so a legal reconnect would show up as a
    // second proxy connection.
    let mut env = Env::setup(variant, 9002).await.expect("reuse setup");
    let mut states = Vec::new();
    for value in [2u64, 3, 4] {
        let write = env.write(value).await;
        let read = env.read().await;
        let proxy_connections = env.proxy.connections().len();
        let pool = env
            .pool
            .as_ref()
            .map(|pool| format!("{:?}", pool.state()))
            .unwrap_or_else(|| "n/a".into());
        states.push(format!(
            "value={value} write={write:?} read={read:?} proxy_connections={proxy_connections} pool={pool}"
        ));
    }
    println!("SKY-REUSE variant={} steps=[{}]", variant.name(), states.join(" | "));
    env.cleanup().await.expect("cleanup");

    // 3. Manual pre/post calibration, two samples each. `pre` skips the target
    // (the same state as dropping it before its first poll); `post` completes it.
    let mut pre = Vec::new();
    let mut post = Vec::new();
    for _ in 0..2 {
        pre.push(calibration(variant, run_id(), false).await.expect("pre"));
        post.push(calibration(variant, run_id(), true).await.expect("post"));
    }
    println!("SKY-CAL-PRE variant={} obs={pre:?}", variant.name());
    println!("SKY-CAL-POST variant={} obs={post:?}", variant.name());
    println!(
        "SKY-CAL variant={} pre_equal={} post_equal={} pre_post_equal={}",
        variant.name(),
        pre[0] == pre[1],
        post[0] == post[1],
        pre[0] == post[0]
    );

    // 4. Observer-visible effect of the completed target.
    let mut env = Env::setup(variant, 9003).await.expect("effect setup");
    let before = env.observer_read().await.expect("observer before");
    env.write(TARGET_V).await.expect("target write");
    let after = env.observer_read().await.expect("observer after");
    println!(
        "SKY-OBSERVER-EFFECT variant={} before={before} after={after} distinguishes={}",
        variant.name(),
        before != after
    );
    env.cleanup().await.expect("cleanup");
}

async fn recovery(variant: Variant) {
    let mut env = Env::setup(variant, 9004).await.expect("recovery setup");
    // Diagnostic cut with the same proxy-delay window, but a manual 10 ms
    // competitor instead of a planned plan (preflight/recovery are not levels).
    let cancelled = {
        let write = env.write(TARGET_V);
        tokio::pin!(write);
        tokio::select! {
            result = &mut write => { result.expect("write before recovery"); false }
            _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => { true }
        }
    };
    let before_reconnect = env.observer_read().await;
    // Contrast only: a brand-new direct connection, not a same-handle recovery.
    let mut fresh = env.server.config().connect_async().await.expect("fresh connect");
    let select = p4e_skytable::select_query(&env.space, p4e_skytable::OBSERVER_KEY);
    let fresh_read = match fresh.query(&select).await {
        Ok(skytable::response::Response::Row(row)) => {
            row.into_first_as::<u64>().map_err(|err| format!("{err:?}"))
        }
        Ok(other) => Err(format!("unexpected response {other:?}")),
        Err(err) => Err(format!("{err:?}")),
    };
    let update = p4e_skytable::update_query(&env.space, SURVIVOR_V, p4e_skytable::OBSERVER_KEY);
    let fresh_write = match fresh.query(&update).await {
        Ok(skytable::response::Response::Empty) => Ok(()),
        Ok(other) => Err(format!("unexpected response {other:?}")),
        Err(err) => Err(format!("{err:?}")),
    };
    let select = p4e_skytable::select_query(&env.space, p4e_skytable::OBSERVER_KEY);
    let fresh_readback = match fresh.query(&select).await {
        Ok(skytable::response::Response::Row(row)) => {
            row.into_first_as::<u64>().map_err(|err| format!("{err:?}"))
        }
        Ok(other) => Err(format!("unexpected response {other:?}")),
        Err(err) => Err(format!("{err:?}")),
    };
    let observer = env.observer_read().await;
    println!(
        "SKY-RECOVERY variant={} cancelled={cancelled} observer_before_reconnect={before_reconnect:?} \
         fresh_read={fresh_read:?} fresh_write={fresh_write:?} fresh_readback={fresh_readback:?} \
         observer_after={observer:?}",
        variant.name()
    );
    env.cleanup().await.expect("cleanup");
}

fn ladder_main(variant: Variant, rep: usize) {
    println!(
        "SKY-LADDER-BEGIN variant={} rep={rep} config=max_runs:64,races:[Immediate,AfterWake],max_cancellations:1,scenario_timeout:20s,settle:0/0,workers:1",
        variant.name()
    );

    let b1 = explore(&config(), |ctx| async move {
        let mut env = Env::setup(variant, run_id()).await?;
        let _ = cut(&ctx, &mut env).await?;
        let _ = bounded(BOUND, "cleanup", env.cleanup()).await;
        Ok(())
    });
    summary("B1", variant, &b1);

    let b2_report = explore(&config(), |ctx| async move {
        let mut env = Env::setup(variant, run_id()).await?;
        let cancelled = cut(&ctx, &mut env).await?;
        if cancelled {
            match bounded(BOUND, "retry of the same update", env.retry_same()).await {
                Ok(()) => println!("SKY-VALUE variant={} level=B2 retry=Ok", variant.name()),
                Err(err) => {
                    println!("SKY-VALUE variant={} level=B2 retry=Err({err})", variant.name());
                    return Err(format!("retry of the same update failed: {err}"));
                }
            }
        }
        env.snapshot("after_retry");
        let _ = bounded(BOUND, "cleanup", env.cleanup()).await;
        Ok(())
    });
    summary("B2", variant, &b2_report);

    let b3_report = explore(&config(), |ctx| async move {
        let mut env = Env::setup(variant, run_id()).await?;
        let cancelled = cut(&ctx, &mut env).await?;
        if cancelled {
            // The concrete value is printed but B3 only judges Ok.
            match bounded(BOUND, "different read", env.different_op()).await {
                Ok(value) => {
                    println!("SKY-VALUE variant={} level=B3 read=Ok({value})", variant.name())
                }
                Err(err) => {
                    println!("SKY-VALUE variant={} level=B3 read=Err({err})", variant.name());
                    return Err(format!("different operation failed: {err}"));
                }
            }
        }
        env.snapshot("after_b3");
        let _ = bounded(BOUND, "cleanup", env.cleanup()).await;
        Ok(())
    });
    summary("B3", variant, &b3_report);

    let m1 = explore_observed(&config(), Expect::Atomic, |ctx| async move {
        let mut env = Env::setup(variant, run_id()).await?;
        let _ = cut(&ctx, &mut env).await?;
        observed_after_cut(&mut env).await
    });
    summary("M1", variant, &m1);

    println!(
        "SKY-LADDER-END variant={} rep={rep} total_plans={} total_violations={}",
        variant.name(),
        b1.trials.len() + b2_report.trials.len() + b3_report.trials.len() + m1.trials.len(),
        b1.violations().count()
            + b2_report.violations().count()
            + b3_report.violations().count()
            + m1.violations().count()
    );
}

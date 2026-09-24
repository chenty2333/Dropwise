//! Dropwise confirmation models for the 16 Q2 true-risk samples.
//!
//! These are deliberately labelled `modelled`: none invokes the target
//! application's compiled function. Each module copies the state transition
//! and cancellation boundary from the pinned source lines cited in FINDINGS.md.

use std::time::Duration;

use dropwise::{Config, Ctx, Race, Report, explore};

fn config() -> Config {
    Config {
        races: vec![Race::Immediate],
        scenario_timeout: Duration::from_secs(2),
        ..Config::default()
    }
}

async fn boundary() {
    tokio::task::yield_now().await;
}

fn assert_modelled_violation(report: Report, id: &str) {
    assert!(
        report.baseline_errors.is_empty(),
        "{id}: model baseline failed:\n{report}"
    );
    assert!(report.exhaustive, "{id}: incomplete exploration:\n{report}");
    assert!(
        report.baseline_settled,
        "{id}: baseline did not settle:\n{report}"
    );
    assert!(
        report
            .trials
            .iter()
            .any(|t| { t.outcomes == [dropwise::CutOutcome::Cancelled] && t.invariant.is_err() }),
        "{id}: no realized cancellation violated the model invariant:\n{report}"
    );
    assert!(
        report.unrealized().next().is_none(),
        "{id}: unrealized cut:\n{report}"
    );
    assert!(
        report.unsettled().next().is_none(),
        "{id}: unsettled scenario:\n{report}"
    );
}

// qdrant/qdrant@6ab21cac: lib/collection/src/shards/local_shard/updaters.rs:134-140.
// Installing the config precedes the async Nop trigger; that signal is the
// model's obligation to wake the re-created optimizer worker.
#[test]
fn qdrant_optimizer_trigger() {
    struct Shard {
        config_installed: bool,
        optimizer_triggered: bool,
    }
    async fn update(s: &mut Shard) {
        s.config_installed = true;
        boundary().await; // update_worker.send(Nop).await
        s.optimizer_triggered = true;
    }
    let report = explore(&config(), |ctx: Ctx| async move {
        let mut s = Shard {
            config_installed: false,
            optimizer_triggered: false,
        };
        let _ = ctx.target(update(&mut s)).await;
        if s.config_installed != s.optimizer_triggered {
            Err("installed optimizer config without its worker trigger".into())
        } else {
            Ok(())
        }
    });
    assert_modelled_violation(report, "qdrant optimizer trigger");
}

// databendlabs/databend@3a1be1e: src/common/storage/src/runtime_layer.rs:393-406.
#[test]
fn databend_runtime_writer_take() {
    struct Writer {
        inner: Option<u8>,
    }
    async fn flush(w: &mut Writer) {
        let inner = w.inner.take().expect("writer present");
        boundary().await; // spawned join handle
        w.inner = Some(inner);
    }
    let report = explore(&config(), |ctx: Ctx| async move {
        let mut w = Writer { inner: Some(1) };
        let _ = ctx.target(flush(&mut w)).await;
        if w.inner.is_none() {
            Err("writer inner lost across cancelled flush".into())
        } else {
            Ok(())
        }
    });
    assert_modelled_violation(report, "databend RuntimeIO::flush");
}

// databendlabs/databend@3a1be1e: transform_serialize_segment.rs:382-399.
#[test]
fn databend_segment_state_replace() {
    struct Processor {
        state: Option<u8>,
    }
    async fn process(p: &mut Processor) {
        let segment = p.state.take().expect("segment ready"); // mem::replace(state, None)
        boundary().await; // write segment
        p.state = Some(segment);
    }
    let report = explore(&config(), |ctx: Ctx| async move {
        let mut p = Processor { state: Some(1) };
        let _ = ctx.target(process(&mut p)).await;
        if p.state.is_none() {
            Err("segment processor left in empty state".into())
        } else {
            Ok(())
        }
    });
    assert_modelled_violation(report, "databend segment state");
}

// databendlabs/databend@3a1be1e: fuse_rows_fetcher.rs:234-249,305-308.
#[test]
fn databend_rows_input_take() {
    struct Fetcher {
        input: Option<Vec<u8>>,
        output: usize,
    }
    async fn process(f: &mut Fetcher) {
        let rows = f.input.take().expect("input present");
        boundary().await; // metadata fetch within the row loop
        f.output += rows.len();
        f.input = Some(Vec::new());
    }
    let report = explore(&config(), |ctx: Ctx| async move {
        let mut f = Fetcher {
            input: Some(vec![1, 2]),
            output: 0,
        };
        let _ = ctx.target(process(&mut f)).await;
        if f.input.is_none() {
            Err("input rows detached and not restored".into())
        } else {
            Ok(())
        }
    });
    assert_modelled_violation(report, "databend FuseRowsFetcher");
}

// databendlabs/databend@3a1be1e: collect_ndv_source.rs:582-596,627-637,727-752.
#[test]
fn databend_ndv_state_replace() {
    #[derive(Debug, PartialEq)]
    enum State {
        Collect,
        Finish,
    }
    struct Source {
        state: State,
        pending_segments: usize,
    }
    async fn process(s: &mut Source) {
        let prior = std::mem::replace(&mut s.state, State::Finish);
        assert_eq!(prior, State::Collect);
        boundary().await; // metadata/block read before state is re-established
        s.pending_segments = 0;
        s.state = State::Collect;
    }
    let report = explore(&config(), |ctx: Ctx| async move {
        let mut s = Source {
            state: State::Collect,
            pending_segments: 2,
        };
        let _ = ctx.target(process(&mut s)).await;
        if s.state == State::Finish && s.pending_segments != 0 {
            Err("NDV source records Finish while work remains".into())
        } else {
            Ok(())
        }
    });
    assert_modelled_violation(report, "databend NDV source");
}

// GreptimeTeam/greptimedb@c7fa48e: src/common/base/src/range_read.rs:243-259.
#[test]
fn greptimedb_range_read_cursor() {
    struct Reader {
        position: usize,
        file_cursor: usize,
    }
    async fn read(r: &mut Reader) {
        let start = 8;
        r.position = start;
        r.file_cursor = start + 1; // read_exact consumed a partial prefix
        boundary().await;
        r.file_cursor = start + 4;
        r.position = start + 4;
    }
    let report = explore(&config(), |ctx: Ctx| async move {
        let mut r = Reader {
            position: 0,
            file_cursor: 0,
        };
        let _ = ctx.target(read(&mut r)).await;
        if r.position != r.file_cursor {
            Err("recorded range position disagrees with file cursor".into())
        } else {
            Ok(())
        }
    });
    assert_modelled_violation(report, "Greptime range reader");
}

// GreptimeTeam/greptimedb@c7fa48e: src/common/datasource/src/parquet_writer.rs:105-149.
#[test]
fn greptimedb_parquet_encoder_take() {
    struct Writer {
        encoder: Option<u8>,
    }
    async fn write(w: &mut Writer) {
        let encoder = w.encoder.take().expect("writer open");
        boundary().await; // spawn_blocking JoinHandle
        w.encoder = Some(encoder);
    }
    let report = explore(&config(), |ctx: Ctx| async move {
        let mut w = Writer { encoder: Some(1) };
        let _ = ctx.target(write(&mut w)).await;
        if w.encoder.is_none() {
            Err("cancelled parquet write lost its encoder".into())
        } else {
            Ok(())
        }
    });
    assert_modelled_violation(report, "Greptime parquet encoder");
}

// GreptimeTeam/greptimedb@c7fa48e: src/mito2/src/sst/parquet/reader.rs:2482-2501.
#[test]
fn greptimedb_row_group_pop() {
    struct Reader {
        selection: Vec<usize>,
        current: Option<usize>,
    }
    async fn next(r: &mut Reader) {
        let group = r.selection.pop().expect("selection available");
        boundary().await; // construct row-group reader
        r.current = Some(group);
    }
    let report = explore(&config(), |ctx: Ctx| async move {
        let mut r = Reader {
            selection: vec![7],
            current: None,
        };
        let _ = ctx.target(next(&mut r)).await;
        if r.selection.is_empty() && r.current.is_none() {
            Err("row group popped but reader not installed".into())
        } else {
            Ok(())
        }
    });
    assert_modelled_violation(report, "Greptime parquet row-group reader");
}

// neondatabase/neon@fa50421: pageserver/src/tenant/storage_layer/image_layer.rs:840-877,1126-1128.
#[test]
fn neon_image_layer_counters() {
    struct Writer {
        num_keys: usize,
        appended_keys: usize,
    }
    async fn put(w: &mut Writer) {
        w.num_keys += 1;
        boundary().await; // blob write; index append is after it
        w.appended_keys += 1;
    }
    let report = explore(&config(), |ctx: Ctx| async move {
        let mut w = Writer {
            num_keys: 0,
            appended_keys: 0,
        };
        let _ = ctx.target(put(&mut w)).await;
        if w.num_keys != w.appended_keys {
            Err("image layer key count has no matching index entry".into())
        } else {
            Ok(())
        }
    });
    assert_modelled_violation(report, "Neon ImageLayerWriter");
}

// neondatabase/neon@fa50421: libs/proxy/postgres-protocol2/src/authentication/sasl.rs:209-236,284.
#[test]
fn neon_scram_state_replace() {
    #[derive(Debug, PartialEq)]
    enum State {
        Update,
        Done,
        Finish,
    }
    struct Scram {
        state: State,
    }
    async fn update(s: &mut Scram) {
        assert_eq!(s.state, State::Update);
        s.state = State::Done; // mem::replace(Update, Done)
        boundary().await; // hi()/PBKDF2 yields
        s.state = State::Finish;
    }
    let report = explore(&config(), |ctx: Ctx| async move {
        let mut s = Scram {
            state: State::Update,
        };
        let _ = ctx.target(update(&mut s)).await;
        if s.state == State::Done {
            Err("SCRAM object cannot accept retry after cancelled update".into())
        } else {
            Ok(())
        }
    });
    assert_modelled_violation(report, "Neon SCRAM update");
}

// neondatabase/neon@fa50421: pageserver/src/tenant/storage_layer/image_layer.rs:1165-1196.
#[test]
fn neon_image_layer_iterator_tail() {
    struct Iterator {
        is_end: bool,
        tail_yielded: bool,
    }
    async fn next_batch(i: &mut Iterator) {
        i.is_end = true; // last plan, before read_blobs/read().await
        boundary().await;
        i.tail_yielded = true;
    }
    let report = explore(&config(), |ctx: Ctx| async move {
        let mut i = Iterator {
            is_end: false,
            tail_yielded: false,
        };
        let _ = ctx.target(next_batch(&mut i)).await;
        if i.is_end && !i.tail_yielded {
            Err("iterator marked end before yielding final batch".into())
        } else {
            Ok(())
        }
    });
    assert_modelled_violation(report, "Neon ImageLayerIterator");
}

// cloudflare/pingora@4487f7b: pingora-core/src/connectors/http/v2.rs:166-180.
#[test]
fn pingora_h2_stream_slot() {
    struct Pool {
        current_streams: usize,
        sessions_created: usize,
    }
    async fn spawn_stream(p: &mut Pool) {
        p.current_streams += 1;
        boundary().await; // new_stream()
        p.sessions_created += 1;
    }
    let report = explore(&config(), |ctx: Ctx| async move {
        let mut p = Pool {
            current_streams: 0,
            sessions_created: 0,
        };
        let _ = ctx.target(spawn_stream(&mut p)).await;
        if p.current_streams != p.sessions_created {
            Err("H2 pool slot reserved without a stream session".into())
        } else {
            Ok(())
        }
    });
    assert_modelled_violation(report, "Pingora H2 stream pool");
}

// risingwavelabs/risingwave@0d32b95: src/stream/src/executor/top_n/top_n_cache.rs:401-434.
#[test]
fn risingwave_topn_partial_refill() {
    struct Cache {
        middle: Vec<u8>,
        high: Vec<u8>,
        staged_deletes: usize,
        expected_high: usize,
    }
    impl Cache {
        fn high_is_synced(&self) -> bool {
            !self.high.is_empty()
        }
    }
    async fn delete(c: &mut Cache) {
        c.middle.remove(0);
        c.staged_deletes += 1;
        c.high.clear();
        c.high.push(9); // first incremental fill before a later await
        boundary().await;
        c.high.push(10);
    }
    let report = explore(&config(), |ctx: Ctx| async move {
        let mut c = Cache {
            middle: vec![1, 2],
            high: vec![0, 1],
            staged_deletes: 0,
            expected_high: 2,
        };
        let _ = ctx.target(delete(&mut c)).await;
        if c.high_is_synced() && c.high.len() != c.expected_high {
            Err("non-empty partial TopN high cache is treated as synced".into())
        } else {
            Ok(())
        }
    });
    assert_modelled_violation(report, "RisingWave TopN cache");
}

// risingwavelabs/risingwave@0d32b95: src/storage/src/hummock/compactor/fast_compactor_runner.rs:263-281.
#[test]
fn risingwave_fast_concat_iterator_take() {
    struct Iter {
        sstable_iter: Option<u8>,
        cur_idx: usize,
    }
    async fn seek(i: &mut Iter) {
        let _old = i.sstable_iter.take().expect("iterator present");
        i.cur_idx = 4;
        boundary().await; // load replacement iterator
        i.sstable_iter = Some(4);
    }
    let report = explore(&config(), |ctx: Ctx| async move {
        let mut i = Iter {
            sstable_iter: Some(3),
            cur_idx: 3,
        };
        let _ = ctx.target(seek(&mut i)).await;
        if i.sstable_iter.is_none() {
            Err("cancelled seek leaves current iterator absent; next unwrap panics".into())
        } else {
            Ok(())
        }
    });
    assert_modelled_violation(report, "RisingWave FastConcatSstableIterator");
}

// risingwavelabs/risingwave@0d32b95: src/storage/src/hummock/compactor/iterator.rs:337-392.
#[test]
fn risingwave_concat_iterator_take() {
    struct Iter {
        sstable_iter: Option<u8>,
        cur_idx: usize,
    }
    async fn seek(i: &mut Iter) {
        let _old = i.sstable_iter.take().expect("iterator present");
        i.cur_idx += 1;
        boundary().await; // load new SST iterator
        i.sstable_iter = Some(i.cur_idx as u8);
    }
    let report = explore(&config(), |ctx: Ctx| async move {
        let mut i = Iter {
            sstable_iter: Some(3),
            cur_idx: 3,
        };
        let _ = ctx.target(seek(&mut i)).await;
        if i.sstable_iter.is_none() {
            Err("cancelled seek leaves iterator absent; next/key/value expect panics".into())
        } else {
            Ok(())
        }
    });
    assert_modelled_violation(report, "RisingWave ConcatSstableIterator");
}

// shotover/shotover-proxy@5cb1e0d: shotover/src/transforms/cassandra/sink_cluster/mod.rs:377-431,641-659.
#[test]
fn shotover_cassandra_handshake_flag() {
    struct Cluster {
        init_handshake_complete: bool,
        nodes_ready: bool,
    }
    async fn complete_handshake(c: &mut Cluster) {
        c.init_handshake_complete = true;
        boundary().await; // nodes_rx.changed() and control connection setup
        c.nodes_ready = true;
    }
    let report = explore(&config(), |ctx: Ctx| async move {
        let mut c = Cluster {
            init_handshake_complete: false,
            nodes_ready: false,
        };
        let _ = ctx.target(complete_handshake(&mut c)).await;
        if c.init_handshake_complete && !c.nodes_ready {
            Err("routing bypasses initialization while node topology is unavailable".into())
        } else {
            Ok(())
        }
    });
    assert_modelled_violation(report, "Shotover Cassandra handshake");
}

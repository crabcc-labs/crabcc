//! `AppState::apply` event-pump bench. The kickoff-review headline
//! finding was that Agents + System routes do 100–300 string allocs/sec
//! at SSE rate, mostly originating from re-renders driven by `apply`.
//! Establishing a baseline here lets PR-B (SharedString flip) and the
//! follow-up tweaks measure their own deltas instead of relying on
//! vibes.
//!
//! Run with `cargo bench -p crabcc-desktop --bench apply` from the
//! `crates/crabcc-desktop` directory. Criterion writes HTML reports
//! into `target/criterion/` — diff between two runs by passing
//! `--save-baseline before` then `--baseline before`.

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};

use crabcc_desktop::api::types::{
    AgentStatus, SseActivityEvent, SseActivityFrame, SseAgent, SseAgentsFrame,
};
use crabcc_desktop::sse::SseEvent;
use crabcc_desktop::state::{AppEvent, AppState};

/// 50 synthetic agents — matches the upper end of typical concurrent
/// agent counts on a busy crabcc workstation.
const FIXTURE_AGENTS: usize = 50;

/// 100 synthetic activity events — one full SSE buffer's worth (the
/// in-memory ring is capped at 64 entries, so this exercises the
/// pop_front + push_back path beyond the cap).
const FIXTURE_ACTIVITY_EVENTS: usize = 100;

fn synth_agents(n: usize) -> SseAgentsFrame {
    let agents = (0..n)
        .map(|i| SseAgent {
            id: format!("bench-agent-{i:03}").into(),
            status: if i % 4 == 0 {
                AgentStatus::Exited
            } else {
                AgentStatus::Running
            },
            started_ts: 1_700_000_000 + i as i64,
            pid: Some(10_000 + i as u64),
            runtime: Some("claude-code".into()),
            model: Some("sonnet-4-6".into()),
            prompt_preview: format!("synthetic prompt {i} — produces realistic alloc churn").into(),
            log_bytes: (i as u64) * 1024,
            root: Some(format!("/tmp/bench-{i:03}").into()),
        })
        .collect();
    SseAgentsFrame { agents }
}

fn synth_activity(n: usize) -> SseActivityFrame {
    let ops = ["sym", "refs", "callers", "outline", "fuzzy", "prefix"];
    let events = (0..n)
        .map(|i| SseActivityEvent {
            ts: 1_700_000_000 + i as i64,
            op: ops[i % ops.len()].into(),
            query: format!("Query{i:04}").into(),
            results: (i as u64) % 100,
        })
        .collect();
    SseActivityFrame {
        repo: "bench".into(),
        cursor: n as i64,
        events,
    }
}

fn bench_apply_agents_frame(c: &mut Criterion) {
    let frame = synth_agents(FIXTURE_AGENTS);
    c.bench_function("apply_agents_frame_50", |b| {
        b.iter_batched(
            // Fresh AppState per iter so steady-state alloc churn isn't
            // hidden behind reuse — the realistic case is "first frame
            // after connect", repeated.
            AppState::new,
            |mut state| {
                state.apply(black_box(AppEvent::Sse(SseEvent::Agents(frame.clone()))));
            },
            BatchSize::SmallInput,
        );
    });
}

fn bench_apply_activity_burst(c: &mut Criterion) {
    let frame = synth_activity(FIXTURE_ACTIVITY_EVENTS);
    c.bench_function("apply_activity_burst_100", |b| {
        b.iter_batched(
            AppState::new,
            |mut state| {
                // One frame carrying 100 events → the buffer cap kicks
                // in midway, giving the bench a representative mix of
                // push_back and pop_front + push_back work.
                state.apply(black_box(AppEvent::Sse(SseEvent::Activity(frame.clone()))));
            },
            BatchSize::SmallInput,
        );
    });
}

fn bench_apply_activity_drip(c: &mut Criterion) {
    // Many small frames is the realistic SSE shape — `crabcc serve`
    // emits one event per crate operation, not bursts. Measure the
    // per-event amortised cost.
    let single_frames: Vec<SseActivityFrame> = (0..FIXTURE_ACTIVITY_EVENTS)
        .map(|i| SseActivityFrame {
            repo: "bench".into(),
            cursor: i as i64,
            events: vec![SseActivityEvent {
                ts: 1_700_000_000 + i as i64,
                op: "sym".into(),
                query: format!("Q{i}").into(),
                results: 1,
            }],
        })
        .collect();
    c.bench_function("apply_activity_drip_100x1", |b| {
        b.iter_batched(
            || (AppState::new(), single_frames.clone()),
            |(mut state, frames)| {
                for f in frames {
                    state.apply(AppEvent::Sse(SseEvent::Activity(f)));
                }
                black_box(state);
            },
            BatchSize::SmallInput,
        );
    });
}

// `bench_apply_services` is deliberately omitted from PR-A —
// `ServiceStatus` carries 10 required fields (incl. `kind: ServiceKind`
// enum + `host`/`port`/`probed_at`) that need a richer fixture
// generator. Lands alongside PR-B (SharedString flip) where the System
// route's alloc surface is in scope.

criterion_group!(
    benches,
    bench_apply_agents_frame,
    bench_apply_activity_burst,
    bench_apply_activity_drip,
);
criterion_main!(benches);

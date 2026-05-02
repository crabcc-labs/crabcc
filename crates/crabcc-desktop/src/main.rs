// Phase A.3 — gpui shell + live SSE bridge.
//
// Static repo / version panel from A.1 stays. On top, the window now
// reflects live counts driven by the SSE worker (`src/sse.rs`):
//
//   live · activity hits: N · agents running: M
//
// Pump topology:
//   1. `sse::spawn_worker` runs on its own OS thread, parses
//      `event:`/`data:` frames from `/api/events`, sends `SseEvent`
//      values down a `flume` channel.
//   2. `Shell::new` spawns a gpui task that drains the channel via
//      `recv_async()` and `cx.update_entity` to mutate state +
//      `cx.notify()`. Render observes notifications and redraws.
//
// See docs/RESEARCH-native-desktop-and-rich-notifications.md (Track A).

use crabcc_desktop::api::DEFAULT_BASE_URL;
use crabcc_desktop::sse::{self, SseEvent};
use gpui::{
    div, prelude::*, px, size, App, Bounds, Context, IntoElement, Render, SharedString,
    TitlebarOptions, Window, WindowBounds, WindowOptions,
};
use gpui_component::{h_flex, v_flex, Root};

const WINDOW_TITLE: &str = "crabcc · live";

struct Shell {
    repo: SharedString,
    version: SharedString,
    activity_hits: u64,
    agents_running: u32,
    agents_total: u32,
    last_topic: SharedString,
}

impl Shell {
    fn new(cx: &mut Context<Self>, rx: flume::Receiver<SseEvent>) -> Self {
        // Drain the SSE channel forever, mutating self via the weak
        // handle gpui passes into the closure. The task drops cleanly
        // when the window closes (upgrade returns None).
        cx.spawn(async move |this_weak, cx| {
            while let Ok(evt) = rx.recv_async().await {
                let Some(this) = this_weak.upgrade() else {
                    return;
                };
                this.update(cx, |this, cx| {
                    this.apply(evt);
                    cx.notify();
                });
            }
        })
        .detach();

        Self {
            repo: env!("CARGO_PKG_REPOSITORY").into(),
            version: env!("CARGO_PKG_VERSION").into(),
            activity_hits: 0,
            agents_running: 0,
            agents_total: 0,
            last_topic: SharedString::new_static("waiting"),
        }
    }

    fn apply(&mut self, evt: SseEvent) {
        match evt {
            SseEvent::Activity(frame) => {
                self.activity_hits = self.activity_hits.saturating_add(frame.events.len() as u64);
                self.last_topic = SharedString::new_static("activity");
            }
            SseEvent::Agents(frame) => {
                self.agents_total = frame.agents.len() as u32;
                self.agents_running = frame
                    .agents
                    .iter()
                    .filter(|a| matches!(a.status, crabcc_desktop::api::types::AgentStatus::Running))
                    .count() as u32;
                self.last_topic = SharedString::new_static("agents");
            }
            SseEvent::Unknown { topic, .. } => {
                self.last_topic = SharedString::from(topic);
            }
        }
    }
}

impl Render for Shell {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_4()
            .child(
                div()
                    .text_2xl()
                    .child(SharedString::new_static("crabcc · live")),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(SharedString::new_static("v"))
                    .child(self.version.clone()),
            )
            .child(self.repo.clone())
            .child(
                h_flex()
                    .gap_4()
                    .child(SharedString::from(format!(
                        "activity hits: {}",
                        self.activity_hits
                    )))
                    .child(SharedString::from(format!(
                        "agents: {}/{} running",
                        self.agents_running, self.agents_total
                    )))
                    .child(SharedString::from(format!(
                        "last: {}",
                        self.last_topic
                    ))),
            )
    }
}

fn main() {
    let app = gpui_platform::application();

    // Spawn the SSE worker before app.run() so `Shell::new` always
    // sees a live receiver. Worker reconnects + backs off on its own
    // — no orchestration needed here.
    let rx = sse::spawn_worker(DEFAULT_BASE_URL);

    app.run(move |cx: &mut App| {
        gpui_component::init(cx);

        let bounds = Bounds::centered(None, size(px(1280.0), px(800.0)), cx);

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some(WINDOW_TITLE.into()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let rx = rx.clone();
        cx.spawn(async move |cx| {
            cx.open_window(options, |window, cx| {
                let shell = cx.new(|cx| Shell::new(cx, rx));
                cx.new(|cx| Root::new(shell, window, cx))
            })
            .expect("failed to open window");
        })
        .detach();
    });
}

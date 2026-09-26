//! Sonos.app — macOS Sonos controller (GPUI UI layer).
//!
//! The protocol lives in the `sonos` lib crate, GPUI-free; this binary only
//! renders state and issues control commands (see docs/decisions.md).

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{
    App, Application, AsyncApp, Bounds, Context, DragMoveEvent, FontWeight, Render, Task,
    TitlebarOptions, WeakEntity, Window, WindowBounds, WindowOptions, div, prelude::*, px, rgb,
    size,
};
use sonos::{GroupView, SystemState, control, discover, snapshot};

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const DISCOVER_TIMEOUT: Duration = Duration::from_secs(2);
const SLIDER_WIDTH: f32 = 160.;
const VOLUME_SEND_INTERVAL: Duration = Duration::from_millis(120);

fn accent() -> gpui::Rgba {
    rgb(0xe8a13d)
}

fn dim() -> gpui::Rgba {
    rgb(0x8b8b96)
}

#[derive(Clone, Copy)]
struct SliderDrag {
    group: usize,
}

/// Invisible drag ghost; GPUI requires a Render view for drag sources.
struct SliderGhost {}

impl Render for SliderGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

struct SonosApp {
    state: Option<SystemState>,
    add_menu: Option<usize>,
    /// Fire-and-forget SOAP tasks. Dropping a pending Task cancels it, so
    /// they are parked here; old ones have long completed by the time this
    /// fills up.
    tasks: Vec<Task<()>>,
    /// Latest desired group volume by coordinator IP. Drag events arrive far
    /// faster than SOAP calls complete; the sender loop coalesces to the
    /// newest value per group.
    desired: Arc<Mutex<HashMap<Ipv4Addr, u8>>>,
    _poll: Option<Task<()>>,
    _sender: Option<Task<()>>,
}

impl SonosApp {
    fn new(cx: &mut Context<Self>) -> Self {
        let desired: Arc<Mutex<HashMap<Ipv4Addr, u8>>> = Arc::new(Mutex::new(HashMap::new()));
        let sender_desired = desired.clone();
        let sender = cx.spawn(async move |_this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let mut last_sent: HashMap<Ipv4Addr, u8> = HashMap::new();
            loop {
                cx.background_executor().timer(VOLUME_SEND_INTERVAL).await;
                let mut pending: Vec<(Ipv4Addr, u8)> = Vec::new();
                {
                    let desired = sender_desired.lock().unwrap();
                    for (ip, volume) in desired.iter() {
                        if last_sent.get(ip) != Some(volume) {
                            pending.push((*ip, *volume));
                        }
                    }
                }
                if pending.is_empty() {
                    continue;
                }
                let results = cx
                    .background_executor()
                    .spawn(async move {
                        let mut results = Vec::new();
                        for (ip, volume) in &pending {
                            results.push((
                                *ip,
                                *volume,
                                control::set_group_volume(*ip, *volume).is_ok(),
                            ));
                        }
                        results
                    })
                    .await;
                for (ip, volume, sent) in results {
                    if sent {
                        last_sent.insert(ip, volume);
                    }
                }
            }
        });
        let poll = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            loop {
                let ips = cx
                    .background_executor()
                    .spawn(async move { discover(DISCOVER_TIMEOUT).unwrap_or_default() })
                    .await;
                let snapshot_ips = ips.clone();
                let state = cx
                    .background_executor()
                    .spawn(async move { snapshot(&snapshot_ips) })
                    .await;
                let has_groups = !state.groups.is_empty();
                this.update(cx, |this, cx| {
                    if has_groups || this.state.is_none() {
                        this.state = Some(state);
                    }
                    cx.notify();
                })
                .ok();
                cx.background_executor().timer(POLL_INTERVAL).await;
            }
        });
        Self {
            state: None,
            add_menu: None,
            tasks: Vec::new(),
            desired,
            _poll: Some(poll),
            _sender: Some(sender),
        }
    }

    /// Run a blocking LAN call off the UI thread; the poll loop corrects any
    /// optimistic state within POLL_INTERVAL.
    fn fire(&mut self, cx: &mut Context<Self>, job: impl FnOnce() + Send + 'static) {
        self.tasks
            .push(cx.background_executor().spawn(async move { job() }));
        if self.tasks.len() > 64 {
            self.tasks.drain(0..32);
        }
    }

    fn coordinator_ip(&self, group: usize) -> Option<Ipv4Addr> {
        self.state
            .as_ref()?
            .groups
            .get(group)?
            .group
            .coordinator_ip()
    }

    fn set_volume(&mut self, group: usize, volume: u8, cx: &mut Context<Self>) {
        if let Some(view) = self.state.as_mut().and_then(|s| s.groups.get_mut(group)) {
            view.volume = Some(volume);
        }
        if let Some(ip) = self.coordinator_ip(group) {
            self.desired.lock().unwrap().insert(ip, volume);
        }
        cx.notify();
    }

    fn step_volume(&mut self, group: usize, delta: i32, cx: &mut Context<Self>) {
        let current = self
            .state
            .as_ref()
            .and_then(|s| s.groups.get(group))
            .and_then(|view| view.volume)
            .unwrap_or(0) as i32;
        let next = (current + delta).clamp(0, 100) as u8;
        self.set_volume(group, next, cx);
    }

    fn leave(&mut self, room_ip: Ipv4Addr, cx: &mut Context<Self>) {
        self.fire(cx, move || {
            let _ = control::leave(room_ip);
        });
    }

    fn join(&mut self, room_ip: Ipv4Addr, coordinator_uuid: String, cx: &mut Context<Self>) {
        self.add_menu = None;
        cx.notify();
        self.fire(cx, move || {
            let _ = control::join(room_ip, &coordinator_uuid);
        });
    }

    fn note(text: &str) -> impl IntoElement {
        div().text_color(dim()).child(text.to_string())
    }

    fn step_button(
        &self,
        index: usize,
        label: &str,
        delta: i32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(if delta < 0 {
                ("minus", index)
            } else {
                ("plus", index)
            })
            .flex()
            .items_center()
            .justify_center()
            .size_6()
            .rounded_full()
            .bg(rgb(0x33333c))
            .cursor_pointer()
            .hover(|this| this.bg(rgb(0x43434e)))
            .child(label.to_string())
            .on_click(cx.listener(move |this, _: &gpui::ClickEvent, _, cx| {
                this.step_volume(index, delta, cx);
            }))
    }

    fn volume_slider(&self, index: usize, volume: u8, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id(("slider", index))
            .flex()
            .items_center()
            .w(px(SLIDER_WIDTH))
            .h_6()
            .cursor_pointer()
            .on_drag(SliderDrag { group: index }, |_: &SliderDrag, _, _, cx| {
                cx.new(|_| SliderGhost {})
            })
            .on_drag_move::<SliderDrag>(cx.listener(
                move |this, ev: &DragMoveEvent<SliderDrag>, _, cx| {
                    // During a drag, every on_drag_move listener of this
                    // payload type fires; only the originating slider acts.
                    if ev.drag(cx).group != index {
                        return;
                    }
                    let fraction = (ev.event.position.x - ev.bounds.left()) / ev.bounds.size.width;
                    let volume = (fraction.clamp(0., 1.) * 100.).round() as u8;
                    this.set_volume(index, volume, cx);
                },
            ))
            .child(
                div()
                    .w_full()
                    .h_1()
                    .rounded_full()
                    .bg(rgb(0x3d3d47))
                    .flex()
                    .child(
                        div()
                            .w(px(SLIDER_WIDTH * (volume as f32 / 100.)))
                            .h_full()
                            .rounded_full()
                            .bg(accent()),
                    ),
            )
    }

    fn group_card(
        &self,
        index: usize,
        view: &GroupView,
        state: &SystemState,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let group = &view.group;
        let volume = view.volume.unwrap_or(0);
        let playing = view.transport_state.as_deref() == Some("PLAYING");
        let now_playing = match view.now_playing.as_ref() {
            Some(np) if np.title.is_some() => format!(
                "{} — {}",
                np.title.as_deref().unwrap_or_default(),
                np.artist.as_deref().unwrap_or(""),
            ),
            Some(np) if playing => match (&np.rel_time, &np.track_duration) {
                (Some(rel), Some(dur)) => format!("{rel} / {dur}"),
                _ => "Playing (line-in / TV)".to_string(),
            },
            _ if playing => "Playing (line-in / TV)".to_string(),
            _ => "Nothing playing".to_string(),
        };

        let coordinator_uuid = group.coordinator_uuid.clone();
        let multi_room = group.rooms.len() > 1;
        // Display signal for real multi-room groups (a bonded stereo pair is
        // one visible room, not a group).
        let grouped = group.visible_rooms().count() > 1;

        // Rooms that can join this group: standalone rooms from other groups.
        let joinable: Vec<(String, Ipv4Addr)> = state
            .groups
            .iter()
            .enumerate()
            .filter(|(i, g)| *i != index && g.group.rooms.len() == 1)
            .filter_map(|(_, g)| {
                g.group
                    .rooms
                    .first()
                    .filter(|room| !room.invisible)
                    .map(|room| (room.name.clone(), room.ip))
            })
            .collect();

        div()
            .id(("group", index))
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .rounded_lg()
            .bg(rgb(0x26262c))
            .when(grouped, |card| card.border_1().border_color(rgb(0x5c4a26)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().text_lg().child(group.label()))
                            .when(grouped, |row| {
                                row.child(
                                    div()
                                        .px_2()
                                        .rounded_full()
                                        .bg(rgb(0x3a3123))
                                        .text_xs()
                                        .text_color(accent())
                                        .child("grouped"),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(dim())
                                    .child(format!("{volume}%")),
                            )
                            .child(
                                div()
                                    .id(("add", index))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size_5()
                                    .rounded_full()
                                    .bg(rgb(0x3a3a44))
                                    .text_xs()
                                    .cursor_pointer()
                                    .hover(|this| this.bg(rgb(0x4a4a56)))
                                    .child("+")
                                    .on_click(cx.listener(
                                        move |this, _: &gpui::ClickEvent, _, cx| {
                                            this.add_menu = if this.add_menu == Some(index) {
                                                None
                                            } else {
                                                Some(index)
                                            };
                                            cx.notify();
                                        },
                                    )),
                            ),
                    ),
            )
            .child(
                div()
                    .text_color(if playing { rgb(0xd8d8de) } else { dim() })
                    .child(now_playing),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(self.step_button(index, "−", -2, cx))
                    .child(self.volume_slider(index, volume, cx))
                    .child(self.step_button(index, "+", 2, cx)),
            )
            .when(grouped, |card| {
                card.child(div().flex().flex_wrap().gap_2().children(
                    group.visible_rooms().enumerate().map(|(chip_ix, room)| {
                        let room_ip = room.ip;
                        let can_leave = multi_room && room.uuid != coordinator_uuid;
                        div()
                            .id(("chip", index * 16 + chip_ix))
                            .flex()
                            .items_center()
                            .gap_1()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(if grouped {
                                rgb(0x3f3826)
                            } else {
                                rgb(0x33333c)
                            })
                            .text_xs()
                            .child(room.name.clone())
                            .when(can_leave, |chip| {
                                chip.cursor_pointer()
                                    .hover(|this| this.bg(rgb(0x43434e)))
                                    .child(div().text_color(dim()).child("×"))
                                    .on_click(cx.listener(
                                        move |this, _: &gpui::ClickEvent, _, cx| {
                                            this.leave(room_ip, cx);
                                        },
                                    ))
                            })
                    }),
                ))
            })
            .when(
                self.add_menu == Some(index) && !joinable.is_empty(),
                |card| {
                    card.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .p_2()
                            .rounded_md()
                            .bg(rgb(0x1f1f24))
                            .child(div().text_xs().text_color(dim()).child("Add a room"))
                            .children(joinable.iter().enumerate().map(
                                |(join_ix, (name, room_ip))| {
                                    let coordinator_uuid = coordinator_uuid.clone();
                                    let room_ip = *room_ip;
                                    div()
                                        .id(("join", index * 16 + join_ix))
                                        .px_2()
                                        .py_1()
                                        .rounded_md()
                                        .cursor_pointer()
                                        .hover(|this| this.bg(rgb(0x3a3a44)))
                                        .child(name.clone())
                                        .on_click(cx.listener(
                                            move |this, _: &gpui::ClickEvent, _, cx| {
                                                this.join(room_ip, coordinator_uuid.clone(), cx);
                                            },
                                        ))
                                },
                            )),
                    )
                },
            )
    }
}

impl Render for SonosApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.clone();
        let mut cards: Vec<gpui::Stateful<gpui::Div>> = Vec::new();
        if let Some(state) = &state {
            for (index, view) in state.groups.iter().enumerate() {
                cards.push(self.group_card(index, view, state, cx));
            }
        }

        div()
            .id("main")
            .flex()
            .flex_col()
            .size_full()
            .overflow_y_scroll()
            .bg(rgb(0x1b1b1f))
            .p_4()
            .gap_3()
            .text_sm()
            .text_color(rgb(0xe8e8ec))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Sonos"),
                    )
                    .when_some(state.as_ref().map(|s| s.groups.len()), |header, len| {
                        header.child(
                            div()
                                .text_xs()
                                .text_color(dim())
                                .child(format!("{len} groups")),
                        )
                    }),
            )
            .when(state.is_none(), |app| {
                app.child(Self::note("Looking for speakers…"))
            })
            .when(state.as_ref().is_some_and(|s| s.groups.is_empty()), |app| {
                app.child(Self::note("No speakers found"))
            })
            .children(cards)
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(420.), px(880.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Sonos".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |_, cx| cx.new(|cx| SonosApp::new(cx)),
        )
        .unwrap();
        cx.activate(true);
    });
}

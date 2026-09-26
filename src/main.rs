//! Sonos.app — macOS Sonos controller (GPUI UI layer).
//!
//! The protocol lives in the `sonos` lib crate, GPUI-free; this binary only
//! renders state and issues control commands (see docs/decisions.md).

use std::collections::{HashMap, HashSet};
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{
    App, Application, Asset, AsyncApp, Bounds, Context, DragMoveEvent, FontWeight, ImageCacheError,
    MouseButton, Render, RenderImage, Task, TitlebarOptions, WeakEntity, Window, WindowBounds,
    WindowOptions, div, img, prelude::*, px, rgb, size,
};
use smallvec::SmallVec;
use sonos::{GroupView, SystemState, control, discover, snapshot};

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const DISCOVER_TIMEOUT: Duration = Duration::from_secs(2);
const SLIDER_WIDTH: f32 = 160.;
const VOLUME_SEND_INTERVAL: Duration = Duration::from_millis(120);
/// Press-and-hold this long on a volume slider to split the card into
/// per-speaker sliders (or merge back), like the first-party app.
const HOLD_TO_SPLIT: Duration = Duration::from_millis(450);

fn accent() -> gpui::Rgba {
    rgb(0xe8a13d)
}

fn dim() -> gpui::Rgba {
    rgb(0x8b8b96)
}

/// Which volume a slider drives.
#[derive(Clone, Copy, PartialEq)]
enum SliderTarget {
    Group(usize),
    Room { group: usize, room: usize },
}

#[derive(Clone, Copy)]
struct SliderDrag {
    target: SliderTarget,
}

/// A pending press-and-hold on a slider. Dropping the Task cancels the
/// timer; the hold only applies if the timer fires while still pending.
struct PendingHold {
    target: SliderTarget,
    /// Never read on purpose: dropping it cancels the hold timer.
    _task: Task<()>,
}

/// What volume a desired-map entry drives.
#[derive(Hash, PartialEq, Eq, Clone, Copy)]
enum VolumeTarget {
    Group(Ipv4Addr),
    Room(Ipv4Addr),
}

/// GPUI's texture pipeline expects BGRA; the image crate decodes RGBA.
/// Swap red and blue per pixel — the same conversion gpui's own asset
/// loader performs on every decode path.
fn to_bgra(image: &mut image::RgbaImage) {
    for pixel in image.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
}

/// Album art loader: fetch the upnp:albumArtURI over HTTP(S) off the UI
/// thread, decode, and hand back a RenderImage. GPUI caches per source URL,
/// so each track's art loads once.
struct AlbumArtAsset;

impl Asset for AlbumArtAsset {
    type Source = String;
    type Output = Result<Arc<RenderImage>, ImageCacheError>;

    fn load(
        url: Self::Source,
        cx: &mut App,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        cx.background_executor().spawn(async move {
            let mut response = ureq::get(&url)
                .config()
                .timeout_global(Some(Duration::from_secs(8)))
                .build()
                .call()
                .map_err(|e| ImageCacheError::from(anyhow::anyhow!("art fetch: {e}")))?;
            let bytes = response
                .body_mut()
                .read_to_vec()
                .map_err(|e| ImageCacheError::from(anyhow::anyhow!("art read: {e}")))?;
            if bytes.len() > 8 * 1024 * 1024 {
                return Err(ImageCacheError::from(anyhow::anyhow!("art too large")));
            }
            let mut rgba = image::load_from_memory(&bytes)?.into_rgba8();
            to_bgra(&mut rgba);
            let art = RenderImage::new(SmallVec::from_elem(image::Frame::new(rgba), 1));
            Ok(Arc::new(art))
        })
    }
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
    /// Group keys (coordinator uuids) whose cards show per-speaker sliders.
    split: HashSet<String>,
    pending_hold: Option<PendingHold>,
    /// Fire-and-forget SOAP tasks. Dropping a pending Task cancels it, so
    /// they are parked here; old ones have long completed by the time this
    /// fills up.
    tasks: Vec<Task<()>>,
    /// Latest desired volume per target. Drag events arrive far faster
    /// than SOAP calls complete; the sender loop coalesces to the newest
    /// value per target.
    desired: Arc<Mutex<HashMap<VolumeTarget, u8>>>,
    _poll: Option<Task<()>>,
    _sender: Option<Task<()>>,
}

impl SonosApp {
    fn new(cx: &mut Context<Self>) -> Self {
        let desired: Arc<Mutex<HashMap<VolumeTarget, u8>>> = Arc::new(Mutex::new(HashMap::new()));
        let sender_desired = desired.clone();
        let sender = cx.spawn(async move |_this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let mut last_sent: HashMap<VolumeTarget, u8> = HashMap::new();
            loop {
                cx.background_executor().timer(VOLUME_SEND_INTERVAL).await;
                let mut pending: Vec<(VolumeTarget, u8)> = Vec::new();
                {
                    let desired = sender_desired.lock().unwrap();
                    for (target, volume) in desired.iter() {
                        if last_sent.get(target) != Some(volume) {
                            pending.push((*target, *volume));
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
                        for (target, volume) in &pending {
                            let sent = match target {
                                VolumeTarget::Group(ip) => {
                                    control::set_group_volume(*ip, *volume).is_ok()
                                }
                                VolumeTarget::Room(ip) => {
                                    control::set_room_volume(*ip, *volume).is_ok()
                                }
                            };
                            results.push((*target, *volume, sent));
                        }
                        results
                    })
                    .await;
                for (target, volume, sent) in results {
                    if sent {
                        last_sent.insert(target, volume);
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
            split: HashSet::new(),
            pending_hold: None,
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

    fn coordinator_key(&self, group: usize) -> Option<String> {
        self.state
            .as_ref()?
            .groups
            .get(group)
            .map(|view| view.group.coordinator_uuid.clone())
    }

    fn set_volume(&mut self, group: usize, volume: u8, cx: &mut Context<Self>) {
        if let Some(view) = self.state.as_mut().and_then(|s| s.groups.get_mut(group)) {
            view.volume = Some(volume);
        }
        if let Some(ip) = self.coordinator_ip(group) {
            self.desired
                .lock()
                .unwrap()
                .insert(VolumeTarget::Group(ip), volume);
        }
        cx.notify();
    }

    fn set_room_volume(&mut self, group: usize, room: usize, volume: u8, cx: &mut Context<Self>) {
        let Some((uuid, ip)) = self
            .state
            .as_ref()
            .and_then(|s| s.groups.get(group))
            .and_then(|view| view.group.visible_rooms().nth(room))
            .map(|room| (room.uuid.clone(), room.ip))
        else {
            return;
        };
        if let Some(view) = self.state.as_mut().and_then(|s| s.groups.get_mut(group)) {
            view.room_volumes.insert(uuid, volume);
        }
        self.desired
            .lock()
            .unwrap()
            .insert(VolumeTarget::Room(ip), volume);
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

    /// Eager: fold the leave into local state immediately; the poll corrects.
    fn leave(&mut self, room_ip: Ipv4Addr, cx: &mut Context<Self>) {
        if let Some(state) = self.state.take() {
            self.state = Some(state.left(room_ip));
        }
        cx.notify();
        self.fire(cx, move || {
            let _ = control::leave(room_ip);
        });
    }

    /// Eager: fold the join into local state immediately; the poll corrects.
    fn join(&mut self, room_ip: Ipv4Addr, coordinator_uuid: String, cx: &mut Context<Self>) {
        self.add_menu = None;
        if let Some(state) = self.state.take() {
            self.state = Some(state.joined(room_ip, &coordinator_uuid));
        }
        cx.notify();
        self.fire(cx, move || {
            let _ = control::join(room_ip, &coordinator_uuid);
        });
    }

    /// Press-and-hold on a slider: after HOLD_TO_SPLIT without a drag or a
    /// release, split (group slider) or merge (room slider) that card.
    fn begin_hold(&mut self, target: SliderTarget, cx: &mut Context<Self>) {
        let task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(HOLD_TO_SPLIT).await;
            this.update(cx, |app, cx| {
                let still_holding = app
                    .pending_hold
                    .as_ref()
                    .is_some_and(|hold| hold.target == target);
                if still_holding {
                    app.pending_hold = None;
                    app.toggle_split(target);
                    cx.notify();
                }
            })
            .ok();
        });
        self.pending_hold = Some(PendingHold {
            target,
            _task: task,
        });
    }

    fn cancel_hold(&mut self) {
        self.pending_hold = None;
    }

    fn toggle_split(&mut self, target: SliderTarget) {
        let SliderTarget::Group(group) = target else {
            return;
        };
        if let Some(key) = self.coordinator_key(group) {
            if self.split.contains(&key) {
                self.split.remove(&key);
            } else {
                self.split.insert(key);
            }
        }
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

    fn volume_slider(
        &self,
        target: SliderTarget,
        volume: u8,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let slider = match target {
            SliderTarget::Group(group) => div().id(("slider", group)),
            SliderTarget::Room { group, room } => div().id(("rslider", group * 16 + room)),
        };
        slider
            .flex()
            .items_center()
            .w(px(SLIDER_WIDTH))
            .h_6()
            .cursor_pointer()
            .on_drag(SliderDrag { target }, |_: &SliderDrag, _, _, cx| {
                cx.new(|_| SliderGhost {})
            })
            .on_drag_move::<SliderDrag>(cx.listener(
                move |this, ev: &DragMoveEvent<SliderDrag>, _, cx| {
                    // During a drag, every on_drag_move listener of this
                    // payload type fires; only the originating slider acts.
                    if ev.drag(cx).target != target {
                        return;
                    }
                    // A drag cancels any pending press-and-hold.
                    this.cancel_hold();
                    let fraction = (ev.event.position.x - ev.bounds.left()) / ev.bounds.size.width;
                    let volume = (fraction.clamp(0., 1.) * 100.).round() as u8;
                    match target {
                        SliderTarget::Group(group) => this.set_volume(group, volume, cx),
                        SliderTarget::Room { group, room } => {
                            this.set_room_volume(group, room, volume, cx);
                        }
                    }
                },
            ))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &gpui::MouseDownEvent, _, cx| {
                    this.begin_hold(target, cx);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _: &gpui::MouseUpEvent, _, _| {
                    this.cancel_hold();
                }),
            )
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

        let album_art_uri = view
            .now_playing
            .as_ref()
            .and_then(|np| np.album_art_uri.clone());
        let coordinator_uuid = group.coordinator_uuid.clone();
        let multi_room = group.rooms.len() > 1;
        // Display signal for real multi-room groups (a bonded stereo pair is
        // one visible room, not a group).
        let grouped = group.visible_rooms().count() > 1;
        let split = self.split.contains(&coordinator_uuid);

        // Join sources are other groups' standalone rooms: exactly one
        // visible room (a bonded pair counts as one standalone room; its
        // invisible twin does not disqualify it). Rooms already in a
        // multi-room group leave via their chip's x, then join elsewhere.
        let joinable: Vec<(String, Ipv4Addr)> = state
            .groups
            .iter()
            .enumerate()
            .filter(|(i, g)| *i != index && g.group.visible_rooms().count() == 1)
            .filter_map(|(_, g)| {
                g.group
                    .visible_rooms()
                    .next()
                    .map(|room| (room.name.clone(), room.ip))
            })
            .collect();

        // Volume area: the group slider with steppers, or per-speaker
        // sliders after a press-and-hold split.
        let mut volume_rows: Vec<gpui::AnyElement> = Vec::new();
        if split {
            for (room_ix, room) in group.visible_rooms().enumerate() {
                let room_volume = view.room_volumes.get(&room.uuid).copied().unwrap_or(0);
                volume_rows.push(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .text_color(dim())
                                .w(px(90.))
                                .flex_none()
                                .truncate()
                                .child(room.name.clone()),
                        )
                        .child(self.volume_slider(
                            SliderTarget::Room {
                                group: index,
                                room: room_ix,
                            },
                            room_volume,
                            cx,
                        ))
                        .into_any_element(),
                );
            }
        } else {
            volume_rows.push(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(self.step_button(index, "−", -2, cx))
                    .child(self.volume_slider(SliderTarget::Group(index), volume, cx))
                    .child(self.step_button(index, "+", 2, cx))
                    .into_any_element(),
            );
        }

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
                        div().flex().items_center().gap_2().child(
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
                                .on_click(cx.listener(move |this, _: &gpui::ClickEvent, _, cx| {
                                    this.add_menu = if this.add_menu == Some(index) {
                                        None
                                    } else {
                                        Some(index)
                                    };
                                    cx.notify();
                                })),
                        ),
                    ),
            )
            .child(match album_art_uri {
                Some(url) => div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        img(move |window: &mut Window, cx: &mut App| {
                            window.use_asset::<AlbumArtAsset>(&url, cx)
                        })
                        .flex_none()
                        .size(px(56.))
                        .rounded_md()
                        .with_loading(|| {
                            div()
                                .size(px(56.))
                                .flex_none()
                                .rounded_md()
                                .bg(rgb(0x33333c))
                                .into_any_element()
                        })
                        .with_fallback(|| {
                            div()
                                .size(px(56.))
                                .flex_none()
                                .rounded_md()
                                .bg(rgb(0x33333c))
                                .into_any_element()
                        }),
                    )
                    .child(
                        div()
                            .text_color(if playing { rgb(0xd8d8de) } else { dim() })
                            .child(now_playing),
                    )
                    .into_any_element(),
                None => div()
                    .text_color(if playing { rgb(0xd8d8de) } else { dim() })
                    .child(now_playing)
                    .into_any_element(),
            })
            .child(div().flex().flex_col().gap_1().children(volume_rows))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn art_pixels_swap_red_blue_to_bgra() {
        let mut rgba = image::RgbaImage::from_raw(1, 1, vec![255, 0, 0, 255]).unwrap();
        to_bgra(&mut rgba);
        assert_eq!(rgba.get_pixel(0, 0).0, [0, 0, 255, 255]);
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

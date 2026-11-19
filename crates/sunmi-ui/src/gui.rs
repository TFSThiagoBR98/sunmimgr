//! GPUI desktop app.
//!
//! Threading model: a tokio runtime lives on its own thread and drives the
//! `Downloader` (tokio::process). A runtime-agnostic `async_channel` bridges
//! tokio tasks to GPUI tasks; a `Context::spawn` loop consumes events and
//! updates the entity reactively.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use gpui::{
    div, prelude::*, px, rgb, size, App, AnyElement, Bounds, ClickEvent, Context, Div,
    FontWeight, IntoElement, PathPromptOptions, ScrollHandle, SharedString, Stateful, Styled,
    Window, WindowBounds, WindowOptions,
};
use sunmi_adb::{Adb, Device, DeviceState};
use sunmi_core::download::{
    validate, DownloadEvent, DownloadItem, DownloadOptions, Downloader,
};
use sunmi_core::filetype::FileKind;
use tokio_util::sync::CancellationToken;

const GREEN: u32 = 0x16a34a;
const GRAY_DARK: u32 = 0x1c1917;
const GRAY: u32 = 0x78716c;
const GRAY_LIGHT: u32 = 0xf5f5f4;
const RED: u32 = 0xdc2626;

const LOG_LINES: usize = 400;
const LOG_VISIBLE: usize = 14;

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

fn tokio_rt() -> &'static tokio::runtime::Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("failed to build tokio runtime")
    })
}

/// One file in the install queue.
#[derive(Clone)]
struct QueueItem {
    path: PathBuf,
    kind: FileKind,
    selected: bool,
}

/// Events flowing from tokio to the GPUI entity.
enum UiEvent {
    Devices(Vec<Device>),
    Log(String),
    DownloadEvent(DownloadEvent),
    Busy(bool),
}

struct SunmiApp {
    devices: Vec<Device>,
    queue: Vec<QueueItem>,
    log: VecDeque<String>,
    log_scroll: ScrollHandle,
    status: String,
    busy: bool,
    stop_token: Option<CancellationToken>,
    events_tx: async_channel::Sender<UiEvent>,
}

impl SunmiApp {
    fn new(cx: &mut Context<Self>) -> Self {
        let (events_tx, events_rx) = async_channel::unbounded::<UiEvent>();

        // Device discovery loop (tokio): polls `adb devices` every 2 s and
        // pushes results to the UI bridge.
        let poll_tx = events_tx.clone();
        tokio_rt().spawn(async move {
            let Some(adb) = Adb::discover() else {
                let _ = poll_tx
                    .send(UiEvent::Log(
                        "'adb' não encontrado no PATH (instale o platform-tools)".to_string(),
                    ))
                    .await;
                return;
            };
            loop {
                match adb.devices().await {
                    Ok(devices) => {
                        let _ = poll_tx.send(UiEvent::Devices(devices)).await;
                    }
                    Err(e) => {
                        let _ = poll_tx
                            .send(UiEvent::Log(format!("adb devices: {e}")))
                            .await;
                    }
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });

        // UI bridge loop (GPUI): consumes UiEvents and updates the entity.
        cx.spawn(async move |this, cx| {
            loop {
                match events_rx.recv().await {
                    Ok(event) => {
                        let _ = this.update(cx, |app, cx| {
                            app.on_ui_event(event);
                            cx.notify();
                        });
                    }
                    Err(_) => break,
                }
            }
        })
        .detach();

        Self {
            devices: Vec::new(),
            queue: Vec::new(),
            log: VecDeque::new(),
            log_scroll: ScrollHandle::new(),
            status: "Aguardando dispositivos...".to_string(),
            busy: false,
            stop_token: None,
            events_tx,
        }
    }

    fn push_log(&mut self, line: impl Into<String>) {
        self.log.push_back(line.into());
        while self.log.len() > LOG_LINES {
            self.log.pop_front();
        }
        // Tail-follow: keep the newest lines visible as they arrive.
        self.log_scroll.scroll_to_bottom();
    }

    fn on_ui_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::Devices(devices) => {
                let n = devices.len();
                self.devices = devices;
                if !self.busy {
                    self.status = match n {
                        0 => "Nenhum dispositivo conectado".to_string(),
                        n => format!("{n} dispositivo(s) conectado(s)"),
                    };
                }
            }
            UiEvent::Log(line) => self.push_log(line),
            UiEvent::Busy(busy) => self.busy = busy,
            UiEvent::DownloadEvent(event) => match event {
                DownloadEvent::ItemStarted { serial, item } => {
                    self.status = format!(
                        "[{serial}] instalando {} {:?}...",
                        item.kind,
                        item.path.file_name().unwrap_or_default()
                    );
                    self.push_log(self.status.clone());
                }
                DownloadEvent::ItemCompleted { serial, item } => {
                    self.push_log(format!(
                        "[{serial}] ✅ {} {:?}",
                        item.kind,
                        item.path.file_name().unwrap_or_default()
                    ));
                }
                DownloadEvent::ItemFailed { serial, item, error } => {
                    self.push_log(format!(
                        "[{serial}] ❌ {} {:?}: {error}",
                        item.kind,
                        item.path.file_name().unwrap_or_default()
                    ));
                }
                DownloadEvent::DeviceFailed { serial, error } => {
                    self.push_log(format!("[{serial}] dispositivo falhou: {error}"));
                }
                DownloadEvent::DeviceFinished { serial, ok } => {
                    self.push_log(format!(
                        "[{serial}] concluído ({})",
                        if ok { "ok" } else { "falhou" }
                    ));
                }
                DownloadEvent::Finished => {
                    self.push_log("Todos os dispositivos terminaram.".to_string());
                }
            },
        }
    }

    fn add_path(&mut self, path: PathBuf) {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let Some(kind) = FileKind::detect(name) else {
            self.push_log(format!("⚠ Tipo desconhecido, adição cancelada: {name}"));
            return;
        };
        if !self.queue.iter().any(|item| item.path == path) {
            self.queue.push(QueueItem {
                path,
                kind,
                selected: true,
            });
        }
    }

    fn selected_items(&self) -> Vec<DownloadItem> {
        self.queue
            .iter()
            .filter(|item| item.selected)
            .map(|item| DownloadItem::new(item.kind, item.path.clone()))
            .collect()
    }

    fn start_install(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            self.push_log("⚠ Já existe um download em andamento.");
            cx.notify();
            return;
        }
        let items = self.selected_items();
        if items.is_empty() {
            self.push_log("⚠ Nenhum arquivo selecionado.");
            cx.notify();
            return;
        }
        if let Err(e) = validate(&items) {
            self.push_log(format!("⚠ Fila inválida: {e}"));
            cx.notify();
            return;
        }

        let serials: Vec<String> = self
            .devices
            .iter()
            .filter(|d| d.state == DeviceState::Device)
            .map(|d| d.serial.clone())
            .collect();
        if serials.is_empty() {
            self.push_log("⚠ Nenhum dispositivo em estado 'device'.");
            cx.notify();
            return;
        }

        let stop = CancellationToken::new();
        self.stop_token = Some(stop.clone());
        self.busy = true;
        self.push_log(format!(
            "▶ Instalando {} arquivo(s) em {} dispositivo(s)...",
            items.len(),
            serials.len()
        ));
        cx.notify();

        let events_tx = self.events_tx.clone();
        tokio_rt().spawn(async move {
            let _ = events_tx.send(UiEvent::Busy(true)).await;
            let Some(downloader) = Adb::discover().map(Downloader::new) else {
                let _ = events_tx.send(UiEvent::Log("'adb' não encontrado".into())).await;
                let _ = events_tx.send(UiEvent::Busy(false)).await;
                return;
            };
            // downloader.run() spawns tokio tasks internally, so the whole
            // flow must run inside the runtime context.
            let run_tx = events_tx.clone();
            tokio_rt()
                .spawn(async move {
                    let mut rx = downloader.run(
                        serials,
                        Arc::new(items),
                        DownloadOptions {
                            stop,
                            ..Default::default()
                        },
                    );
                    while let Some(event) = rx.recv().await {
                        if run_tx.send(UiEvent::DownloadEvent(event)).await.is_err() {
                            break;
                        }
                    }
                })
                .await
                .ok();
            let _ = events_tx.send(UiEvent::Busy(false)).await;
        });
    }

    fn stop_install(&mut self, cx: &mut Context<Self>) {
        if let Some(token) = &self.stop_token {
            token.cancel();
            self.push_log("⏹ Parando download...");
        }
        cx.notify();
    }

    fn add_files_dialog(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Selecione APK / zip (AppCrp) / APN".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                let _ = this.update(cx, |app, cx| {
                    for path in paths {
                        app.add_path(path);
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn show_device_info(&mut self, serial: &str, cx: &mut Context<Self>) {
        let serial = serial.to_string();
        let events_tx = self.events_tx.clone();
        tokio_rt().spawn(async move {
            let Some(downloader) = Adb::discover().map(Downloader::new) else {
                let _ = events_tx.send(UiEvent::Log("'adb' não encontrado".into())).await;
                return;
            };
            let _ = events_tx
                .send(UiEvent::Log(format!("[{serial}] sincronizando relógio...")))
                .await;
            match downloader.sync_time(&serial).await {
                Ok(_) => {
                    let _ = events_tx
                        .send(UiEvent::Log(format!("[{serial}] relógio sincronizado")))
                        .await;
                }
                Err(e) => {
                    let _ = events_tx
                        .send(UiEvent::Log(format!("[{serial}] relógio falhou: {e}")))
                        .await;
                }
            }
            match downloader.term_info(&serial).await {
                Ok(info) => {
                    for (label, value) in info.display_rows() {
                        let _ = events_tx
                            .send(UiEvent::Log(format!("[{serial}] {label}: {value}")))
                            .await;
                    }
                    if let Some(battery) = info.battery_percent() {
                        let _ = events_tx
                            .send(UiEvent::Log(format!("[{serial}] Battery: {battery}%")))
                            .await;
                    }
                    if let Some(free_mb) = info.data_free_space_mb() {
                        let _ = events_tx
                            .send(UiEvent::Log(format!(
                                "[{serial}] Data free space: {free_mb} MB"
                            )))
                            .await;
                    }
                }
                Err(e) => {
                    let _ = events_tx
                        .send(UiEvent::Log(format!("[{serial}] get posinfo falhou: {e}")))
                        .await;
                }
            }
        });
        cx.notify();
    }
}

impl Render for SunmiApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy;

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(GRAY_LIGHT))
            .text_color(rgb(GRAY_DARK))
            // CSD titlebar with window controls (client-side decorations)
            .child(titlebar(cx))
            // content
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .p_4()
                    .gap_2()
            // header: title + status
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::BOLD)
                            .child("sunmimgr"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(GRAY))
                            .child(self.status.clone()),
                    ),
            )
            // device list
            .child(section_title("Dispositivos"))
            .child(if self.devices.is_empty() {
                empty_row("nenhum dispositivo detectado (poll a cada 2s)")
            } else {
                device_list(self.devices.clone(), cx)
            })
            // queue
            .child(section_title("Fila de arquivos"))
            .child(if self.queue.is_empty() {
                empty_row("nenhum arquivo na fila")
            } else {
                queue_list(self.queue.clone(), cx)
            })
            // action bar
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(button(
                        "add-btn",
                        "+ Arquivo",
                        GRAY_DARK,
                        cx.listener(|this, _ev, _window, cx| this.add_files_dialog(cx)),
                    ))
                    .child(button(
                        "install-btn",
                        if busy { "Instalando..." } else { "Instalar" },
                        GREEN,
                        cx.listener(|this, _ev, _window, cx| this.start_install(cx)),
                    ))
                    .child(button(
                        "stop-btn",
                        "Parar",
                        RED,
                        cx.listener(|this, _ev, _window, cx| this.stop_install(cx)),
                    )),
            )
            // log
            .child(section_title("Log"))
            .child(log_view(&self.log_scroll, &self.log)),
        )
    }
}

// ---------------------------------------------------------------------------
// render helpers
// ---------------------------------------------------------------------------

/// Client-side titlebar: drag area (starts window move) + minimize,
/// maximize and close buttons. Works the same on Wayland and later macOS
/// (where gpui ignores decorations and uses its own controls).
fn titlebar(cx: &mut Context<SunmiApp>) -> AnyElement {
    div()
        .id("titlebar")
        .flex()
        .items_center()
        .justify_between()
        .h(px(36.))
        .pl_3()
        .bg(rgb(GRAY_DARK))
        .text_color(rgb(0xffffff))
        .child(
            // drag area: only the title strip, not the buttons
            div()
                .id("title-drag")
                .flex_1()
                .h_full()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|_this, _ev, window, _cx| window.start_window_move()),
                )
                .child("sunmimgr — Sunmi terminal manager"),
        )
        .child(
            div()
                .flex()
                .h_full()
                .child(
                    div()
                        .id("min-btn")
                        .w(px(44.))
                        .h_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_sm()
                        .cursor_pointer()
                        .hover(|style| style.bg(rgb(0x44403c)))
                        .on_click(cx.listener(|_this, _ev, window, _cx| window.minimize_window()))
                        .child("–"),
                )
                .child(
                    div()
                        .id("close-btn")
                        .w(px(44.))
                        .h_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_sm()
                        .cursor_pointer()
                        .hover(|style| style.bg(rgb(0xb91c1c)))
                        .on_click(cx.listener(|_this, _ev, window, _cx| window.remove_window()))
                        .child("✕"),
                ),
        )
        .into_any_element()
}

fn section_title(text: &'static str) -> AnyElement {
    div()
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(GRAY))
        .child(text)
        .into_any_element()
}

fn empty_row(text: &str) -> AnyElement {
    div()
        .text_sm()
        .text_color(rgb(GRAY))
        .child(text.to_string())
        .into_any_element()
}

fn device_list(devices: Vec<Device>, cx: &mut Context<SunmiApp>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .children(devices.into_iter().map(|device| {
            let serial = device.serial.clone();
            let state_label = match &device.state {
                DeviceState::Device => "device".to_string(),
                DeviceState::Offline => "offline".to_string(),
                DeviceState::Unauthorized => "não autorizado".to_string(),
                DeviceState::Recovery => "recovery".to_string(),
                DeviceState::Bootloader => "bootloader".to_string(),
                DeviceState::Other(other) => other.clone(),
            };
            let state_color =
                if device.state == DeviceState::Device { GREEN } else { GRAY };
            let info_serial = serial.clone();

            div()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .bg(rgb(0xffffff))
                .rounded_md()
                .child(div().text_sm().child(serial.clone()))
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(state_color))
                        .child(state_label),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("info-{serial}")))
                        .text_xs()
                        .px_2()
                        .py_0p5()
                        .rounded_md()
                        .bg(rgb(GRAY_LIGHT))
                        .text_color(rgb(GRAY_DARK))
                        .cursor_pointer()
                        .hover(|style| style.text_color(rgb(GREEN)))
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            this.show_device_info(&info_serial, cx);
                        }))
                        .child("info"),
                )
        }))
        .into_any_element()
}

fn queue_list(items: Vec<QueueItem>, cx: &mut Context<SunmiApp>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .children(items.into_iter().enumerate().map(|(i, item)| {
            let name = item
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?")
                .to_string();
            let check = if item.selected { "☑" } else { "☐" };
            let kind_label = item.kind.to_string();

            div()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .bg(rgb(0xffffff))
                .rounded_md()
                .child(
                    div()
                        .id(SharedString::from(format!("chk-{i}")))
                        .text_sm()
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            if let Some(item) = this.queue.get_mut(i) {
                                item.selected = !item.selected;
                            }
                            cx.notify();
                        }))
                        .child(check),
                )
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .min_w_0()
                        .truncate()
                        .child(name),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("kind-{i}")))
                        .text_xs()
                        .px_2()
                        .py_0p5()
                        .rounded_md()
                        .bg(rgb(0xdbeafe))
                        .text_color(rgb(0x1d4ed8))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            if let Some(item) = this.queue.get_mut(i) {
                                let all = FileKind::ALL;
                                let next = all
                                    .iter()
                                    .position(|k| *k == item.kind)
                                    .map(|p| (p + 1) % all.len())
                                    .unwrap_or(0);
                                item.kind = all[next];
                            }
                            cx.notify();
                        }))
                        .child(kind_label),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("rm-{i}")))
                        .text_sm()
                        .text_color(rgb(GRAY))
                        .cursor_pointer()
                        .hover(|style| style.text_color(rgb(RED)))
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            this.queue.remove(i);
                            cx.notify();
                        }))
                        .child("✕"),
                )
        }))
        .into_any_element()
}

fn button(
    id: &'static str,
    label: &'static str,
    color: u32,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .px_4()
        .py_2()
        .rounded_md()
        .bg(rgb(color))
        .text_color(rgb(0xffffff))
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .hover(|style| style.opacity(0.9))
        .on_click(on_click)
        .child(label)
}

fn log_view(scroll: &ScrollHandle, log: &VecDeque<String>) -> AnyElement {
    div()
        .id("log-scroll")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .track_scroll(scroll)
        .flex()
        .flex_col()
        .gap_0p5()
        .bg(rgb(0x0c0a09))
        .text_color(rgb(0xa8a29e))
        .rounded_md()
        .p_2()
        .children(log.iter().map(|line| div().text_xs().child(line.clone())))
        .into_any_element()
}

/// Entry point: opens the main window and runs the app loop.
pub fn run() {
    gpui::Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(900.), px(620.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("sunmimgr — Sunmi terminal manager".into()),
                ..Default::default()
            }),
            // Client-side decorations: the app draws its own titlebar with
            // minimize/maximize/close (GNOME mutter doesn't reliably provide
            // server-side decorations). Ignored on macOS.
            window_decorations: Some(gpui::WindowDecorations::Client),
            ..Default::default()
        };
        cx.open_window(options, |_window, cx| cx.new(SunmiApp::new))
            .expect("failed to open window");
        cx.activate(true);
    });
}

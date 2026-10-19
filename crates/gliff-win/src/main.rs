//! A portable gliff client: a winit window, a softbuffer blit, and the CPU
//! (OpenH264) decode tier. Built for Windows, where the GTK client's
//! VA-API, dmabuf and Wayland paths do not exist; it runs on Linux too.
//!
//! It stays a console program on purpose: ssh asks for host keys and
//! passwords on the console, and the server's log lands there.

#[cfg(windows)]
mod capture;
mod clip;
mod keys;
mod net;
#[cfg(windows)]
mod probe;
mod setup;
mod view;

use std::collections::HashSet;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clap::Parser;
use gliff_proto::{Axis, ClientMsg};
use gliff_transport::SshTarget;
use tokio::sync::mpsc::UnboundedSender;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::PhysicalKey;
use winit::window::{CustomCursor, Fullscreen, Window, WindowId};

use net::{Endpoint, Event, Picture, Slot};

const APP_NAME: &str = "gliff";

/// A window size must hold this long before the server is asked to match
/// it, so a drag-resize does not resize the remote output on every step.
const RESIZE_SETTLE: Duration = Duration::from_millis(200);

#[derive(Parser, Debug)]
#[command(
    name = "gliff-win",
    version,
    about = "Remote-desktop a Hyprland session over ssh"
)]
struct Cli {
    /// `user@host` (or an ssh config alias) to ssh to and spawn gliff-server.
    /// Without it, the machine connected to last time.
    host: Option<String>,
    /// Install for this user: copy gliff-win to
    /// %LOCALAPPDATA%\Programs\gliff, add it to PATH, and add a Start menu
    /// shortcut (Windows only).
    #[arg(long, conflicts_with_all = ["host", "connect"])]
    install: bool,
    /// Dev: connect directly to a `gliff-server --listen` address.
    #[arg(long, conflicts_with = "host")]
    connect: Option<String>,
    /// Remote gliff-server path. Remembered for later runs; without it, the
    /// one given last time, else `gliff-server`.
    #[arg(long)]
    server_bin: Option<String>,
    /// Create a private headless output on the remote, sized and scaled to
    /// this window, instead of mirroring the remote's focused screen.
    #[arg(long, conflicts_with = "output")]
    headless: bool,
    /// Mirror the named remote output (e.g. `DP-1`) instead of the focused one.
    #[arg(long)]
    output: Option<String>,
    /// Disable clipboard sharing in both directions.
    #[arg(long)]
    no_clipboard: bool,
    /// Do not play the remote machine's sound.
    #[arg(long)]
    no_audio: bool,
    /// Colour detail: `444` for sharp text (two streams to decode) or
    /// `420` for half the decode work.
    #[arg(long, default_value = "444", value_parser = ["444", "420"])]
    chroma: String,
    /// Start full screen. Ctrl+Alt+Enter toggles it at any time.
    #[arg(long)]
    fullscreen: bool,
    /// Test: open the window without a server, type A and B into it, and
    /// report where each key arrived (Windows only).
    #[arg(long, hide = true)]
    probe_keys: bool,
}

/// How to drive the window from the keyboard, printed once connected.
const KEY_HELP: &str = "While this window is in front, every key goes to the remote, \
Win and Alt+Tab included (Windows keeps only Ctrl+Alt+Del and Win+L).\n\
  Ctrl+Alt+Enter  toggle full screen\n\
  Shift+Esc       give the keyboard back to Windows; click the picture to take it again";

impl Cli {
    fn endpoint(&self) -> Endpoint {
        if let Some(addr) = &self.connect {
            return Endpoint::Tcp(addr.clone());
        }
        let mut t = SshTarget::new(self.host.clone().unwrap_or_default());
        if let Some(bin) = &self.server_bin {
            t.server_bin = bin.clone();
        }
        // Notice a dead link (a laptop waking from sleep) within about 15 s,
        // so the session can reconnect instead of hanging.
        t.ssh_args = [
            "-o",
            "ServerAliveInterval=5",
            "-o",
            "ServerAliveCountMax=3",
            "-o",
            "ConnectTimeout=15",
        ]
        .map(String::from)
        .to_vec();
        t.server_args = match (&self.output, self.headless) {
            (Some(name), _) => vec!["--output".into(), name.clone()],
            (None, true) => vec!["--headless".into()],
            (None, false) => vec!["--output".into(), "auto".into()],
        };
        Endpoint::Ssh(t)
    }

    fn machine(&self) -> &str {
        self.connect
            .as_deref()
            .or(self.host.as_deref())
            .unwrap_or_default()
    }
}

struct Gfx {
    window: Rc<Window>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
}

struct App {
    cli: Cli,
    proxy: EventLoopProxy<Event>,
    slot: Slot,
    gfx: Option<Gfx>,
    input: Option<UnboundedSender<ClientMsg>>,
    /// The picture on screen, kept to redraw on expose and resize.
    picture: Option<Picture>,
    /// Where `picture` was last drawn, for pointer mapping.
    drawn: Option<view::Rect>,
    headless: bool,
    /// evdev codes of keys pressed on the remote, released on focus loss.
    pressed: HashSet<u32>,
    /// When the window size last changed, until the resize is sent.
    resize_due: Option<Instant>,
    /// The size and scale last sent, so an unchanged window is not resent.
    resize_sent: Option<((u32, u32), f64)>,
    video: String,
    exit_error: Option<String>,
    /// The keyboard was handed back with the release hotkey; a click on the
    /// picture takes it again.
    released: bool,
    /// A session has connected before, so the next one is a reconnection.
    connected_before: bool,
    /// A key reached the window past the hook; noted once on the console.
    #[cfg(windows)]
    stray_key_noted: bool,
    /// The Windows keyboard hook, which takes keys (Win and Alt+Tab
    /// included) while our window is active; keys it lets through still
    /// reach us as winit key events.
    #[cfg(windows)]
    hook: Option<capture::Hook>,
}

impl App {
    fn new(cli: Cli, proxy: EventLoopProxy<Event>) -> Self {
        Self {
            cli,
            proxy,
            slot: Arc::new(Mutex::new(None)),
            gfx: None,
            input: None,
            picture: None,
            drawn: None,
            headless: false,
            pressed: HashSet::new(),
            resize_due: None,
            resize_sent: None,
            video: String::new(),
            exit_error: None,
            released: false,
            connected_before: false,
            #[cfg(windows)]
            stray_key_noted: false,
            #[cfg(windows)]
            hook: None,
        }
    }

    #[cfg(windows)]
    fn hooked(&self) -> bool {
        self.hook.is_some()
    }

    /// A key from the hook or from winit, as an evdev code. Handles the two
    /// local hotkeys; everything else goes to the remote.
    fn on_key(&mut self, keycode: u32, pressed: bool, source: &str) {
        if self.cli.probe_keys {
            eprintln!("probe: key evdev {keycode} pressed {pressed} from {source}");
        }
        if self.released {
            return;
        }
        if pressed {
            let held = |a, b| self.pressed.contains(&a) || self.pressed.contains(&b);
            let ctrl = held(keys::KEY_LEFTCTRL, keys::KEY_RIGHTCTRL);
            let alt = held(keys::KEY_LEFTALT, keys::KEY_RIGHTALT);
            let shift = held(keys::KEY_LEFTSHIFT, keys::KEY_RIGHTSHIFT);
            if keycode == keys::KEY_ENTER && ctrl && alt {
                self.toggle_fullscreen();
                return;
            }
            if keycode == keys::KEY_ESC && shift && !ctrl && !alt {
                self.release_capture();
                return;
            }
        }
        // Never send a release whose press was not sent, nor a repeat: the
        // remote compositor repeats held keys on its own.
        let changed = if pressed {
            self.pressed.insert(keycode)
        } else {
            self.pressed.remove(&keycode)
        };
        if changed {
            self.send(ClientMsg::Key { keycode, pressed });
        }
    }

    fn toggle_fullscreen(&self) {
        if let Some(gfx) = &self.gfx {
            let full = gfx.window.fullscreen().is_some();
            gfx.window
                .set_fullscreen((!full).then_some(Fullscreen::Borderless(None)));
        }
    }

    fn release_capture(&mut self) {
        self.release_keys();
        self.released = true;
        #[cfg(windows)]
        capture::set_active(false);
        eprintln!("gliff: keyboard handed back; click the picture to capture it again");
    }

    fn recapture(&mut self) {
        if self.released {
            self.released = false;
            #[cfg(windows)]
            capture::set_active(true);
        }
    }

    fn send(&self, msg: ClientMsg) {
        if let Some(tx) = &self.input {
            let _ = tx.send(msg);
        }
    }

    fn set_title(&self, extra: &str) {
        if let Some(gfx) = &self.gfx {
            let title = if extra.is_empty() {
                format!("{APP_NAME} — {}", self.cli.machine())
            } else {
                format!("{APP_NAME} — {} — {extra}", self.cli.machine())
            };
            gfx.window.set_title(&title);
        }
    }

    /// Send a Resize for the current window when it differs from the last
    /// one sent. Sizes are physical and even, as the GTK client sends them.
    fn send_resize(&mut self) {
        self.resize_due = None;
        let Some(gfx) = &self.gfx else {
            return;
        };
        let size = gfx.window.inner_size();
        let scale = gfx.window.scale_factor();
        let size = (size.width & !1, size.height & !1);
        if size.0 < 64 || size.1 < 64 || self.resize_sent == Some((size, scale)) {
            return;
        }
        self.resize_sent = Some((size, scale));
        tracing::debug!(?size, scale, "requesting resize");
        self.send(ClientMsg::Resize {
            width: size.0,
            height: size.1,
            scale: scale as f32,
        });
    }

    /// Release every key held on the remote: once the window loses focus
    /// their releases never reach us, and Alt-Tab would leave Alt held.
    fn release_keys(&mut self) {
        for keycode in std::mem::take(&mut self.pressed) {
            self.send(ClientMsg::Key {
                keycode,
                pressed: false,
            });
        }
    }

    fn redraw(&mut self) {
        let Some(gfx) = &mut self.gfx else {
            return;
        };
        let size = gfx.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
            return;
        };
        if let Err(e) = gfx.surface.resize(w, h) {
            tracing::warn!(error = %e, "cannot resize the surface");
            return;
        }
        let mut buffer = match gfx.surface.buffer_mut() {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(error = %e, "cannot map the surface");
                return;
            }
        };
        self.drawn = None;
        match &self.picture {
            Some(p) => {
                let rect = view::fit(p.view, (size.width, size.height));
                match rect {
                    Some(rect) => {
                        view::blit(
                            &mut buffer,
                            size.width as usize,
                            &p.frame.pixels,
                            (p.frame.width, p.frame.height),
                            rect,
                        );
                        self.drawn = Some(rect);
                    }
                    None => buffer.fill(0),
                }
            }
            None => buffer.fill(0),
        }
        if let Err(e) = buffer.present() {
            tracing::warn!(error = %e, "cannot present");
        }
    }

    fn pointer_motion(&self, pos: PhysicalPosition<f64>) {
        let (Some(p), Some(rect)) = (&self.picture, self.drawn) else {
            return;
        };
        let (x, y) = view::to_remote((pos.x, pos.y), rect, p.stream, p.scale_milli);
        self.send(ClientMsg::PointerMotion { x, y });
    }

    fn scroll(&self, delta: MouseScrollDelta) {
        // winit counts away from the user (wheel up) as positive; the
        // protocol, like GTK, counts scrolling down as positive.
        let (dx, dy) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (x as f64, -y as f64),
            MouseScrollDelta::PixelDelta(p) => (p.x / 15.0, -p.y / 15.0),
        };
        for (axis, d) in [(Axis::Vertical, dy), (Axis::Horizontal, dx)] {
            if d != 0.0 {
                self.send(ClientMsg::PointerAxis {
                    axis,
                    value: d * 15.0,
                    discrete: Some(d.signum() as i32),
                    stop: false,
                });
            }
        }
    }

    fn set_cursor(
        &self,
        event_loop: &ActiveEventLoop,
        width: u32,
        height: u32,
        hot: (i32, i32),
        argb: &[u8],
    ) {
        let Some(gfx) = &self.gfx else {
            return;
        };
        match remote_cursor(width, height, hot, argb).map(|c| event_loop.create_custom_cursor(c)) {
            Some(cursor) => {
                gfx.window.set_cursor(cursor);
                gfx.window.set_cursor_visible(true);
            }
            // A headless session's frames carry the compositor-drawn
            // cursor; showing ours as well would give two pointers.
            None if self.headless => gfx.window.set_cursor_visible(false),
            None => {
                gfx.window.set_cursor(winit::window::CursorIcon::Default);
                gfx.window.set_cursor_visible(true);
            }
        }
    }
}

/// The remote cursor image (B, G, R, A bytes), or `None` when it has no
/// visible shape: fully transparent, or one flat colour as Hyprland sends
/// when it has no image to share.
fn remote_cursor(
    width: u32,
    height: u32,
    hot: (i32, i32),
    argb: &[u8],
) -> Option<winit::window::CustomCursorSource> {
    let needed = width as u64 * height as u64 * 4;
    if width == 0 || height == 0 || width > 1024 || height > 1024 || (argb.len() as u64) < needed {
        return None;
    }
    let argb = &argb[..needed as usize];
    let mut pixels = argb.chunks_exact(4);
    let first = pixels.next()?;
    let opaque = first[3] != 0 || pixels.clone().any(|p| p[3] != 0);
    if !opaque || pixels.all(|p| p == first) {
        return None;
    }
    let rgba: Vec<u8> = argb
        .chunks_exact(4)
        .flat_map(|p| [p[2], p[1], p[0], p[3]])
        .collect();
    let hot_x = hot.0.clamp(0, width as i32 - 1) as u16;
    let hot_y = hot.1.clamp(0, height as i32 - 1) as u16;
    CustomCursor::from_rgba(rgba, width as u16, height as u16, hot_x, hot_y).ok()
}

impl ApplicationHandler<Event> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title(format!("{APP_NAME} — {}", self.cli.machine()))
            .with_inner_size(LogicalSize::new(1600.0, 900.0))
            .with_fullscreen(self.cli.fullscreen.then_some(Fullscreen::Borderless(None)));
        let gfx = event_loop
            .create_window(attrs)
            .map_err(anyhow::Error::from)
            .and_then(|window| {
                let window = Rc::new(window);
                let context = softbuffer::Context::new(window.clone())
                    .map_err(|e| anyhow::anyhow!("softbuffer context: {e}"))?;
                let surface = softbuffer::Surface::new(&context, window.clone())
                    .map_err(|e| anyhow::anyhow!("softbuffer surface: {e}"))?;
                Ok(Gfx { window, surface })
            });
        match gfx {
            Ok(gfx) => {
                #[cfg(windows)]
                {
                    // A window opened from a terminal can be left without
                    // the keyboard focus, which stays with the terminal.
                    gfx.window.focus_window();
                    self.hook = install_hook(self.proxy.clone());
                    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
                    if let Ok(RawWindowHandle::Win32(w)) =
                        gfx.window.window_handle().map(|h| h.as_raw())
                    {
                        capture::set_window(w.hwnd.get());
                    }
                    capture::set_focused(gfx.window.has_focus());
                }
                self.gfx = Some(gfx);
            }
            Err(e) => {
                self.exit_error = Some(format!("cannot open a window: {e:#}"));
                event_loop.exit();
                return;
            }
        }
        #[cfg(windows)]
        if self.cli.probe_keys {
            probe::start(self.proxy.clone());
            return;
        }
        // ssh asks for host keys and passwords on the console, which this
        // window covers on first connect.
        self.set_title("connecting… (answer any ssh prompt in the terminal)");
        self.input = Some(net::spawn(
            self.cli.endpoint(),
            self.proxy.clone(),
            self.slot.clone(),
            net::Options {
                clipboard: !self.cli.no_clipboard,
                full_chroma: self.cli.chroma == "444",
                audio: !self.cli.no_audio,
            },
        ));
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Event) {
        match event {
            Event::Frame => {
                let picture = self.slot.lock().unwrap_or_else(|e| e.into_inner()).take();
                if let Some(p) = picture {
                    self.picture = Some(p);
                    if let Some(gfx) = &self.gfx {
                        gfx.window.request_redraw();
                    }
                }
            }
            Event::Connected { video, headless } => {
                let first = self.video.is_empty();
                self.headless = headless;
                self.video = video;
                if first {
                    if self.connected_before {
                        eprintln!("gliff: reconnected");
                    } else {
                        eprintln!("gliff: connected. {KEY_HELP}");
                        self.connected_before = true;
                        if let Some(host) = &self.cli.host {
                            setup::remember_host(host);
                        }
                    }
                    self.set_title(&self.video);
                    if headless {
                        if let Some(gfx) = &self.gfx {
                            gfx.window.set_cursor_visible(false);
                        }
                    }
                    // Fit the remote to this window straight away.
                    self.send_resize();
                }
            }
            Event::Stats {
                fps,
                mbit,
                decode_ms,
            } => {
                self.set_title(&format!(
                    "{} · {fps:.0} fps · {mbit:.1} Mbit/s · decode {decode_ms:.1} ms",
                    self.video
                ));
            }
            Event::Cursor {
                width,
                height,
                hot_x,
                hot_y,
                argb,
            } => self.set_cursor(event_loop, width, height, (hot_x, hot_y), &argb),
            Event::Key { keycode, pressed } => self.on_key(keycode, pressed, "the keyboard hook"),
            Event::Reconnecting { reason, in_secs } => {
                // The next session starts afresh: no keys held, and it
                // must be told the window size again.
                self.pressed.clear();
                self.resize_sent = None;
                self.video.clear();
                eprintln!("gliff: connection lost ({reason}); reconnecting in {in_secs} s");
                self.set_title(&format!("reconnecting… ({reason})"));
            }
            Event::Error(e) => {
                self.exit_error = Some(e);
                event_loop.exit();
            }
            Event::Closed => event_loop.exit(),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.release_keys();
                self.input = None;
                event_loop.exit();
            }
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                self.resize_due = Some(Instant::now() + RESIZE_SETTLE);
                if let Some(gfx) = &self.gfx {
                    gfx.window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::Focused(focused) => {
                #[cfg(windows)]
                capture::set_focused(focused);
                if !focused {
                    self.release_keys();
                }
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } => {
                // winit makes up presses and releases for held keys when
                // focus changes; only real keys go to the remote.
                if is_synthetic || event.repeat {
                    return;
                }
                // The hook takes keys while it captures; one that reaches
                // the window anyway is still the user's, so it is sent on.
                #[cfg(windows)]
                if self.hooked() && !self.released && !self.stray_key_noted {
                    self.stray_key_noted = true;
                    tracing::debug!(
                        "a key reached the window instead of the keyboard hook ({:?} {:?}); sending it on",
                        event.physical_key, event.state
                    );
                }
                let PhysicalKey::Code(code) = event.physical_key else {
                    return;
                };
                let Some(keycode) = keys::evdev_key(code) else {
                    tracing::debug!(?code, "no evdev code for key");
                    return;
                };
                self.on_key(keycode, event.state == ElementState::Pressed, "winit");
            }
            WindowEvent::CursorMoved { position, .. } => self.pointer_motion(position),
            WindowEvent::MouseInput { state, button, .. } => {
                let pressed = state == ElementState::Pressed;
                if pressed {
                    self.recapture();
                    // A click should make the window active; if Windows has
                    // not, ask for it, so the keyboard follows the click.
                    #[cfg(windows)]
                    if let Some(gfx) = &self.gfx {
                        if !gfx.window.has_focus() {
                            gfx.window.focus_window();
                        }
                    }
                }
                self.send(ClientMsg::PointerButton {
                    button: keys::evdev_button(button),
                    pressed,
                });
            }
            WindowEvent::MouseWheel { delta, .. } => self.scroll(delta),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        match self.resize_due {
            Some(due) if Instant::now() >= due => {
                self.send_resize();
                event_loop.set_control_flow(ControlFlow::Wait);
            }
            Some(due) => event_loop.set_control_flow(ControlFlow::WaitUntil(due)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }
}

/// Install the keyboard hook, or `None` to fall back to winit's key
/// events, which Windows filters (no Win key, no Alt+Tab). Printed rather
/// than logged: whether shortcuts reach the remote is the first thing to
/// check when they do not.
#[cfg(windows)]
fn install_hook(proxy: EventLoopProxy<Event>) -> Option<capture::Hook> {
    match capture::install(proxy) {
        Ok(hook) => {
            tracing::debug!("keyboard hook installed");
            Some(hook)
        }
        Err(code) => {
            eprintln!("gliff: cannot install the keyboard hook (error {code}); Win and Alt+Tab stay with Windows");
            None
        }
    }
}

fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();
    let mut cli = Cli::parse();
    if cli.install {
        #[cfg(windows)]
        return match setup::install() {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("gliff: install failed: {e:#}");
                std::process::ExitCode::FAILURE
            }
        };
        #[cfg(not(windows))]
        {
            eprintln!("gliff: --install is for Windows");
            return std::process::ExitCode::FAILURE;
        }
    }
    match &cli.server_bin {
        Some(bin) => setup::remember_server_bin(bin),
        None => cli.server_bin = setup::server_bin(),
    }
    if cli.host.is_none() && cli.connect.is_none() && !cli.probe_keys {
        cli.host = setup::last_host().or_else(setup::ask_host);
        match &cli.host {
            Some(host) => eprintln!("gliff: connecting to {host}"),
            None => {
                eprintln!("gliff: no machine to connect to; run gliff-win user@host");
                return std::process::ExitCode::FAILURE;
            }
        }
    }
    let event_loop = match EventLoop::<Event>::with_user_event().build() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("gliff: cannot start the event loop: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let mut app = App::new(cli, event_loop.create_proxy());
    // winit registers for raw keyboard input to report device events, and
    // while it does, Windows skips low-level keyboard hooks for our own
    // window, so the capture hook never sees a key. Gliff uses no device
    // events. `--probe-keys` in CI guards this.
    #[cfg(windows)]
    event_loop.listen_device_events(winit::event_loop::DeviceEvents::Never);
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("gliff: {e}");
        return std::process::ExitCode::FAILURE;
    }
    match app.exit_error {
        Some(e) => {
            eprintln!("gliff: {e}");
            std::process::ExitCode::FAILURE
        }
        None => std::process::ExitCode::SUCCESS,
    }
}

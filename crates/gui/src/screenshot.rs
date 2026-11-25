//! Debug builds only: the window saves a picture of itself for design
//! review. The picture comes from the window's own frame, not
//! from the screen, so the system asks for no screen-recording permission.
//!
//! `MARSA_GUI_SCREENSHOT=<file.bmp>` saves the first frame drawn once the
//! window has connected to the daemon (or after `LIMIT`) as an
//! uncompressed 24-bit BMP, then closes the window.
//! `MARSA_GUI_THEME=light|dark` draws that theme instead of the system's.
//! `MARSA_GUI_SIZE=900x720` opens the window at that size.
//! `MARSA_GUI_ROUTE=home|sites|runtimes|runtimes:<runtime>|services|logs|logs:<service>|mail|dumps|settings|site:<name>`
//! opens that screen (`site:shop` is the page of the site `shop`; `runtimes:node` is Node.js chosen). `MARSA_GUI_DEMO=ok|caddy-stopped|first-run-1|first-run-2|sites-search|sites-empty|sites-extras|site-parked|runtimes|runtimes-installing|runtimes-install|settings|settings-setup|services|logs|logs-filtered|logs-no-match|logs-long|mail|mail-empty|mail-stopped|dumps|dumps-empty|dumps-off|dumps-busy|dumps-long|confirm-delete-mail|confirm-unlink|confirm-uninstall|notice-success|notice-error`
//! draws fixed fake data and starts no daemon (see `demo`).
//! `MARSA_GUI_CLICK=x,y` clicks that point (in points, from the top left of
//! the window) once the window has settled, so a menu is open in the picture
//!. `MARSA_GUI_HOVER=x,y` then moves the pointer there and waits
//! for the tooltip, so the reason over a disabled row is in the picture (it needs a click). `MARSA_GUI_SCROLL=x,y,dx,dy` puts the pointer on
//! that point and turns the wheel by that much, so a list that scrolls
//! sideways is scrolled in the picture. `MARSA_GUI_KEY=1|esc`
//! presses that key once the window has settled (a number opens
//! a site of the menu-bar panel, Esc closes it, so no picture comes and the
//! exit code is the result; the window of a picture has no daemon to ask).
//! A window with see-through corners (the panel) is drawn over a gray
//! backdrop in the picture. On macOS,
//! `sips -s format png <file.bmp> --out <file.png>` converts the picture.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};
use std::{fs, io};

use eframe::egui;
use marsa_core::runtimes::Runtime;

use crate::model::{LogSource, Route};

/// The file to save; read once at start.
const PATH_VAR: &str = "MARSA_GUI_SCREENSHOT";
/// `light` or `dark`.
const THEME_VAR: &str = "MARSA_GUI_THEME";
/// The window's size when it opens: `900x720`.
const SIZE_VAR: &str = "MARSA_GUI_SIZE";
/// The screen the window opens on: `sites`.
const ROUTE_VAR: &str = "MARSA_GUI_ROUTE";
/// The point to click before the picture: `600,200`.
const CLICK_VAR: &str = "MARSA_GUI_CLICK";
/// The point to rest the pointer on after the click: `640,260`.
const HOVER_VAR: &str = "MARSA_GUI_HOVER";
/// A wheel to turn over a point, before the picture: `700,400,-400,0` (the
/// point, then how far to move the content, sideways and down).
const SCROLL_VAR: &str = "MARSA_GUI_SCROLL";
/// A key to press before the picture: `1`, or `esc`.
const KEY_VAR: &str = "MARSA_GUI_KEY";
/// What shows behind the see-through parts of a window in the picture: a
/// gray that neither theme's panel blends into.
const BACKDROP: [u8; 3] = [0x8a, 0x8f, 0x94];
/// The longest wait for the daemon before the picture is taken anyway.
const LIMIT: Duration = Duration::from_secs(10);
/// The wait after the window connected, so the daemon's lists arrive and the
/// layout settles first.
const SETTLE: Duration = Duration::from_secs(1);
/// The size of the BMP file header and its info header.
const HEADER: usize = 54;

/// Where a click injected for the picture is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClickStage {
    /// Not yet: the window has not settled.
    Wait,
    /// The next frame presses the button.
    Press,
    /// The next frame lets it go.
    Release,
    /// The click is done; the menu it opened draws until this time.
    Settle(Instant),
    /// The next frame moves the pointer to the hover point.
    Hover,
    /// The pointer rests there; its tooltip draws until this time.
    Tip(Instant),
    Done,
}

/// Where a wheel turn injected for the picture is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScrollStage {
    /// Not yet: the window has not settled.
    Wait,
    /// The next frame turns the wheel.
    Send,
    /// The content scrolls until this time.
    Settle(Instant),
    Done,
}

/// Where a key press injected for the picture is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyStage {
    /// Not yet: the window has not settled.
    Wait,
    /// The next frame presses the key and lets it go.
    Send,
    /// The window reacts until this time.
    Settle(Instant),
    Done,
}

/// How long the content takes to scroll after the wheel turned.
const SCROLL_SETTLE: Duration = Duration::from_millis(1000);

/// How long a menu takes to open and fade in.
const MENU_SETTLE: Duration = Duration::from_millis(500);
/// How long a tooltip takes to show, after the pointer rests.
const TIP_SETTLE: Duration = Duration::from_millis(1000);

pub struct Screenshot {
    path: PathBuf,
    /// The click to make before the picture, and how far it is.
    click: Option<(egui::Pos2, ClickStage)>,
    /// Where the pointer rests after the click.
    hover: Option<egui::Pos2>,
    /// The wheel to turn: over this point, by this much.
    scroll: Option<(egui::Pos2, egui::Vec2, ScrollStage)>,
    /// The key to press.
    key: Option<(egui::Key, KeyStage)>,
    started: Instant,
    /// When the window first showed the daemon's data.
    ready_at: Option<Instant>,
    /// The result of writing the file, once the picture was asked for.
    saved: Option<Receiver<io::Result<()>>>,
    /// The file is written (or failed) and the window is closing.
    done: bool,
}

impl Screenshot {
    /// Reads the variables. `None` unless a file is named. Sets the theme
    /// the variable names.
    pub fn start(ctx: &egui::Context) -> Option<Self> {
        let path = PathBuf::from(std::env::var_os(PATH_VAR)?);
        match std::env::var(THEME_VAR).as_deref() {
            Ok("light") => ctx.set_theme(egui::Theme::Light),
            Ok("dark") => ctx.set_theme(egui::Theme::Dark),
            _ => {}
        }
        let click = std::env::var(CLICK_VAR)
            .ok()
            .and_then(|text| parse_point(&text))
            .map(|at| (at, ClickStage::Wait));
        let hover = std::env::var(HOVER_VAR)
            .ok()
            .and_then(|text| parse_point(&text));
        let scroll = std::env::var(SCROLL_VAR)
            .ok()
            .and_then(|text| parse_wheel(&text))
            .map(|(at, delta)| (at, delta, ScrollStage::Wait));
        let key = std::env::var(KEY_VAR)
            .ok()
            .and_then(|text| parse_key(&text))
            .map(|key| (key, KeyStage::Wait));
        Some(Self {
            path,
            click,
            hover,
            scroll,
            key,
            started: Instant::now(),
            ready_at: None,
            saved: None,
            done: false,
        })
    }

    /// Called at the end of each frame. `ready`: the window shows the
    /// daemon's data.
    pub fn frame(&mut self, ctx: &egui::Context, ready: bool) {
        if self.done {
            return;
        }
        if let Some(saved) = &self.saved {
            let result = match saved.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => Err(io::Error::other("no picture came")),
            };
            match result {
                Ok(()) => tracing::info!(path = %self.path.display(), "screenshot saved"),
                Err(err) => tracing::warn!(error = %err, "cannot save the screenshot"),
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            self.done = true;
            return;
        }
        if self.ready_at.is_none() && (ready || self.started.elapsed() >= LIMIT) {
            self.ready_at = Some(Instant::now());
        }
        let Some(ready_at) = self.ready_at else {
            ctx.request_repaint_after(LIMIT);
            return;
        };
        if ready_at.elapsed() < SETTLE {
            ctx.request_repaint_after(SETTLE);
            return;
        }
        let hover = self.hover;
        if let Some((_, stage)) = &mut self.click {
            match *stage {
                ClickStage::Wait => *stage = ClickStage::Press,
                ClickStage::Press | ClickStage::Release | ClickStage::Hover => {}
                ClickStage::Settle(until) if Instant::now() >= until => {
                    *stage = if hover.is_some() {
                        ClickStage::Hover
                    } else {
                        ClickStage::Done
                    };
                }
                ClickStage::Tip(until) if Instant::now() >= until => {
                    *stage = ClickStage::Done;
                }
                ClickStage::Settle(_) | ClickStage::Tip(_) | ClickStage::Done => {}
            }
            if *stage != ClickStage::Done {
                ctx.request_repaint();
                return;
            }
        }
        if let Some((_, _, stage)) = &mut self.scroll {
            match *stage {
                ScrollStage::Wait => *stage = ScrollStage::Send,
                ScrollStage::Settle(until) if Instant::now() >= until => {
                    *stage = ScrollStage::Done;
                }
                ScrollStage::Send | ScrollStage::Settle(_) | ScrollStage::Done => {}
            }
            if *stage != ScrollStage::Done {
                ctx.request_repaint();
                return;
            }
        }
        if let Some((_, stage)) = &mut self.key {
            match *stage {
                KeyStage::Wait => *stage = KeyStage::Send,
                KeyStage::Settle(until) if Instant::now() >= until => *stage = KeyStage::Done,
                KeyStage::Send | KeyStage::Settle(_) | KeyStage::Done => {}
            }
            if *stage != KeyStage::Done {
                ctx.request_repaint();
                return;
            }
        }
        let (send, saved) = mpsc::channel();
        let path = self.path.clone();
        let waker = ctx.clone();
        ctx.request_screenshot(move |image| {
            let _ = send.send(bmp(&image).and_then(|bytes| fs::write(&path, bytes)));
            // The callback does not wake the window by itself.
            waker.request_repaint();
        });
        self.saved = Some(saved);
    }
}

impl Screenshot {
    /// Called before each frame, with the input it will read: adds the
    /// press and then the release of the click, one frame each.
    pub fn inject(&mut self, input: &mut egui::RawInput) {
        if let Some((at, delta, stage)) = &mut self.scroll
            && *stage == ScrollStage::Send
        {
            input.events.push(egui::Event::PointerMoved(*at));
            input.events.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: *delta,
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::NONE,
                source: egui::MouseWheelSource::Wheel,
            });
            *stage = ScrollStage::Settle(Instant::now() + SCROLL_SETTLE);
        }
        if let Some((key, stage)) = &mut self.key
            && *stage == KeyStage::Send
        {
            let press = |pressed| egui::Event::Key {
                key: *key,
                physical_key: Some(*key),
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            };
            input.events.push(press(true));
            input.events.push(press(false));
            *stage = KeyStage::Settle(Instant::now() + MENU_SETTLE);
        }
        let hover = self.hover;
        let Some((at, stage)) = &mut self.click else {
            return;
        };
        let button = |pressed| egui::Event::PointerButton {
            pos: *at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        match *stage {
            ClickStage::Press => {
                input.events.push(egui::Event::PointerMoved(*at));
                input.events.push(button(true));
                *stage = ClickStage::Release;
            }
            ClickStage::Release => {
                input.events.push(button(false));
                *stage = ClickStage::Settle(Instant::now() + MENU_SETTLE);
            }
            ClickStage::Hover => {
                if let Some(over) = hover {
                    input.events.push(egui::Event::PointerMoved(over));
                }
                *stage = ClickStage::Tip(Instant::now() + TIP_SETTLE);
            }
            ClickStage::Wait | ClickStage::Settle(_) | ClickStage::Tip(_) | ClickStage::Done => {}
        }
    }
}

/// `600,200` as a point.
fn parse_point(text: &str) -> Option<egui::Pos2> {
    let (x, y) = text.split_once(',')?;
    Some(egui::pos2(x.trim().parse().ok()?, y.trim().parse().ok()?))
}

/// `1` to `9`, or `esc`, as a key.
fn parse_key(text: &str) -> Option<egui::Key> {
    Some(match text.trim() {
        "1" => egui::Key::Num1,
        "2" => egui::Key::Num2,
        "3" => egui::Key::Num3,
        "4" => egui::Key::Num4,
        "5" => egui::Key::Num5,
        "6" => egui::Key::Num6,
        "7" => egui::Key::Num7,
        "8" => egui::Key::Num8,
        "9" => egui::Key::Num9,
        "esc" => egui::Key::Escape,
        _ => return None,
    })
}

/// `700,400,-400,0` as a point and how far to move the content.
fn parse_wheel(text: &str) -> Option<(egui::Pos2, egui::Vec2)> {
    let mut parts = text.split(',').map(|part| part.trim().parse::<f32>());
    let (x, y, dx, dy) = (
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    );
    parts
        .next()
        .is_none()
        .then(|| (egui::pos2(x, y), egui::vec2(dx, dy)))
}

/// The size `MARSA_GUI_SIZE` asks for, if it names one.
pub fn size() -> Option<egui::Vec2> {
    parse_size(&std::env::var(SIZE_VAR).ok()?)
}

/// `900x720` as a width and a height.
fn parse_size(text: &str) -> Option<egui::Vec2> {
    let (width, height) = text.split_once('x')?;
    Some(egui::vec2(
        width.trim().parse().ok()?,
        height.trim().parse().ok()?,
    ))
}

/// The screen `MARSA_GUI_ROUTE` asks for, if it names one.
pub fn route() -> Option<Route> {
    parse_route(&std::env::var(ROUTE_VAR).ok()?)
}

fn parse_route(name: &str) -> Option<Route> {
    if let Some(site) = name.strip_prefix("site:") {
        return (!site.is_empty()).then(|| Route::Site(site.to_owned()));
    }
    if let Some(id) = name.strip_prefix("logs:") {
        return id.parse().ok().map(|id| Route::Logs(LogSource::of(id)));
    }
    if let Some(folder) = name.strip_prefix("runtimes:") {
        let runtime = Runtime::ALL.into_iter().find(|r| r.dir_name() == folder);
        return runtime.map(Route::Runtimes);
    }
    Some(match name {
        "home" => Route::Home,
        "sites" => Route::Sites,
        "runtimes" => Route::Runtimes(Runtime::Php),
        "services" => Route::Services,
        "logs" => Route::Logs(LogSource::Caddy),
        "mail" => Route::Mail,
        "dumps" => Route::Dumps,
        "databases" => Route::Databases,
        "settings" => Route::Settings,
        _ => return None,
    })
}

/// `image` as an uncompressed 24-bit BMP: rows from the bottom up, each
/// padded to 4 bytes, blue before green before red.
fn bmp(image: &egui::ColorImage) -> io::Result<Vec<u8>> {
    let [width, height] = image.size;
    let row = (width * 3).next_multiple_of(4);
    let pixels = row * height;
    let too_large = |_| io::Error::other("the picture is too large for a BMP");
    let file_size = u32::try_from(HEADER + pixels).map_err(too_large)?;
    let width_field = i32::try_from(width).map_err(too_large)?;
    let height_field = i32::try_from(height).map_err(too_large)?;
    let mut out = Vec::with_capacity(HEADER + pixels);
    // The file header: "BM", the file size, two reserved words, where the
    // pixels start.
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&file_size.to_le_bytes());
    out.extend_from_slice(&0_u32.to_le_bytes());
    out.extend_from_slice(&(HEADER as u32).to_le_bytes());
    // The info header (BITMAPINFOHEADER): its size, width, height, one
    // plane, 24 bits a pixel, no compression, the pixel bytes, 72 dpi
    // (2835 pixels a metre), no palette.
    out.extend_from_slice(&40_u32.to_le_bytes());
    out.extend_from_slice(&width_field.to_le_bytes());
    out.extend_from_slice(&height_field.to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes());
    out.extend_from_slice(&24_u16.to_le_bytes());
    out.extend_from_slice(&0_u32.to_le_bytes());
    out.extend_from_slice(&(file_size - HEADER as u32).to_le_bytes());
    out.extend_from_slice(&2835_i32.to_le_bytes());
    out.extend_from_slice(&2835_i32.to_le_bytes());
    out.extend_from_slice(&0_u32.to_le_bytes());
    out.extend_from_slice(&0_u32.to_le_bytes());
    for y in (0..height).rev() {
        for pixel in &image.pixels[y * width..(y + 1) * width] {
            // The pixel is premultiplied: what is behind it shows through in
            // the share the pixel does not cover.
            let [red, green, blue, alpha] = pixel.to_array();
            let behind = |back: u8| {
                let share = u16::from(255 - alpha);
                u8::try_from((u16::from(back) * share + 127) / 255).unwrap_or(u8::MAX)
            };
            let [back_red, back_green, back_blue] = BACKDROP;
            out.extend_from_slice(&[
                blue.saturating_add(behind(back_blue)),
                green.saturating_add(behind(back_green)),
                red.saturating_add(behind(back_red)),
            ]);
        }
        out.resize(out.len() + row - width * 3, 0);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use eframe::egui::Color32;

    use super::*;
    use marsa_core::types::ServiceId;

    #[test]
    fn a_size_and_a_route_are_read_from_their_names() {
        assert_eq!(parse_size("900x720"), Some(egui::vec2(900.0, 720.0)));
        assert_eq!(parse_size("900"), None);
        assert_eq!(parse_size("wide x 720"), None);
        assert_eq!(parse_route("sites"), Some(Route::Sites));
        assert_eq!(parse_route("logs"), Some(Route::Logs(LogSource::Caddy)));
        assert_eq!(
            parse_route("logs:php-fpm-8.4"),
            Some(Route::Logs(LogSource::Service(
                "php-fpm-8.4".parse::<ServiceId>().unwrap()
            )))
        );
        assert_eq!(
            parse_route("logs:caddy"),
            Some(Route::Logs(LogSource::Caddy))
        );
        assert_eq!(parse_route("logs:"), None);
        assert_eq!(parse_route("mail"), Some(Route::Mail));
        assert_eq!(parse_route("dumps"), Some(Route::Dumps));
        assert_eq!(parse_route("queues"), None);
        assert_eq!(parse_route("site:shop"), Some(Route::Site("shop".into())));
        assert_eq!(parse_route("site:"), None);
        assert_eq!(
            parse_route("runtimes:node"),
            Some(Route::Runtimes(Runtime::Node))
        );
        assert_eq!(parse_route("runtimes:cobol"), None);
        assert_eq!(parse_point("600, 200"), Some(egui::pos2(600.0, 200.0)));
        assert_eq!(parse_point("600"), None);
    }

    #[test]
    fn a_click_is_a_press_then_a_release_then_a_while_to_draw_the_menu() {
        let mut shot = Screenshot {
            path: PathBuf::new(),
            click: Some((egui::pos2(10.0, 20.0), ClickStage::Press)),
            hover: Some(egui::pos2(30.0, 40.0)),
            scroll: None,
            key: None,
            started: Instant::now(),
            ready_at: None,
            saved: None,
            done: false,
        };
        let mut input = egui::RawInput::default();
        shot.inject(&mut input);
        assert_eq!(input.events.len(), 2);
        assert!(matches!(
            input.events[1],
            egui::Event::PointerButton { pressed: true, .. }
        ));
        let mut input = egui::RawInput::default();
        shot.inject(&mut input);
        assert!(matches!(
            input.events[..],
            [egui::Event::PointerButton { pressed: false, .. }]
        ));
        assert!(matches!(
            shot.click.map(|(_, stage)| stage),
            Some(ClickStage::Settle(_))
        ));
        // Then the pointer moves to rest on the hover point, once.
        shot.click = shot.click.map(|(at, _)| (at, ClickStage::Hover));
        let mut input = egui::RawInput::default();
        shot.inject(&mut input);
        assert!(matches!(
            input.events[..],
            [egui::Event::PointerMoved(at)] if at == egui::pos2(30.0, 40.0)
        ));
        assert!(matches!(
            shot.click.map(|(_, stage)| stage),
            Some(ClickStage::Tip(_))
        ));
        let mut input = egui::RawInput::default();
        shot.inject(&mut input);
        assert!(input.events.is_empty());
    }

    #[test]
    fn a_wheel_is_read_from_its_point_and_its_distance_and_turns_once() {
        assert_eq!(
            parse_wheel("700, 400, -400, 0"),
            Some((egui::pos2(700.0, 400.0), egui::vec2(-400.0, 0.0)))
        );
        assert_eq!(parse_wheel("700,400"), None);
        assert_eq!(parse_wheel("1,2,3,4,5"), None);
        assert_eq!(parse_wheel("a,2,3,4"), None);
        let mut shot = Screenshot {
            path: PathBuf::new(),
            click: None,
            hover: None,
            scroll: Some((
                egui::pos2(5.0, 6.0),
                egui::vec2(-10.0, 0.0),
                ScrollStage::Send,
            )),
            key: None,
            started: Instant::now(),
            ready_at: None,
            saved: None,
            done: false,
        };
        let mut input = egui::RawInput::default();
        shot.inject(&mut input);
        assert!(matches!(
            input.events[..],
            [
                egui::Event::PointerMoved(_),
                egui::Event::MouseWheel { delta, .. }
            ] if delta == egui::vec2(-10.0, 0.0)
        ));
        assert!(matches!(
            shot.scroll.map(|(_, _, stage)| stage),
            Some(ScrollStage::Settle(_))
        ));
        // The wheel turns once: the next frame has no event.
        let mut input = egui::RawInput::default();
        shot.inject(&mut input);
        assert!(input.events.is_empty());
    }

    #[test]
    fn a_key_is_read_from_its_name_and_pressed_and_let_go_once() {
        assert_eq!(parse_key("1"), Some(egui::Key::Num1));
        assert_eq!(parse_key(" 9 "), Some(egui::Key::Num9));
        assert_eq!(parse_key("esc"), Some(egui::Key::Escape));
        assert_eq!(parse_key("0"), None);
        assert_eq!(parse_key("enter"), None);
        let mut shot = Screenshot {
            path: PathBuf::new(),
            click: None,
            hover: None,
            scroll: None,
            key: Some((egui::Key::Num3, KeyStage::Wait)),
            started: Instant::now(),
            ready_at: None,
            saved: None,
            done: false,
        };
        // Not yet: the window has not settled.
        let mut input = egui::RawInput::default();
        shot.inject(&mut input);
        assert!(input.events.is_empty());
        shot.key = Some((egui::Key::Num3, KeyStage::Send));
        shot.inject(&mut input);
        assert!(matches!(
            input.events[..],
            [
                egui::Event::Key {
                    key: egui::Key::Num3,
                    pressed: true,
                    ..
                },
                egui::Event::Key {
                    key: egui::Key::Num3,
                    pressed: false,
                    ..
                },
            ]
        ));
        // Once.
        let mut input = egui::RawInput::default();
        shot.inject(&mut input);
        assert!(input.events.is_empty());
    }

    #[test]
    fn a_see_through_pixel_shows_the_backdrop_and_a_solid_one_does_not() {
        let clear = Color32::TRANSPARENT;
        let half = Color32::from_rgba_premultiplied(100, 100, 100, 128);
        let image = egui::ColorImage::new([3, 1], vec![clear, Color32::RED, half]);
        let bytes = bmp(&image).unwrap();
        let pixel = |x: usize| &bytes[HEADER + 3 * x..HEADER + 3 * x + 3];
        let [red, green, blue] = BACKDROP;
        assert_eq!(pixel(0), [blue, green, red]);
        assert_eq!(pixel(1), [0, 0, 255]);
        // Half covered: its own light plus half of the backdrop.
        let mix = |own: u8, back: u8| own + ((u16::from(back) * 127 + 127) / 255) as u8;
        assert_eq!(pixel(2), [mix(100, blue), mix(100, green), mix(100, red)]);
    }

    #[test]
    fn a_bmp_has_its_headers_and_padded_bottom_up_rows() {
        // Two pixels a row: 6 bytes, padded to 8.
        let pixels = vec![Color32::RED, Color32::GREEN, Color32::BLUE, Color32::WHITE];
        let image = egui::ColorImage::new([2, 2], pixels);
        let bytes = bmp(&image).unwrap();
        assert_eq!(bytes.len(), HEADER + 2 * 8);
        assert_eq!(&bytes[..2], b"BM");
        assert_eq!(u32::from_le_bytes(bytes[2..6].try_into().unwrap()), 70);
        assert_eq!(u16::from_le_bytes(bytes[28..30].try_into().unwrap()), 24);
        // The bottom row comes first: blue, then white, in BGR.
        assert_eq!(
            &bytes[HEADER..HEADER + 8],
            &[255, 0, 0, 255, 255, 255, 0, 0]
        );
        // Then the top row: red, then green.
        assert_eq!(
            &bytes[HEADER + 8..HEADER + 16],
            &[0, 0, 255, 0, 255, 0, 0, 0]
        );
    }
}

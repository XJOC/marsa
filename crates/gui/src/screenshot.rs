//! Debug builds only: the window saves a picture of itself for design
//! review (task M5-D1). The picture comes from the window's own frame, not
//! from the screen, so the system asks for no screen-recording permission.
//!
//! `MARSA_GUI_SCREENSHOT=<file.bmp>` saves the first frame drawn once the
//! window has connected to the daemon (or after [`LIMIT`]) as an
//! uncompressed 24-bit BMP, then closes the window.
//! `MARSA_GUI_THEME=light|dark` draws that theme instead of the system's.
//! `MARSA_GUI_SIZE=900x720` opens the window at that size (task M5-D4).
//! `MARSA_GUI_ROUTE=home|sites|runtimes|services|logs|settings` opens that
//! screen. `MARSA_GUI_DEMO=ok|caddy-stopped|first-run-1|first-run-2` draws
//! fixed fake data and starts no daemon (task M5-D5, see `demo`).
//! `MARSA_GUI_CLICK=x,y` clicks that point (in points, from the top left of
//! the window) once the window has settled, so a menu is open in the picture
//! (task M5-D6). On macOS,
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
    Done,
}

/// How long a menu takes to open and fade in.
const MENU_SETTLE: Duration = Duration::from_millis(500);

pub struct Screenshot {
    path: PathBuf,
    /// The click to make before the picture, and how far it is.
    click: Option<(egui::Pos2, ClickStage)>,
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
        Some(Self {
            path,
            click,
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
        if let Some((_, stage)) = &mut self.click {
            match *stage {
                ClickStage::Wait => *stage = ClickStage::Press,
                ClickStage::Press | ClickStage::Release => {}
                ClickStage::Settle(until) if Instant::now() >= until => {
                    *stage = ClickStage::Done;
                }
                ClickStage::Settle(_) | ClickStage::Done => {}
            }
            if *stage != ClickStage::Done {
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
            ClickStage::Wait | ClickStage::Settle(_) | ClickStage::Done => {}
        }
    }
}

/// `600,200` as a point.
fn parse_point(text: &str) -> Option<egui::Pos2> {
    let (x, y) = text.split_once(',')?;
    Some(egui::pos2(x.trim().parse().ok()?, y.trim().parse().ok()?))
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
    Some(match name {
        "home" => Route::Home,
        "sites" => Route::Sites,
        "runtimes" => Route::Runtimes(Runtime::Php),
        "services" => Route::Services,
        "logs" => Route::Logs(LogSource::Caddy),
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
            let [red, green, blue, _] = pixel.to_srgba_unmultiplied();
            out.extend_from_slice(&[blue, green, red]);
        }
        out.resize(out.len() + row - width * 3, 0);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use eframe::egui::Color32;

    use super::*;

    #[test]
    fn a_size_and_a_route_are_read_from_their_names() {
        assert_eq!(parse_size("900x720"), Some(egui::vec2(900.0, 720.0)));
        assert_eq!(parse_size("900"), None);
        assert_eq!(parse_size("wide x 720"), None);
        assert_eq!(parse_route("sites"), Some(Route::Sites));
        assert_eq!(parse_route("logs"), Some(Route::Logs(LogSource::Caddy)));
        assert_eq!(parse_route("mail"), None);
        assert_eq!(parse_point("600, 200"), Some(egui::pos2(600.0, 200.0)));
        assert_eq!(parse_point("600"), None);
    }

    #[test]
    fn a_click_is_a_press_then_a_release_then_a_while_to_draw_the_menu() {
        let mut shot = Screenshot {
            path: PathBuf::new(),
            click: Some((egui::pos2(10.0, 20.0), ClickStage::Press)),
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
        let mut input = egui::RawInput::default();
        shot.inject(&mut input);
        assert!(input.events.is_empty());
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

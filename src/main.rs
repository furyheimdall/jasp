mod autostart;
mod backend;
mod chrome;
mod dac;
mod dialog;
mod ja;
mod ja_library;
mod ja_library_admin_ui;
mod ja_ui;
mod library_activity;
mod model;
mod progress;
mod settings;
mod settings_web;
mod splash;
mod startup;
mod theme;
mod track_dialog;
mod ui;

use anyhow::{Context, Result};
use clap::Parser;
use eframe::egui;
use reqwest::Url;
use std::{env, os::unix::process::CommandExt, process::Command};

pub(crate) const APP_NAME: &str = "Jasp";

#[derive(clap::Subcommand)]
enum Cli {
    /// Manage Jasp autostart on moOde, a desktop, or a Lite kiosk.
    Autostart { action: autostart::Action },
}

#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// Base URL of the optional moOde HTTP API (defaults to http://moode.local).
    #[arg(long)]
    server: Option<Url>,
    /// MPD port on the same host.
    #[arg(long, default_value_t = 6600)]
    mpd_port: u16,
    /// Override the saved jastreamer Server URL for this launch.
    #[arg(long, default_value = "")]
    jastreamer: String,
    /// Start without sending any playback or volume commands.
    #[arg(long)]
    read_only: bool,
    /// Occupy the existing display without changing display orientation.
    #[arg(long)]
    fullscreen: bool,
    /// System font file to use for glyphs missing from the bundled fonts.
    #[arg(long)]
    font: Option<std::path::PathBuf>,
    /// Report live status, queue count and library root without opening a window.
    #[arg(long)]
    probe: bool,
    #[command(subcommand)]
    command: Option<Cli>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if let Some(Cli::Autostart { action }) = args.command { return autostart::command(action); }
    if !args.probe && !["DISPLAY", "WAYLAND_DISPLAY", "WAYLAND_SOCKET"].iter()
        .any(|key| env::var_os(key).is_some_and(|value| !value.is_empty()))
    {
        anyhow::ensure!(
            autostart::Autostart::detect()?.backend == autostart::Backend::Lite,
            "No graphical session. On moOde, start the local display first; on Desktop, open Jasp after login."
        );
        let error = Command::new("/usr/libexec/jasp/jasp-console").args(env::args_os().skip(1)).exec();
        return Err(error).context("Opening the Jasp console launcher; install the Jasp Debian package");
    }
    let fullscreen = args.fullscreen || env::var("JASP_LITE_KIOSK").as_deref() == Ok("1");
    let show_moode = args.server.is_some();
    let server = args.server.unwrap_or_else(|| Url::parse("http://moode.local").expect("valid default moOde URL"));
    let client = backend::Client::new(server, args.mpd_port)?;
    if args.probe {
        return client.probe();
    }
    let noto = "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc";
    let fonts = if let Some(path) = &args.font {
        vec![std::fs::read(path).with_context(|| format!("reading font {}", path.display()))?]
    } else if let Ok(font) = std::fs::read(noto) {
        vec![font]
    } else {
        [
            "/usr/share/fonts/truetype/fonts-japanese-gothic.ttf",
            "/usr/share/fonts/truetype/unfonts-core/UnDotum.ttf",
        ].into_iter().filter_map(|path| std::fs::read(path).ok()).collect()
    };
    let bare_x11_size = fullscreen.then(bare_x11_size).flatten();
    let viewport = if fullscreen {
        egui::ViewportBuilder::default()
            .with_position([0.0, 0.0])
            .with_decorations(false)
            .with_fullscreen(bare_x11_size.is_none())
    } else {
        egui::ViewportBuilder::default()
            .with_inner_size([1040.0, 680.0])
            .with_min_inner_size([360.0, 400.0])
    };
    eframe::run_native(
        APP_NAME,
        eframe::NativeOptions { viewport, vsync: true, ..Default::default() },
        Box::new(move |cc| {
            cc.egui_ctx.set_zoom_factor(1.10);
            if !fonts.is_empty() {
                let mut definitions = egui::FontDefinitions::default();
                for (index, font) in fonts.into_iter().enumerate() {
                    let family = format!("system-fallback-{index}");
                    definitions.font_data.insert(family.clone(), egui::FontData::from_owned(font).into());
                    definitions.families.entry(egui::FontFamily::Proportional).or_default().push(family);
                }
                cc.egui_ctx.set_fonts(definitions);
            }
            if fullscreen {
                cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(false));
            }
            let connection = client.spawn(args.read_only, cc.egui_ctx.clone())?;
            let ja = ja_ui::View::new(cc.egui_ctx.clone(), args.jastreamer, args.read_only)?;
            Ok(Box::new(ui::Player::new(connection, bare_x11_size, ja, show_moode)))
        }),
    ).map_err(|err| anyhow::anyhow!(err.to_string())).context("opening the native player window")
}

#[cfg(target_os = "linux")]
fn bare_x11_size() -> Option<egui::Vec2> {
    use x11rb::{connection::Connection, protocol::xproto::{AtomEnum, ConnectionExt}};

    if ["WAYLAND_DISPLAY", "WAYLAND_SOCKET"].iter().any(|key| std::env::var_os(key).is_some()) {
        return None;
    }
    let (connection, screen) = x11rb::connect(None).ok()?;
    let root = connection.setup().roots.get(screen)?.root;
    let wm = connection.intern_atom(true, b"_NET_SUPPORTING_WM_CHECK").ok()?.reply().ok()?;
    if wm.atom != 0 && connection.get_property(false, root, wm.atom, AtomEnum::WINDOW, 0, 1)
        .ok()?.reply().ok()?.value32().and_then(|mut ids| ids.next()).is_some_and(|id| id != 0) {
        return None;
    }
    let geometry = connection.get_geometry(root).ok()?.reply().ok()?;
    Some(egui::vec2(f32::from(geometry.width), f32::from(geometry.height)))
}

#[cfg(not(target_os = "linux"))]
fn bare_x11_size() -> Option<egui::Vec2> { None }

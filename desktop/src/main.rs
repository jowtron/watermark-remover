// Watermark Remover — clean-room desktop app (pure Rust, iced).
// Removes a fixed semi-transparent watermark (the Gemini ✦ by default) by
// reversing its alpha composite. Ships with a measured Gemini profile and can
// learn any other fixed watermark from a batch of images. See engine.rs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod engine;
mod profile;

use iced::widget::image::{FilterMethod, Handle};
use iced::widget::{
    button, checkbox, column, container, pick_list, row, scrollable, slider, text, text_input,
    Space,
};
use iced::{Alignment, Border, Color, Element, Length, Size, Subscription, Task, Theme};

use engine::LoadedImage;
use profile::Profile;
use std::path::PathBuf;

fn main() -> iced::Result {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--batch") {
        batch_main(&args);
        std::process::exit(0);
    }
    iced::application("Watermark Remover", App::update, App::view)
        .subscription(App::subscription)
        .theme(App::theme)
        .window_size(Size::new(1140.0, 880.0))
        .run_with(App::new)
}

/// Headless bulk removal: `watermark-remover --batch <in_dir> [out_dir]`.
/// Uses the built-in Gemini profile, auto-locates and removes the mark in every
/// image, and reports the detection confidence (NCC) per file.
fn batch_main(args: &[String]) {
    let i = args.iter().position(|a| a == "--batch").unwrap();
    let indir = args.get(i + 1).cloned().expect("usage: --batch <in_dir> [out_dir]");
    let outdir = args.get(i + 2).cloned().unwrap_or_else(|| "clean-out".into());
    let _ = std::fs::create_dir_all(&outdir);
    let profile = Profile::with_default();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&indir)
        .expect("cannot read input dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            matches!(
                p.extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_ascii_lowercase())
                    .as_deref(),
                Some("png" | "jpg" | "jpeg" | "webp" | "bmp" | "gif" | "tiff")
            )
        })
        .collect();
    paths.sort();
    println!("{} image(s) in {indir}\n", paths.len());
    let mut ok = 0;
    for p in &paths {
        let fname = p.file_name().unwrap().to_string_lossy().into_owned();
        match engine::load_image_from_path(p) {
            Ok(img) => {
                let size = profile.pick_size(img.w, img.h).unwrap();
                let map = profile.maps.get(&size).unwrap();
                let lum = engine::lum_of(&img.rgba, img.w, img.h);
                match engine::detect(&lum, img.w, img.h, map) {
                    Some(d) => {
                        let out =
                            engine::remove_at(&img.rgba, img.w, img.h, map, d.ox, d.oy, [255, 255, 255], 1.0);
                        let stem = p.file_stem().unwrap().to_string_lossy();
                        let outp = std::path::Path::new(&outdir).join(format!("{stem}-clean.png"));
                        let _ = engine::save_png(&outp, &out, img.w, img.h);
                        println!(
                            "{:48} {:>5}x{:<5} {} ({},{}) ncc {:.3}",
                            fname, img.w, img.h, d.corner, d.ox, d.oy, d.ncc
                        );
                        ok += 1;
                    }
                    None => println!("{fname:48} detect FAILED"),
                }
            }
            Err(e) => println!("{fname:48} load error: {e}"),
        }
    }
    println!("\n{ok}/{} processed → {outdir}", paths.len());
}

// ───────────────────────────── model ─────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Watermark,
    Remove,
    About,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CornerSel {
    Auto,
    Br,
    Bl,
    Tr,
    Tl,
}

impl CornerSel {
    const REMOVE: [CornerSel; 5] = [
        CornerSel::Auto,
        CornerSel::Br,
        CornerSel::Bl,
        CornerSel::Tr,
        CornerSel::Tl,
    ];
    const LEARN: [CornerSel; 4] = [CornerSel::Br, CornerSel::Bl, CornerSel::Tr, CornerSel::Tl];
    fn code(self) -> &'static str {
        match self {
            CornerSel::Auto => "auto",
            CornerSel::Br => "br",
            CornerSel::Bl => "bl",
            CornerSel::Tr => "tr",
            CornerSel::Tl => "tl",
        }
    }
}

impl std::fmt::Display for CornerSel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            CornerSel::Auto => "Auto-detect",
            CornerSel::Br => "Bottom-right",
            CornerSel::Bl => "Bottom-left",
            CornerSel::Tr => "Top-right",
            CornerSel::Tl => "Top-left",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SizeSel {
    Auto,
    S96,
    S48,
}

impl SizeSel {
    const ALL: [SizeSel; 3] = [SizeSel::Auto, SizeSel::S96, SizeSel::S48];
}

impl std::fmt::Display for SizeSel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SizeSel::Auto => "Auto (96 if image ≥1024, else 48)",
            SizeSel::S96 => "96 px",
            SizeSel::S48 => "48 px",
        })
    }
}

struct RemovalResult {
    rgba: Vec<u8>,
    ox: i64,
    oy: i64,
    size: usize,
}

#[derive(Debug, Clone)]
enum Message {
    Tab(Tab),
    OpenImage,
    CornerChanged(CornerSel),
    NudgeX(String),
    NudgeY(String),
    Color(String),
    Strength(i32),
    ToggleOutline(bool),
    ToggleBefore(bool),
    ToggleZoom(bool),
    Save,
    AddLearnImages,
    LearnCorner(CornerSel),
    LearnSizeMsg(SizeSel),
    DoLearn,
    SaveProfile,
    LoadProfile,
    FileDropped(PathBuf),
}

struct App {
    tab: Tab,
    profile: Profile,
    profile_label: String,
    profile_explain: String,
    alpha_handle: Option<Handle>,
    composite_handle: Option<Handle>,
    profile_meta: String,
    // remove
    src: Option<LoadedImage>,
    result: Option<RemovalResult>,
    result_handle: Option<Handle>,
    result_disp: (f32, f32),
    corner_sel: CornerSel,
    nudge_x: String,
    nudge_y: String,
    color_hex: String,
    strength: i32,
    show_outline: bool,
    show_before: bool,
    show_zoom: bool,
    status: String,
    det_info: String,
    // learn
    learn_imgs: Vec<LoadedImage>,
    learn_corner: CornerSel,
    learn_size: SizeSel,
    learn_status: String,
}

// ───────────────────────────── app ─────────────────────────────

impl App {
    fn new() -> (Self, Task<Message>) {
        let (profile, label, explain) = match load_persisted_profile() {
            Some((p, _)) => (
                p,
                "Saved on this device".to_string(),
                "Your saved profile. Delete the app config folder to restore the built-in Gemini ✦ profile.".to_string(),
            ),
            None => (
                Profile::with_default(),
                "Built-in: Gemini ✦ star".to_string(),
                "Measured from a real Gemini image — nothing to set up. For Gemini images just go to the Remove tab.".to_string(),
            ),
        };
        let strength = load_strength().unwrap_or(100);
        let mut app = App {
            tab: Tab::Watermark,
            profile,
            profile_label: label,
            profile_explain: explain,
            alpha_handle: None,
            composite_handle: None,
            profile_meta: String::new(),
            src: None,
            result: None,
            result_handle: None,
            result_disp: (0.0, 0.0),
            corner_sel: CornerSel::Auto,
            nudge_x: "0".into(),
            nudge_y: "0".into(),
            color_hex: "#ffffff".into(),
            strength,
            show_outline: true,
            show_before: false,
            show_zoom: false,
            status: "Drop an image anywhere, or click Open.".into(),
            det_info: String::new(),
            learn_imgs: Vec::new(),
            learn_corner: CornerSel::Br,
            learn_size: SizeSel::Auto,
            learn_status: String::new(),
        };
        app.rebuild_profile_preview();
        (app, Task::none())
    }

    fn theme(&self) -> Theme {
        Theme::Dark
    }

    fn subscription(&self) -> Subscription<Message> {
        iced::event::listen_with(|event, _status, _id| {
            if let iced::Event::Window(iced::window::Event::FileDropped(path)) = event {
                Some(Message::FileDropped(path))
            } else {
                None
            }
        })
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tab(t) => self.tab = t,
            Message::OpenImage => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Images", &["png", "jpg", "jpeg", "webp", "bmp", "gif", "tiff"])
                    .pick_file()
                {
                    self.load_src(&path);
                }
            }
            Message::CornerChanged(c) => {
                self.corner_sel = c;
                self.run_removal();
            }
            Message::NudgeX(s) => {
                self.nudge_x = s;
                self.run_removal();
            }
            Message::NudgeY(s) => {
                self.nudge_y = s;
                self.run_removal();
            }
            Message::Color(s) => {
                self.color_hex = s;
                self.run_removal();
            }
            Message::Strength(v) => {
                self.strength = v;
                save_strength(v);
                self.run_removal();
            }
            Message::ToggleOutline(b) => {
                self.show_outline = b;
                self.rebuild_result_preview();
            }
            Message::ToggleBefore(b) => {
                self.show_before = b;
                self.rebuild_result_preview();
            }
            Message::ToggleZoom(b) => {
                self.show_zoom = b;
                self.rebuild_result_preview();
            }
            Message::Save => self.save_result(),
            Message::AddLearnImages => {
                if let Some(paths) = rfd::FileDialog::new()
                    .add_filter("Images", &["png", "jpg", "jpeg", "webp", "bmp", "gif", "tiff"])
                    .pick_files()
                {
                    for p in paths {
                        if let Ok(img) = engine::load_image_from_path(&p) {
                            self.learn_imgs.push(img);
                        }
                    }
                    self.update_learn_count();
                }
            }
            Message::LearnCorner(c) => self.learn_corner = c,
            Message::LearnSizeMsg(s) => self.learn_size = s,
            Message::DoLearn => self.do_learn(),
            Message::SaveProfile => {
                if let Some(path) = rfd::FileDialog::new()
                    .set_file_name("watermark-profile.json")
                    .add_filter("JSON", &["json"])
                    .save_file()
                {
                    let json = self.profile.to_json(self.strength as u32);
                    match std::fs::write(&path, json) {
                        Ok(()) => self.status = format!("Profile saved to {}", path.display()),
                        Err(e) => self.status = format!("Save failed: {e}"),
                    }
                }
            }
            Message::LoadProfile => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("JSON", &["json"])
                    .pick_file()
                {
                    match std::fs::read_to_string(&path)
                        .map_err(|e| e.to_string())
                        .and_then(|t| Profile::from_json(&t))
                    {
                        Ok((p, cal)) => {
                            self.profile = p;
                            if let Some(c) = cal {
                                self.strength = c as i32;
                                save_strength(self.strength);
                            }
                            self.profile_label = "Loaded from file".into();
                            self.profile_explain =
                                "Imported from a profile JSON. This replaced the built-in Gemini profile.".into();
                            self.rebuild_profile_preview();
                            persist_profile(&self.profile, self.strength);
                            self.run_removal();
                            self.learn_status = "Profile loaded.".into();
                        }
                        Err(e) => self.status = format!("Bad profile: {e}"),
                    }
                }
            }
            Message::FileDropped(path) => match self.tab {
                Tab::Remove => self.load_src(&path),
                Tab::Watermark => {
                    if let Ok(img) = engine::load_image_from_path(&path) {
                        self.learn_imgs.push(img);
                        self.update_learn_count();
                    }
                }
                Tab::About => {}
            },
        }
        Task::none()
    }

    fn load_src(&mut self, path: &std::path::Path) {
        match engine::load_image_from_path(path) {
            Ok(img) => {
                self.status = format!("{} — {}×{}", img.name, img.w, img.h);
                self.src = Some(img);
                self.tab = Tab::Remove;
                self.run_removal();
            }
            Err(e) => self.status = format!("Failed to load: {e}"),
        }
    }

    fn save_result(&mut self) {
        let Some(res) = &self.result else {
            return;
        };
        let default_name = self
            .src
            .as_ref()
            .map(|s| format!("{}-clean.png", strip_ext(&s.name)))
            .unwrap_or_else(|| "clean.png".into());
        if let Some(path) = rfd::FileDialog::new()
            .set_file_name(default_name)
            .add_filter("PNG", &["png"])
            .save_file()
        {
            let (w, h) = self.src.as_ref().map(|s| (s.w, s.h)).unwrap();
            match engine::save_png(path.as_path(), &res.rgba, w, h) {
                Ok(()) => self.status = format!("Saved {}", path.display()),
                Err(e) => self.status = format!("Save failed: {e}"),
            }
        }
    }

    fn update_learn_count(&mut self) {
        let n = self.learn_imgs.len();
        self.learn_status = format!(
            "{n} image(s) loaded.{}",
            if n < 20 {
                "  (20+ recommended for a clean result)"
            } else {
                "  good"
            }
        );
    }

    fn do_learn(&mut self) {
        if self.learn_imgs.len() < 6 {
            self.learn_status = "Add at least 6 images (20+ recommended).".into();
            return;
        }
        let size = match self.learn_size {
            SizeSel::S96 => 96,
            SizeSel::S48 => 48,
            SizeSel::Auto => {
                let big = self
                    .learn_imgs
                    .iter()
                    .filter(|im| im.w >= 1024 && im.h >= 1024)
                    .count();
                if big as f32 >= self.learn_imgs.len() as f32 / 2.0 {
                    96
                } else {
                    48
                }
            }
        };
        let corner = self.learn_corner.code();
        match engine::learn(&self.learn_imgs, corner, size) {
            Some(m) => {
                let n = self.learn_imgs.len();
                let peak = m.peak();
                let foot = m.a.iter().filter(|&&v| v > 0.02).count();
                self.profile.set_map(m);
                self.profile_label = format!("Learned from {n} images");
                self.profile_explain = format!(
                    "Measured from your {n} images ({corner} corner). This replaced the built-in Gemini profile."
                );
                self.learn_status = format!(
                    "Learned {size}×{size} watermark in {corner} from {n} images · peak α {peak:.2} · footprint {foot}px"
                );
                self.rebuild_profile_preview();
                persist_profile(&self.profile, self.strength);
                self.run_removal();
            }
            None => self.learn_status = "Images too small for this watermark size.".into(),
        }
    }

    fn run_removal(&mut self) {
        let Some(src) = &self.src else {
            return;
        };
        let (w, h) = (src.w, src.h);
        let Some(size) = self.profile.pick_size(w, h) else {
            self.det_info = "No active profile.".into();
            return;
        };
        let map = self.profile.maps.get(&size).unwrap();

        let (mut ox, mut oy, corner, ncc);
        if self.corner_sel == CornerSel::Auto {
            let lum = engine::lum_of(&src.rgba, w, h);
            match engine::detect(&lum, w, h, map) {
                Some(d) => {
                    ox = d.ox;
                    oy = d.oy;
                    corner = d.corner.to_string();
                    ncc = Some(d.ncc);
                }
                None => {
                    self.det_info = "Could not locate the watermark — pick a corner manually.".into();
                    return;
                }
            }
        } else {
            let t = size as i64;
            let m = if w >= 1024 && h >= 1024 { 64i64 } else { 32 };
            let (wi, hi) = (w as i64, h as i64);
            let (a, b) = match self.corner_sel {
                CornerSel::Br => (wi - t - m, hi - t - m),
                CornerSel::Bl => (m, hi - t - m),
                CornerSel::Tr => (wi - t - m, m),
                CornerSel::Tl => (m, m),
                CornerSel::Auto => unreachable!(),
            };
            ox = a;
            oy = b;
            corner = self.corner_sel.code().to_string();
            ncc = None;
        }
        ox += self.nudge_x.trim().parse::<i64>().unwrap_or(0);
        oy += self.nudge_y.trim().parse::<i64>().unwrap_or(0);
        let color = parse_hex(&self.color_hex).unwrap_or([255, 255, 255]);
        let strength = self.strength as f32 / 100.0;
        let out = engine::remove_at(&src.rgba, w, h, map, ox, oy, color, strength);
        self.result = Some(RemovalResult {
            rgba: out,
            ox,
            oy,
            size,
        });
        self.det_info = match ncc {
            Some(n) => format!(
                "Region {size}×{size} at {corner} ({ox},{oy}) · match {n:.2}{}",
                if n < 0.12 { "  (low — adjust manually)" } else { "" }
            ),
            None => format!("Region {size}×{size} at {corner} ({ox},{oy}) · manual"),
        };
        self.rebuild_result_preview();
    }

    fn rebuild_result_preview(&mut self) {
        let Some(src) = &self.src else { return };
        let Some(res) = &self.result else { return };
        let (w, h) = (src.w, src.h);
        let base = if self.show_before { &src.rgba } else { &res.rgba };
        let t = res.size as i64;

        if self.show_zoom {
            let pad = (res.size as f32 * 0.6).round() as i64;
            let x0 = (res.ox - pad).clamp(0, w as i64);
            let y0 = (res.oy - pad).clamp(0, h as i64);
            let x1 = (res.ox + t + pad).clamp(0, w as i64);
            let y1 = (res.oy + t + pad).clamp(0, h as i64);
            let (cw, ch) = ((x1 - x0).max(1) as usize, (y1 - y0).max(1) as usize);
            let mut crop = vec![0u8; cw * ch * 4];
            for y in 0..ch {
                for x in 0..cw {
                    let si = (((y0 as usize + y) * w) + (x0 as usize + x)) * 4;
                    let di = (y * cw + x) * 4;
                    crop[di..di + 4].copy_from_slice(&base[si..si + 4]);
                }
            }
            if self.show_outline {
                draw_rect(
                    &mut crop,
                    cw,
                    ch,
                    res.ox - x0,
                    res.oy - y0,
                    res.size as i64,
                    [59, 130, 246],
                );
            }
            self.result_disp = display_size(cw, ch, 760.0);
            self.result_handle = Some(Handle::from_rgba(cw as u32, ch as u32, crop));
        } else {
            let (pw, ph, mut px) = fit_rgba(base, w, h, 1000);
            if self.show_outline {
                let sx = pw as f32 / w as f32;
                let sy = ph as f32 / h as f32;
                draw_rect(
                    &mut px,
                    pw,
                    ph,
                    (res.ox as f32 * sx) as i64,
                    (res.oy as f32 * sy) as i64,
                    (res.size as f32 * sx) as i64,
                    [59, 130, 246],
                );
                let _ = sy;
            }
            self.result_disp = (pw as f32, ph as f32);
            self.result_handle = Some(Handle::from_rgba(pw as u32, ph as u32, px));
        }
    }

    fn rebuild_profile_preview(&mut self) {
        let size = if self.profile.maps.contains_key(&96) {
            96
        } else {
            self.profile.maps.keys().next().copied().unwrap_or(96)
        };
        let Some(m) = self.profile.maps.get(&size) else {
            return;
        };
        let t = m.size;
        // α-map as greyscale (white = opaque)
        let mut ap = vec![0u8; t * t * 4];
        for k in 0..t * t {
            let v = (m.a[k] * 255.0) as u8;
            ap[k * 4] = v;
            ap[k * 4 + 1] = v;
            ap[k * 4 + 2] = v;
            ap[k * 4 + 3] = 255;
        }
        self.alpha_handle = Some(Handle::from_rgba(t as u32, t as u32, ap));
        // white watermark composited over a checkerboard
        let mut cp = vec![0u8; t * t * 4];
        for y in 0..t {
            for x in 0..t {
                let light = (((x >> 3) + (y >> 3)) & 1) == 1;
                let bg = if light { [65, 74, 90] } else { [38, 45, 58] };
                let a = m.a[y * t + x];
                let i = (y * t + x) * 4;
                for c in 0..3 {
                    cp[i + c] = (bg[c] as f32 * (1.0 - a) + 255.0 * a).round() as u8;
                }
                cp[i + 3] = 255;
            }
        }
        self.composite_handle = Some(Handle::from_rgba(t as u32, t as u32, cp));
        let peak = m.peak();
        self.profile_meta = format!(
            "{t}×{t} · peak opacity {}% · colour white",
            (peak * 100.0).round() as i32
        );
    }

    // ─────────────────────────── view ───────────────────────────

    fn view(&self) -> Element<'_, Message> {
        let header = column![
            text("Watermark Remover").size(22),
            text("Clean-room, pure-Rust desktop edition. Ships with a Gemini-star profile measured from a real image; the Watermark tab can re-derive any mark from a batch of your own images.")
                .size(12)
                .style(muted),
        ]
        .spacing(4);

        let tabs = row![
            tab_btn("1 · Watermark", Tab::Watermark, self.tab),
            tab_btn("2 · Remove", Tab::Remove, self.tab),
            tab_btn("How it works", Tab::About, self.tab),
        ]
        .spacing(6);

        let content = match self.tab {
            Tab::Watermark => self.view_watermark(),
            Tab::Remove => self.view_remove(),
            Tab::About => view_about(),
        };

        column![
            header,
            tabs,
            scrollable(container(content).padding(4)).height(Length::Fill),
        ]
        .spacing(14)
        .padding(18)
        .into()
    }

    fn view_watermark(&self) -> Element<'_, Message> {
        let profile_panel = panel(
            column![
                row![
                    text("Active watermark profile").size(15),
                    badge(&self.profile_label, true),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
                text(&self.profile_explain).size(12).style(muted),
                row![
                    labeled("Opacity map — light = solid", opt_img(&self.alpha_handle)),
                    labeled(
                        "Composited over a checkerboard",
                        opt_img(&self.composite_handle)
                    ),
                ]
                .spacing(16),
                text(&self.profile_meta).size(12).style(muted),
                row![
                    button("Download profile JSON…").on_press(Message::SaveProfile),
                    button("Load profile JSON…").on_press(Message::LoadProfile),
                ]
                .spacing(10),
            ]
            .spacing(12),
        );

        let mut learn_btn = button("Learn & replace profile");
        if self.learn_imgs.len() >= 6 {
            learn_btn = learn_btn.on_press(Message::DoLearn);
        }

        let teach_panel = panel(
            column![
                row![text("Teach a different watermark").size(15), badge("optional", false)]
                    .spacing(10)
                    .align_y(Alignment::Center),
                text("The Gemini ✦ mark is already built in (above) — for Gemini images just use the Remove tab. Use this only to remove a different fixed watermark: add ~20–60 images that all carry the same mark in the same corner, and it is measured statistically (the watermark is the one thing common to every image, so picture content cancels out). No clean originals or labelling needed.")
                    .size(12)
                    .style(muted),
                button("Add watermarked images…").on_press(Message::AddLearnImages),
                row![
                    labeled_input(
                        "Corner the mark sits in",
                        pick_list(
                            CornerSel::LEARN.to_vec(),
                            Some(self.learn_corner),
                            Message::LearnCorner
                        )
                        .width(Length::Fixed(160.0))
                        .into()
                    ),
                    labeled_input(
                        "Watermark size",
                        pick_list(
                            SizeSel::ALL.to_vec(),
                            Some(self.learn_size),
                            Message::LearnSizeMsg
                        )
                        .width(Length::Fixed(240.0))
                        .into()
                    ),
                    learn_btn,
                ]
                .spacing(16)
                .align_y(Alignment::End),
                text(&self.learn_status).size(12).style(muted),
            ]
            .spacing(12),
        );

        column![profile_panel, teach_panel].spacing(16).into()
    }

    fn view_remove(&self) -> Element<'_, Message> {
        let sizes: Vec<String> = self.profile.maps.keys().map(|k| format!("{k}px")).collect();
        let active = if self.profile.maps.is_empty() {
            "none — set one on the Watermark tab".to_string()
        } else {
            format!("{} · {}", self.profile_label, sizes.join(", "))
        };

        let mut col = column![
            panel(
                row![text("Active profile:").size(13), badge(&active, !self.profile.maps.is_empty())]
                    .spacing(8)
                    .align_y(Alignment::Center)
            ),
            panel(
                row![
                    button("Open image…").on_press(Message::OpenImage),
                    text(&self.status).size(12).style(muted),
                ]
                .spacing(12)
                .align_y(Alignment::Center)
            ),
        ]
        .spacing(16);

        if self.src.is_some() {
            let controls = panel(
                column![
                    row![
                        labeled_input(
                            "Corner",
                            pick_list(
                                CornerSel::REMOVE.to_vec(),
                                Some(self.corner_sel),
                                Message::CornerChanged
                            )
                            .width(Length::Fixed(150.0))
                            .into()
                        ),
                        labeled_input(
                            "Nudge X",
                            text_input("0", &self.nudge_x)
                                .on_input(Message::NudgeX)
                                .width(Length::Fixed(70.0))
                                .into()
                        ),
                        labeled_input(
                            "Nudge Y",
                            text_input("0", &self.nudge_y)
                                .on_input(Message::NudgeY)
                                .width(Length::Fixed(70.0))
                                .into()
                        ),
                        labeled_input(
                            "Watermark colour",
                            text_input("#ffffff", &self.color_hex)
                                .on_input(Message::Color)
                                .width(Length::Fixed(100.0))
                                .into()
                        ),
                        labeled_input(
                            &format!("Opacity calibration  {}%", self.strength),
                            slider(20..=220, self.strength, Message::Strength)
                                .width(Length::Fixed(220.0))
                                .into()
                        ),
                    ]
                    .spacing(16)
                    .align_y(Alignment::End),
                    text(&self.det_info).size(12).style(muted),
                ]
                .spacing(10),
            );
            col = col.push(controls);

            let mut result_col = column![row![
                checkbox("outline", self.show_outline).on_toggle(Message::ToggleOutline),
                checkbox("original", self.show_before).on_toggle(Message::ToggleBefore),
                checkbox("zoom", self.show_zoom).on_toggle(Message::ToggleZoom),
                Space::with_width(Length::Fill),
                button("Save PNG").on_press(Message::Save),
            ]
            .spacing(16)
            .align_y(Alignment::Center)]
            .spacing(12);

            if let Some(h) = &self.result_handle {
                result_col = result_col.push(
                    container(
                        iced::widget::image(h.clone())
                            .width(Length::Fixed(self.result_disp.0))
                            .height(Length::Fixed(self.result_disp.1)),
                    )
                    .width(Length::Fill)
                    .align_x(Alignment::Center),
                );
            }
            col = col.push(panel(result_col));
        }

        col.into()
    }
}

// ─────────────────────────── small view helpers ───────────────────────────

fn view_about() -> Element<'static, Message> {
    panel(
        column![
            text("Why this is clean-room").size(15),
            text("Nothing here is taken from any watermark-removal product. The watermark profile is measured from your own images using a generic statistical idea:")
                .size(12)
                .style(muted),
            text("• Learning. The mark is a fixed, mostly-white overlay added the same way to every image: observed = original·(1−α) + 255·α. Across many varied images the darkest observed values at each watermark pixel occur where the picture is dark, so the low percentile of observed luminance ≈ 255·α. A low percentile per pixel (minus the surrounding background) recovers α directly; picture content cancels out.")
                .size(12)
                .style(muted),
            text("• Removal. With α known, the composite is reversed exactly: original = (observed − 255·α)/(1−α). The watermark's position in each new image is found by correlating the learned α-shape against the local brightening (NCC).")
                .size(12)
                .style(muted),
            text("Everything runs locally — no image ever leaves your machine. Give it 20+ varied images and the recovered watermark is essentially identical to the true one.")
                .size(12)
                .style(muted),
        ]
        .spacing(12),
    )
}

fn tab_btn(label: &'static str, which: Tab, active: Tab) -> Element<'static, Message> {
    let b = button(text(label)).on_press(Message::Tab(which)).padding([8, 14]);
    if which == active {
        b.style(button::primary).into()
    } else {
        b.style(button::secondary).into()
    }
}

/// A label above a widget (a small column).
fn labeled<'a>(label: &'a str, content: Element<'a, Message>) -> Element<'a, Message> {
    column![text(label).size(11).style(muted), content]
        .spacing(4)
        .into()
}

fn labeled_input<'a>(label: &str, content: Element<'a, Message>) -> Element<'a, Message> {
    column![text(label.to_string()).size(11).style(muted), content]
        .spacing(4)
        .into()
}

fn opt_img(h: &Option<Handle>) -> Element<'static, Message> {
    match h {
        Some(h) => iced::widget::image(h.clone())
            .width(Length::Fixed(150.0))
            .height(Length::Fixed(150.0))
            .filter_method(FilterMethod::Nearest)
            .into(),
        None => text("(no profile)").size(12).style(muted).into(),
    }
}

fn badge(label: &str, ok: bool) -> Element<'static, Message> {
    let color = if ok {
        Color::from_rgb8(0x22, 0xc5, 0x5e)
    } else {
        Color::from_rgb8(0xf5, 0x9e, 0x0b)
    };
    container(text(label.to_string()).size(11).style(move |_t: &Theme| text::Style {
        color: Some(color),
    }))
    .padding([2, 8])
    .style(move |_t: &Theme| container::Style {
        border: Border {
            color,
            width: 1.0,
            radius: 99.0.into(),
        },
        ..Default::default()
    })
    .into()
}

fn panel<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content)
        .padding(16)
        .width(Length::Fill)
        .style(panel_style)
        .into()
}

fn panel_style(_t: &Theme) -> container::Style {
    container::Style {
        background: Some(Color::from_rgb8(0x16, 0x1b, 0x22).into()),
        border: Border {
            color: Color::from_rgb8(0x2b, 0x32, 0x40),
            width: 1.0,
            radius: 10.0.into(),
        },
        ..Default::default()
    }
}

fn muted(_t: &Theme) -> text::Style {
    text::Style {
        color: Some(Color::from_rgb8(0x9a, 0xa6, 0xb2)),
    }
}

// ─────────────────────────── pixel helpers ───────────────────────────

fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    Some([
        u8::from_str_radix(&s[0..2], 16).ok()?,
        u8::from_str_radix(&s[2..4], 16).ok()?,
        u8::from_str_radix(&s[4..6], 16).ok()?,
    ])
}

fn set_px(px: &mut [u8], w: usize, h: usize, x: i64, y: i64, c: [u8; 3]) {
    if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
        return;
    }
    let i = ((y as usize) * w + x as usize) * 4;
    px[i] = c[0];
    px[i + 1] = c[1];
    px[i + 2] = c[2];
    px[i + 3] = 255;
}

fn draw_rect(px: &mut [u8], w: usize, h: usize, x: i64, y: i64, size: i64, c: [u8; 3]) {
    for d in 0..2i64 {
        for xx in x..x + size {
            set_px(px, w, h, xx, y + d, c);
            set_px(px, w, h, xx, y + size - 1 - d, c);
        }
        for yy in y..y + size {
            set_px(px, w, h, x + d, yy, c);
            set_px(px, w, h, x + size - 1 - d, yy, c);
        }
    }
}

/// Nearest-neighbour downscale so big images don't blow up the GPU upload
/// (the full-resolution result is still what gets saved).
fn fit_rgba(src: &[u8], w: usize, h: usize, max: usize) -> (usize, usize, Vec<u8>) {
    if w.max(h) <= max {
        return (w, h, src.to_vec());
    }
    let scale = max as f32 / w.max(h) as f32;
    let pw = ((w as f32 * scale).round() as usize).max(1);
    let ph = ((h as f32 * scale).round() as usize).max(1);
    let mut out = vec![0u8; pw * ph * 4];
    for y in 0..ph {
        let sy = (y * h / ph).min(h - 1);
        for x in 0..pw {
            let sx = (x * w / pw).min(w - 1);
            let si = (sy * w + sx) * 4;
            let di = (y * pw + x) * 4;
            out[di..di + 4].copy_from_slice(&src[si..si + 4]);
        }
    }
    (pw, ph, out)
}

fn display_size(w: usize, h: usize, maxdim: f32) -> (f32, f32) {
    let s = maxdim / w.max(h) as f32;
    (w as f32 * s, h as f32 * s)
}

fn strip_ext(name: &str) -> String {
    match name.rfind('.') {
        Some(i) => name[..i].to_string(),
        None => name.to_string(),
    }
}

// ─────────────────────────── persistence ───────────────────────────

fn config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("watermark-remover"))
}

fn persist_profile(p: &Profile, strength: i32) {
    if let Some(d) = config_dir() {
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(d.join("profile.json"), p.to_json(strength as u32));
    }
}

fn load_persisted_profile() -> Option<(Profile, Option<u32>)> {
    let d = config_dir()?;
    let t = std::fs::read_to_string(d.join("profile.json")).ok()?;
    Profile::from_json(&t).ok()
}

fn save_strength(v: i32) {
    if let Some(d) = config_dir() {
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(d.join("strength.txt"), v.to_string());
    }
}

fn load_strength() -> Option<i32> {
    let d = config_dir()?;
    std::fs::read_to_string(d.join("strength.txt"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

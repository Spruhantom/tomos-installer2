//! The installer window: a sidebar with the steps and one page per step.

mod style;

use crate::backend;
use crate::catalog::{self, Choice, Layout};
use crate::disks::{self, Disk};
use crate::events::Event;
use crate::plan::{self, Filesystem, InstallPlan, Storage, User, MIN_TARGET_BYTES};
use gtk::prelude::*;
use gtk::{cairo, gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

const WALLPAPERS: &[&str] = &[
    "/usr/share/backgrounds/tomos/wallpaper-eclipse.png",
    "/usr/share/backgrounds/tomos/wallpaper-eclipse-1080.png",
    "/usr/share/backgrounds/tomos/wallpaper.png",
];
const LOGOS: &[&str] = &["/usr/share/icons/hicolor/256x256/apps/tomos.png", "/usr/share/pixmaps/tomos.png"];

const STEPS: &[&str] = &["Welcome", "Language & Region", "Keyboard", "Disk", "You", "Summary", "Install"];
const P_WELCOME: usize = 0;
const P_LANG: usize = 1;
const P_KEYS: usize = 2;
const P_DISK: usize = 3;
const P_USER: usize = 4;
const P_SUMMARY: usize = 5;
const P_INSTALL: usize = 6;
const P_DONE: usize = 7;

const TIPS: &[&str] = &[
    "SUPER + Enter opens kitty, the Tom OS terminal.",
    "SUPER + W opens WordDocEdit for documents, with Word and PDF export.",
    "SUPER + D (or SUPER + Space) opens the app launcher.",
    "Your keyboard shortcuts are in Welcome → Keyboard Shortcuts (PDF).",
    "The neon Quickshell bar shows Wi‑Fi, volume, battery and the clock.",
    "TDM, the Tom OS login screen, remembers who signed in last.",
    "Ventoy is included: make multi-boot USB sticks from the app menu.",
    "Firmware for most Wi‑Fi, graphics and Bluetooth chips is already installed.",
];

/// Options from the command line.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Sample disks, prefilled answers and a simulated install. Nothing is touched.
    pub demo: bool,
    /// Open on this page (for screenshots in demo mode).
    pub page: Option<usize>,
    /// Install from this squashfs or directory instead of the live medium.
    pub source: Option<String>,
    /// Offer loop devices (testing).
    pub allow_loop: bool,
    /// Demo mode: make the simulated install fail, to show the error page.
    pub demo_fail: bool,
}

struct Ui {
    opts: Options,
    window: gtk::ApplicationWindow,
    stack: gtk::Stack,
    steps: Vec<gtk::Box>,
    back: gtk::Button,
    next: gtk::Button,
    nav: gtk::Box,
    error: gtk::Label,
    page: Cell<usize>,
    plan: RefCell<InstallPlan>,
    installing: Cell<bool>,
    uefi: bool,
    // Language & region
    locales: Vec<Choice>,
    locale_dd: gtk::DropDown,
    zones: Vec<String>,
    regions: Vec<String>,
    region_dd: gtk::DropDown,
    city_dd: gtk::DropDown,
    cities: RefCell<Vec<String>>,
    clock: gtk::Label,
    // Keyboard
    layouts: Vec<Layout>,
    layout_list: gtk::ListBox,
    variant_dd: gtk::DropDown,
    // Disk
    disks: RefCell<Vec<Disk>>,
    disk_list: gtk::ListBox,
    erase_opt: gtk::CheckButton,
    manual_opt: gtk::CheckButton,
    erase_box: gtk::Box,
    manual_box: gtk::Box,
    manual_box_frame: gtk::Box,
    part_dd: gtk::DropDown,
    part_paths: RefCell<Vec<String>>,
    esp_dd: gtk::DropDown,
    esp_paths: RefCell<Vec<Option<String>>>,
    fs_dd: gtk::DropDown,
    swap: gtk::SpinButton,
    preview: gtk::DrawingArea,
    // You
    full_name: gtk::Entry,
    username: gtk::Entry,
    hostname: gtk::Entry,
    password: gtk::PasswordEntry,
    confirm: gtk::PasswordEntry,
    strength: gtk::LevelBar,
    strength_label: gtk::Label,
    autologin: gtk::Switch,
    username_edited: Cell<bool>,
    hostname_edited: Cell<bool>,
    setting: Cell<bool>,
    // Summary
    summary: gtk::Box,
    // Install
    step_label: gtk::Label,
    percent: gtk::Label,
    progress: gtk::ProgressBar,
    tip: gtk::Label,
    log: gtk::TextView,
    log_scroll: gtk::ScrolledWindow,
    // Done
    done_title: gtk::Label,
    done_text: gtk::Label,
    done_icon: gtk::Label,
    restart: gtk::Button,
    close: gtk::Button,
}

fn label(text: &str, class: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    if !class.is_empty() {
        for c in class.split_whitespace() {
            l.add_css_class(c);
        }
    }
    l
}

fn wrap(l: gtk::Label) -> gtk::Label {
    l.set_wrap(true);
    l.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    l
}

fn vbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}

fn hbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Horizontal, spacing)
}

/// The first path that exists; `TOMOS_DATA_ROOT` (a Tom OS rootfs) is tried first, for demos.
fn first_existing(paths: &[&str]) -> Option<String> {
    if let Some(root) = std::env::var_os("TOMOS_DATA_ROOT") {
        for p in paths {
            let full = Path::new(&root).join(p.trim_start_matches('/'));
            if full.exists() {
                return Some(full.to_string_lossy().into_owned());
            }
        }
    }
    paths.iter().find(|p| Path::new(p).exists()).map(|s| s.to_string())
}

fn page_header(title: &str, subtitle: &str) -> gtk::Box {
    let b = vbox(6);
    b.append(&label(title, "ti-title"));
    b.append(&wrap(label(subtitle, "ti-subtitle")));
    b.set_margin_bottom(18);
    b
}

fn field(name: &str, widget: &impl IsA<gtk::Widget>) -> gtk::Box {
    let b = vbox(6);
    b.append(&label(name, "ti-label"));
    b.append(widget);
    b
}

fn string_dropdown(items: &[String], search: bool) -> gtk::DropDown {
    let refs: Vec<&str> = items.iter().map(|s| s.as_str()).collect();
    let dd = gtk::DropDown::from_strings(&refs);
    if search {
        dd.set_enable_search(true);
        dd.set_expression(Some(gtk::PropertyExpression::new(gtk::StringObject::static_type(), None::<&gtk::Expression>, "string")));
    }
    dd
}

/// Lets a dropdown shrink: long items are cut with "…" instead of widening the window.
fn ellipsize(dd: &gtk::DropDown) {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let l = gtk::Label::new(None);
        l.set_xalign(0.0);
        l.set_ellipsize(gtk::pango::EllipsizeMode::End);
        l.set_width_chars(10);
        l.set_max_width_chars(40);
        item.downcast_ref::<gtk::ListItem>().unwrap().set_child(Some(&l));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        if let (Some(l), Some(s)) = (item.child().and_downcast::<gtk::Label>(), item.item().and_downcast::<gtk::StringObject>()) {
            l.set_text(&s.string());
        }
    });
    dd.set_factory(Some(&factory));
    dd.set_hexpand(true);
}

fn set_dropdown_items(dd: &gtk::DropDown, items: &[String]) {
    let refs: Vec<&str> = items.iter().map(|s| s.as_str()).collect();
    dd.set_model(Some(&gtk::StringList::new(&refs)));
}

fn mem_total() -> u64 {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("MemTotal:")).and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok()))
        .map(|kb| kb * 1024)
        .unwrap_or(0)
}

/// (on battery?, has battery)
fn power_state() -> (bool, bool) {
    let Ok(dir) = std::fs::read_dir("/sys/class/power_supply") else { return (false, false) };
    let mut has_battery = false;
    let mut on_ac = false;
    for e in dir.flatten() {
        let p = e.path();
        let kind = std::fs::read_to_string(p.join("type")).unwrap_or_default();
        match kind.trim() {
            "Battery" => has_battery = true,
            "Mains" | "USB" => {
                if std::fs::read_to_string(p.join("online")).map(|s| s.trim() == "1").unwrap_or(false) {
                    on_ac = true;
                }
            }
            _ => {}
        }
    }
    (has_battery && !on_ac, has_battery)
}

impl Ui {
    // ---------------------------------------------------------------------------------------
    // Navigation
    // ---------------------------------------------------------------------------------------

    fn show_page(self: &Rc<Self>, n: usize) {
        self.page.set(n);
        self.stack.set_visible_child_name(&format!("p{n}"));
        let side = n.min(P_INSTALL);
        for (i, s) in self.steps.iter().enumerate() {
            s.remove_css_class("active");
            s.remove_css_class("done");
            if i == side {
                s.add_css_class("active");
            } else if i < side || n == P_DONE {
                s.add_css_class("done");
            }
        }
        self.error.set_text("");
        self.back.set_sensitive(n > P_WELCOME);
        self.nav.set_visible(n < P_INSTALL);
        self.next.remove_css_class("ti-danger");
        self.next.add_css_class("ti-primary");
        if n == P_SUMMARY {
            self.next.set_label("_INSTALL");
            if matches!(self.plan.borrow().storage, Storage::Erase { .. }) {
                self.next.remove_css_class("ti-primary");
                self.next.add_css_class("ti-danger");
            }
            self.fill_summary();
        } else {
            self.next.set_label("_NEXT");
        }
        if n == P_USER {
            self.full_name.grab_focus();
        }
        if n == P_KEYS {
            // Scroll the layout list so the chosen layout is visible.
            let list = self.layout_list.clone();
            glib::timeout_add_local_once(Duration::from_millis(300), move || {
                if let Some(row) = list.selected_row() {
                    let y = row.compute_point(&list, &gtk::graphene::Point::new(0.0, 0.0)).map(|p| p.y() as f64).unwrap_or(0.0);
                    if let Some(sc) = list.ancestor(gtk::ScrolledWindow::static_type()).and_downcast::<gtk::ScrolledWindow>() {
                        let adj = sc.vadjustment();
                        adj.set_value((y - adj.page_size() / 2.0).max(0.0));
                    }
                    row.grab_focus();
                }
            });
        }
    }

    fn go_next(self: &Rc<Self>) {
        let n = self.page.get();
        let r = match n {
            P_LANG => self.read_language(),
            P_KEYS => self.read_keyboard(),
            P_DISK => self.read_disk(),
            P_USER => self.read_user(),
            _ => Ok(()),
        };
        if let Err(e) = r {
            self.error.set_text(&e);
            return;
        }
        if n == P_SUMMARY {
            self.confirm_install();
        } else if n < P_SUMMARY {
            self.show_page(n + 1);
        }
    }

    // ---------------------------------------------------------------------------------------
    // Reading the answers into the plan
    // ---------------------------------------------------------------------------------------

    fn selected_zone(&self) -> String {
        let region = self.regions.get(self.region_dd.selected() as usize).cloned().unwrap_or_default();
        let cities = self.cities.borrow();
        match cities.get(self.city_dd.selected() as usize) {
            Some(c) if !c.is_empty() => format!("{region}/{c}"),
            _ => region,
        }
    }

    fn read_language(&self) -> Result<(), String> {
        let loc = self.locales.get(self.locale_dd.selected() as usize).ok_or("Choose a language.")?;
        let tz = self.selected_zone();
        plan::check_timezone(&tz)?;
        let mut p = self.plan.borrow_mut();
        p.locale = loc.id.clone();
        p.timezone = tz;
        Ok(())
    }

    fn selected_layout(&self) -> Option<&Layout> {
        let row = self.layout_list.selected_row()?;
        self.layouts.get(row.index() as usize)
    }

    fn read_keyboard(&self) -> Result<(), String> {
        let layout = self.selected_layout().ok_or("Choose a keyboard layout.")?;
        let v = self.variant_dd.selected() as usize;
        let variant = if v == 0 { String::new() } else { layout.variants.get(v - 1).map(|c| c.id.clone()).unwrap_or_default() };
        let mut p = self.plan.borrow_mut();
        p.keyboard.layout = layout.id.clone();
        p.keyboard.variant = variant;
        Ok(())
    }

    fn selected_disk(&self) -> Option<Disk> {
        let row = self.disk_list.selected_row()?;
        self.disks.borrow().get(row.index() as usize).cloned()
    }

    fn read_disk(&self) -> Result<(), String> {
        let disk = self.selected_disk().ok_or("Choose the disk to install Tom OS on.")?;
        if let Some(why) = disk.unusable_reason() {
            return Err(why);
        }
        let storage = if self.erase_opt.is_active() {
            Storage::Erase { disk: disk.path.clone() }
        } else {
            let root = self
                .part_paths
                .borrow()
                .get(self.part_dd.selected() as usize)
                .cloned()
                .ok_or("Choose the partition for Tom OS (it needs at least 12 GB).")?;
            let esp = self.esp_paths.borrow().get(self.esp_dd.selected() as usize).cloned().flatten();
            if self.uefi && esp.is_none() && !self.opts.demo {
                return Err("This computer starts in UEFI mode: choose the EFI system partition too.".into());
            }
            Storage::Manual { root, esp, disk: disk.path.clone() }
        };
        let mut p = self.plan.borrow_mut();
        p.storage = storage;
        p.filesystem = if self.fs_dd.selected() == 1 { Filesystem::Btrfs } else { Filesystem::Ext4 };
        p.swap_mib = (self.swap.value() as u32) * 1024;
        Ok(())
    }

    fn read_user(&self) -> Result<(), String> {
        let full = self.full_name.text().trim().to_string();
        if full.is_empty() {
            return Err("Enter your name.".into());
        }
        if full.contains([':', ',', '\n']) {
            return Err("Your name can't contain : or ,".into());
        }
        let user = self.username.text().to_string();
        plan::check_username(&user)?;
        let host = self.hostname.text().to_string();
        plan::check_hostname(&host)?;
        plan::check_password(&self.password.text(), &self.confirm.text())?;
        let mut p = self.plan.borrow_mut();
        p.user = User { full_name: full, username: user, password: self.password.text().to_string(), autologin: self.autologin.is_active() };
        p.hostname = host;
        Ok(())
    }

    // ---------------------------------------------------------------------------------------
    // Disk page helpers
    // ---------------------------------------------------------------------------------------

    fn refresh_disk_choices(self: &Rc<Self>) {
        let disk = self.selected_disk();
        let mut parts = Vec::new();
        let mut part_names = Vec::new();
        let mut esps: Vec<Option<String>> = vec![None];
        let mut esp_names = vec!["None — no EFI boot entry".to_string()];
        if let Some(d) = &disk {
            for p in &d.partitions {
                let desc = format!(
                    "{}  ·  {}  ·  {}{}",
                    p.path,
                    disks::human_size(p.size),
                    if p.fstype.is_empty() { "unformatted" } else { &p.fstype },
                    if p.label.is_empty() { String::new() } else { format!(" \"{}\"", p.label) }
                );
                if p.is_esp() {
                    esps.push(Some(p.path.clone()));
                    esp_names.push(desc.clone());
                } else if p.size >= MIN_TARGET_BYTES && !p.is_bios_boot() {
                    parts.push(p.path.clone());
                    part_names.push(desc);
                }
            }
        }
        if part_names.is_empty() {
            part_names.push("No partition of 12 GB or more on this disk".into());
        }
        set_dropdown_items(&self.part_dd, &part_names);
        set_dropdown_items(&self.esp_dd, &esp_names);
        if esps.len() > 1 {
            self.esp_dd.set_selected(1);
        }
        let has_parts = !parts.is_empty();
        *self.part_paths.borrow_mut() = parts;
        *self.esp_paths.borrow_mut() = esps;
        self.manual_opt.set_sensitive(has_parts);
        if !has_parts {
            self.erase_opt.set_active(true);
        }
        self.update_mode();
    }

    fn update_mode(&self) {
        let erase = self.erase_opt.is_active();
        self.manual_box.set_visible(!erase);
        for (b, on) in [(&self.erase_box, erase), (&self.manual_box_frame, !erase)] {
            if on { b.add_css_class("selected") } else { b.remove_css_class("selected") }
        }
        self.preview.queue_draw();
    }

    /// Draws the disk as a bar: what it will look like after installing.
    fn draw_preview(&self, cr: &cairo::Context, w: i32, h: i32) {
        let (w, h) = (w as f64, h as f64);
        let Some(disk) = self.selected_disk() else { return };
        let bar_h = 34.0;
        let y = 4.0;
        // (fraction, colour, label, highlighted)
        let mut segs: Vec<(f64, &str, String, bool)> = Vec::new();
        let total = disk.size.max(1) as f64;
        if self.erase_opt.is_active() {
            segs.push(((512.0 * 1024.0 * 1024.0) / total, style::CYAN, "EFI 512 MB".into(), false));
            segs.push((1.0, style::PINK, format!("Tom OS  {}", disks::human_size(disk.size)), true));
        } else {
            let chosen = self.part_paths.borrow().get(self.part_dd.selected() as usize).cloned();
            for p in &disk.partitions {
                let hl = Some(&p.path) == chosen.as_ref();
                let colour = if hl { style::PINK } else if p.is_esp() { style::CYAN } else { style::PURPLE };
                let name = if hl {
                    format!("Tom OS  {}", disks::human_size(p.size))
                } else if !p.label.is_empty() {
                    format!("{}  {}", p.label, disks::human_size(p.size))
                } else {
                    format!("{}  {}", if p.fstype.is_empty() { "—" } else { &p.fstype }, disks::human_size(p.size))
                };
                segs.push((p.size as f64 / total, colour, name, hl));
            }
            let used: u64 = disk.partitions.iter().map(|p| p.size).sum();
            if disk.size > used + 64 * 1024 * 1024 {
                segs.push(((disk.size - used) as f64 / total, style::MUTED, "free".into(), false));
            }
        }
        // Minimum visible width per segment, then scale the rest.
        let min_w = 96.0;
        let n = segs.len() as f64;
        let free_w = (w - 4.0 - min_w * n - 4.0 * (n - 1.0)).max(0.0);
        let sum: f64 = segs.iter().map(|s| s.0.min(1.0)).sum::<f64>().max(1e-9);
        let mut x = 2.0;
        cr.select_font_face("JetBrainsMono Nerd Font", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
        cr.set_font_size(11.0);
        for (frac, colour, name, hl) in &segs {
            let sw = min_w + free_w * frac.min(1.0) / sum;
            let (r, g, b) = style::rgb(colour);
            rounded(cr, x, y, sw, bar_h, 8.0);
            cr.set_source_rgba(r, g, b, if *hl { 0.85 } else { 0.35 });
            let _ = cr.fill_preserve();
            cr.set_source_rgba(r, g, b, 1.0);
            cr.set_line_width(if *hl { 2.0 } else { 1.0 });
            let _ = cr.stroke();
            let (fr, fg, fb) = if *hl { style::rgb(style::BG) } else { style::rgb(style::FG) };
            cr.set_source_rgb(fr, fg, fb);
            let mut text = name.clone();
            while text.chars().count() > 3 {
                let ext = cr.text_extents(&text).map(|e| e.width()).unwrap_or(0.0);
                if ext <= sw - 14.0 {
                    break;
                }
                text = text.chars().take(text.chars().count() - 2).collect::<String>();
                text = format!("{}…", text.trim_end_matches('…'));
            }
            let tw = cr.text_extents(&text).map(|e| e.width()).unwrap_or(0.0);
            cr.move_to(x + (sw - tw) / 2.0, y + bar_h / 2.0 + 4.0);
            let _ = cr.show_text(&text);
            x += sw + 4.0;
        }
        let _ = h;
    }

    // ---------------------------------------------------------------------------------------
    // Summary & install
    // ---------------------------------------------------------------------------------------

    fn fill_summary(&self) {
        while let Some(c) = self.summary.first_child() {
            self.summary.remove(&c);
        }
        let p = self.plan.borrow();
        let disks = self.disks.borrow();
        let disk_name = |path: &str| disks.iter().find(|d| d.path == path).map(|d| d.title()).unwrap_or_else(|| path.to_string());
        let loc = self.locales.iter().find(|c| c.id == p.locale).map(|c| c.name.clone()).unwrap_or(p.locale.clone());
        let kb = self
            .layouts
            .iter()
            .find(|l| l.id == p.keyboard.layout)
            .map(|l| {
                let v = l.variants.iter().find(|v| v.id == p.keyboard.variant).map(|v| format!(" — {}", v.name)).unwrap_or_default();
                format!("{}{v}", l.name)
            })
            .unwrap_or(p.keyboard.layout.clone());
        let (disk_line, action) = match &p.storage {
            Storage::Erase { disk } => (format!("{}  ({disk})", disk_name(disk)), "Erase the whole disk and install Tom OS".to_string()),
            Storage::Manual { root, esp, disk } => (
                format!("{}  ({disk})", disk_name(disk)),
                format!(
                    "Format {root} and install Tom OS there{}",
                    esp.as_ref().map(|e| format!("; boot files on {e} (kept)")).unwrap_or_default()
                ),
            ),
        };
        if let Storage::Erase { disk } = &p.storage {
            let warn = hbox(12);
            warn.add_css_class("ti-warnbox");
            warn.append(&label("\u{f071}", "ti-error"));
            let t = vbox(2);
            t.append(&label("EVERYTHING ON THIS DISK WILL BE ERASED", "ti-error"));
            t.append(&wrap(label(&format!("{} — all partitions, files and other operating systems on it are deleted.", disk_name(disk)), "ti-text")));
            warn.append(&t);
            self.summary.append(&warn);
        }
        let grid = gtk::Grid::new();
        grid.add_css_class("ti-summary");
        grid.set_row_spacing(10);
        grid.set_column_spacing(24);
        let rows: Vec<(&str, String)> = vec![
            ("LANGUAGE", loc),
            ("TIME ZONE", p.timezone.replace('_', " ")),
            ("KEYBOARD", kb),
            ("DISK", disk_line),
            ("WHAT HAPPENS", action),
            (
                "FILE SYSTEM",
                match p.filesystem {
                    Filesystem::Ext4 => "ext4".to_string(),
                    Filesystem::Btrfs => "btrfs (subvolumes @ and @home, zstd compression)".to_string(),
                },
            ),
            ("SWAP", if p.swap_mib == 0 { "None".into() } else { format!("{} GB swap file", p.swap_mib / 1024) }),
            ("COMPUTER NAME", p.hostname.clone()),
            (
                "ACCOUNT",
                format!("{} ({}){}", p.user.full_name, p.user.username, if p.user.autologin { " · signs in automatically" } else { "" }),
            ),
        ];
        for (i, (k, v)) in rows.iter().enumerate() {
            let kl = label(k, "key");
            kl.set_valign(gtk::Align::Start);
            grid.attach(&kl, 0, i as i32, 1, 1);
            let vl = wrap(label(v, "value"));
            vl.set_hexpand(true);
            grid.attach(&vl, 1, i as i32, 1, 1);
        }
        self.summary.append(&grid);
        self.summary.append(&wrap(label(
            "Nothing has been changed yet. Press INSTALL to start; it takes about 5–15 minutes.",
            "ti-muted",
        )));
    }

    fn confirm_install(self: &Rc<Self>) {
        let p = self.plan.borrow().clone();
        let (msg, detail, button) = match &p.storage {
            Storage::Erase { disk } => (
                "Erase the disk and install Tom OS?".to_string(),
                format!("Everything on {disk} will be deleted. This can't be undone."),
                "Erase and install",
            ),
            Storage::Manual { root, .. } => (
                "Format the partition and install Tom OS?".to_string(),
                format!("Everything on {root} will be deleted. Other partitions are kept."),
                "Format and install",
            ),
        };
        let dialog = gtk::AlertDialog::builder().modal(true).message(msg).detail(detail).buttons(["Cancel", button]).cancel_button(0).default_button(0).build();
        let ui = self.clone();
        dialog.choose(Some(&self.window), None::<&gio::Cancellable>, move |res| {
            if let Ok(1) = res {
                ui.start_install();
            }
        });
    }

    fn append_log(&self, text: &str) {
        let buf = self.log.buffer();
        let mut end = buf.end_iter();
        buf.insert(&mut end, text);
        buf.insert(&mut end, "\n");
        if buf.line_count() > 4000 {
            let mut s = buf.start_iter();
            let mut e = buf.iter_at_line(500).unwrap_or(buf.start_iter());
            buf.delete(&mut s, &mut e);
        }
        let adj = self.log_scroll.vadjustment();
        let a = adj.clone();
        glib::idle_add_local_once(move || a.set_value(a.upper() - a.page_size()));
    }

    fn start_install(self: &Rc<Self>) {
        self.installing.set(true);
        self.show_page(P_INSTALL);
        let (tx, rx) = mpsc::channel::<Event>();
        let plan = self.plan.borrow().clone();
        if self.opts.demo {
            let fail = self.opts.demo_fail;
            std::thread::spawn(move || {
                let steps = backend::build_steps(&plan, &backend::demo_context());
                let mut runner = backend::SimRunner { delay_ms: if fail { 20 } else { 110 }, fail_on: fail.then(|| "grub-install".to_string()) };
                let _ = backend::execute(&steps, &mut runner, &mut |e| {
                    let _ = tx.send(e);
                }, backend::TARGET, false);
            });
        } else {
            spawn_backend(plan, self.opts.allow_loop, tx);
        }
        // Rotating tips.
        let ui = self.clone();
        let tip_i = Cell::new(0usize);
        glib::timeout_add_local(Duration::from_secs(8), move || {
            if !ui.installing.get() {
                return glib::ControlFlow::Break;
            }
            tip_i.set((tip_i.get() + 1) % TIPS.len());
            ui.tip.set_text(TIPS[tip_i.get()]);
            glib::ControlFlow::Continue
        });
        let ui = self.clone();
        glib::timeout_add_local(Duration::from_millis(80), move || {
            loop {
                match rx.try_recv() {
                    Ok(Event::Step { index, total, title }) => ui.step_label.set_text(&format!("Step {} of {total}  ·  {title}", index + 1)),
                    Ok(Event::Progress { value }) => {
                        ui.progress.set_fraction(value);
                        ui.percent.set_text(&format!("{:.0}%", value * 100.0));
                    }
                    Ok(Event::Log { text }) => ui.append_log(&text),
                    Ok(Event::Done) => {
                        ui.finish(None);
                        return glib::ControlFlow::Break;
                    }
                    Ok(Event::Error { text }) => {
                        ui.finish(Some(text));
                        return glib::ControlFlow::Break;
                    }
                    Err(mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        ui.finish(Some("The installer backend stopped unexpectedly. See the details log.".into()));
                        return glib::ControlFlow::Break;
                    }
                }
            }
        });
    }

    fn finish(self: &Rc<Self>, error: Option<String>) {
        self.installing.set(false);
        match error {
            None => {
                self.done_icon.set_text("\u{f05d}");
                self.done_icon.set_css_classes(&["ti-done-icon", "ti-ok"]);
                self.done_title.set_text("TOM OS IS INSTALLED");
                self.done_text.set_text(&format!(
                    "Restart the computer and remove the USB stick when the screen goes dark. Sign in as {} on the TDM login screen.",
                    self.plan.borrow().user.username
                ));
                self.restart.set_visible(true);
                self.close.set_label("KEEP EXPLORING");
            }
            Some(e) => {
                self.done_icon.set_text("\u{f057}");
                self.done_icon.set_css_classes(&["ti-done-icon", "ti-error"]);
                self.done_title.set_text("INSTALLATION FAILED");
                self.done_text.set_text(&format!(
                    "{e}\n\nYour live session still works. The full log is in {} — you can try again or ask for help with it.",
                    backend::LOG_FILE
                ));
                self.restart.set_visible(false);
                self.close.set_label("CLOSE");
            }
        }
        self.show_page(P_DONE);
    }
}

fn rounded(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -std::f64::consts::FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, std::f64::consts::FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, std::f64::consts::FRAC_PI_2, std::f64::consts::PI);
    cr.arc(x + r, y + r, r, std::f64::consts::PI, 3.0 * std::f64::consts::FRAC_PI_2);
    cr.close_path();
}

/// Starts `tomos-installer backend` as root and forwards its events.
fn spawn_backend(plan: InstallPlan, allow_loop: bool, tx: mpsc::Sender<Event>) {
    std::thread::spawn(move || {
        let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "tomos-installer".into());
        let mut args = vec![exe, "backend".to_string()];
        if allow_loop {
            args.push("--allow-loop".into());
        }
        let as_root = backend::euid() == Some(0);
        let sudo_ok = !as_root
            && Command::new("sudo").args(["-n", "true"]).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
        let mut cmd = if as_root {
            let mut c = Command::new(&args[0]);
            c.args(&args[1..]);
            c
        } else if sudo_ok {
            let mut c = Command::new("sudo");
            c.arg("-n").args(&args);
            c
        } else {
            let mut c = Command::new("pkexec");
            c.args(&args);
            c
        };
        let _ = tx.send(Event::Log { text: format!("Starting the backend ({})", if as_root { "root" } else if sudo_ok { "sudo" } else { "pkexec" }) });
        let mut child = match cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(Event::Error { text: format!("Could not start the installer as administrator: {e}") });
                return;
            }
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(serde_json::to_string(&plan).unwrap_or_default().as_bytes());
        }
        let stderr = child.stderr.take();
        let tx2 = tx.clone();
        let err_thread = std::thread::spawn(move || {
            if let Some(e) = stderr {
                for line in BufReader::new(e).lines().map_while(Result::ok) {
                    let _ = tx2.send(Event::Log { text: line });
                }
            }
        });
        let mut finished = false;
        if let Some(out) = child.stdout.take() {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                match Event::from_line(&line) {
                    Some(e) => {
                        if matches!(e, Event::Done | Event::Error { .. }) {
                            finished = true;
                        }
                        let _ = tx.send(e);
                    }
                    None => {
                        let _ = tx.send(Event::Log { text: line });
                    }
                }
            }
        }
        let status = child.wait();
        let _ = err_thread.join();
        if !finished {
            let text = match status {
                Ok(s) if s.code() == Some(126) || s.code() == Some(127) => "Administrator access was refused.".to_string(),
                Ok(s) => format!("The installer backend stopped unexpectedly ({s})."),
                Err(e) => format!("The installer backend failed: {e}"),
            };
            let _ = tx.send(Event::Error { text });
        }
    });
}

// ===========================================================================================
// Building the window
// ===========================================================================================

fn check_row(icon: &str, class: &str, title: &str, detail: &str) -> gtk::Box {
    let row = hbox(12);
    row.add_css_class("ti-check");
    let i = label(icon, &format!("icon {class}"));
    row.append(&i);
    let t = vbox(2);
    t.append(&label(title, "ti-big"));
    t.append(&wrap(label(detail, "ti-muted")));
    row.append(&t);
    row
}

fn welcome_page(opts: &Options, uefi: bool, disks: &[Disk]) -> gtk::Box {
    let page = vbox(10);
    let top = hbox(22);
    if let Some(logo) = first_existing(LOGOS) {
        let img = gtk::Image::from_file(logo);
        img.set_pixel_size(96);
        top.append(&img);
    }
    let head = page_header("WELCOME TO TOM OS", "This will install Tom OS — Hyprland, kitty, Quickshell, TDM and WordDocEdit — on this computer.");
    head.set_hexpand(true);
    top.append(&head);
    page.append(&top);
    page.append(&label("BEFORE YOU START", "ti-label"));
    let mem = if opts.demo { 8 * 1024 * 1024 * 1024 } else { mem_total() };
    let gib = mem as f64 / 1024f64.powi(3);
    page.append(&if gib >= 3.5 {
        check_row("\u{f00c}", "ti-ok", &format!("{gib:.0} GB memory"), "Plenty for Tom OS (2 GB minimum, 4 GB recommended).")
    } else if gib >= 1.8 {
        check_row("\u{f071}", "ti-warn", &format!("{gib:.1} GB memory"), "Enough to install; 4 GB makes Tom OS feel snappier.")
    } else {
        check_row("\u{f00d}", "ti-error", &format!("{gib:.1} GB memory"), "Tom OS needs at least 2 GB of memory.")
    });
    let usable = disks.iter().filter(|d| d.unusable_reason().is_none()).count();
    page.append(&if usable > 0 {
        check_row(
            "\u{f00c}",
            "ti-ok",
            &format!("{usable} disk{} with room for Tom OS", if usable == 1 { "" } else { "s" }),
            "Tom OS needs 12 GB or more. You choose the disk in a moment — nothing changes until the last step.",
        )
    } else {
        check_row("\u{f00d}", "ti-error", "No suitable disk found", "Tom OS needs a disk of at least 12 GB that isn't the live USB.")
    });
    let (on_battery, has_battery) = if opts.demo { (false, true) } else { power_state() };
    page.append(&if on_battery {
        check_row("\u{f071}", "ti-warn", "Running on battery", "Plug in the charger so the computer doesn't switch off part-way.")
    } else {
        check_row("\u{f00c}", "ti-ok", if has_battery { "Plugged in" } else { "Mains power" }, "Good — the computer won't run out of power while installing.")
    });
    page.append(&check_row(
        "\u{f05a}",
        "ti-cyan",
        if uefi { "Starts in UEFI mode" } else { "Starts in BIOS (legacy) mode" },
        "Tom OS installs boot loaders for both UEFI and BIOS, so the disk boots either way.",
    ));
    if !opts.demo && opts.source.is_none() && backend::resolve_source(None).is_err() {
        page.append(&check_row(
            "\u{f071}",
            "ti-warn",
            "Not running from the Tom OS live USB",
            "The system image wasn't found. Start the installer from the live USB, or pass --source.",
        ));
    }
    page
}

fn build(app: &gtk::Application, opts: &Options) {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&style::css());
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_USER);
        gtk::Settings::for_display(&display).set_gtk_application_prefer_dark_theme(true);
    }
    let uefi = opts.demo || Path::new("/sys/firmware/efi").is_dir();
    let disk_vec = if opts.demo { disks::demo_disks() } else { disks::list(opts.allow_loop).unwrap_or_default() };

    let window = gtk::ApplicationWindow::builder().application(app).title("Install Tom OS").default_width(1180).default_height(780).build();
    window.add_css_class("ti");

    let overlay = gtk::Overlay::new();
    if let Some(bg) = first_existing(WALLPAPERS) {
        let pic = gtk::Picture::for_filename(&bg);
        pic.set_content_fit(gtk::ContentFit::Cover);
        pic.set_can_shrink(true);
        overlay.set_child(Some(&pic));
    }
    let root = hbox(0);
    root.add_css_class("ti-scrim");
    overlay.add_overlay(&root);
    window.set_child(Some(&overlay));

    // Sidebar
    let side = vbox(6);
    side.add_css_class("ti-side");
    let brand_box = hbox(10);
    if let Some(logo) = first_existing(LOGOS) {
        let img = gtk::Image::from_file(logo);
        img.set_pixel_size(40);
        brand_box.append(&img);
    }
    let bt = vbox(0);
    bt.append(&label("TOM OS", "ti-brand"));
    bt.append(&label("INSTALLER", "ti-brand-sub"));
    brand_box.append(&bt);
    brand_box.set_margin_bottom(26);
    side.append(&brand_box);
    let mut steps = Vec::new();
    for (i, name) in STEPS.iter().enumerate() {
        let row = hbox(12);
        row.add_css_class("ti-step");
        let num = gtk::Label::new(Some(&(i + 1).to_string()));
        num.add_css_class("num");
        num.set_valign(gtk::Align::Center);
        row.append(&num);
        row.append(&label(name, "name"));
        side.append(&row);
        steps.push(row);
    }
    let spacer = vbox(0);
    spacer.set_vexpand(true);
    side.append(&spacer);
    if opts.demo {
        side.append(&label("DEMO MODE · NOTHING IS CHANGED", "ti-warn ti-version"));
    }
    side.append(&label(&format!("v{}", crate::VERSION), "ti-version"));
    root.append(&side);

    // Card
    let card = vbox(0);
    card.add_css_class("ti-card");
    card.set_hexpand(true);
    card.set_vexpand(true);
    card.set_margin_top(28);
    card.set_margin_bottom(28);
    card.set_margin_start(28);
    card.set_margin_end(28);
    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::SlideLeftRight);
    stack.set_transition_duration(220);
    stack.set_vexpand(true);
    card.append(&stack);
    let nav = hbox(12);
    nav.add_css_class("ti-nav");
    let back = gtk::Button::with_mnemonic("_BACK");
    back.add_css_class("ti-ghost");
    let error = wrap(label("", "ti-error"));
    error.set_hexpand(true);
    error.set_xalign(1.0);
    let next = gtk::Button::with_mnemonic("_NEXT");
    next.add_css_class("ti-primary");
    nav.append(&back);
    nav.append(&error);
    nav.append(&next);
    card.append(&nav);
    root.append(&card);

    let add_page = |n: usize, child: &gtk::Box| {
        child.add_css_class("ti-page");
        let sc = gtk::ScrolledWindow::new();
        sc.set_hscrollbar_policy(gtk::PolicyType::Never);
        sc.set_child(Some(child));
        stack.add_named(&sc, Some(&format!("p{n}")));
    };

    // 0 Welcome
    add_page(P_WELCOME, &welcome_page(opts, uefi, &disk_vec));

    // 1 Language & region
    let locales = catalog::locales();
    let names: Vec<String> = locales.iter().map(|c| c.name.clone()).collect();
    let locale_dd = string_dropdown(&names, true);
    let zones = catalog::timezones();
    let mut regions: Vec<String> = zones.iter().map(|z| catalog::split_zone(z).0).collect();
    regions.dedup();
    regions.sort();
    regions.dedup();
    let region_dd = string_dropdown(&regions, true);
    let city_dd = string_dropdown(&[], true);
    let clock = label("--:--", "ti-clock");
    let lang = vbox(16);
    lang.append(&page_header("LANGUAGE & REGION", "Choose the language for Tom OS and where you are, for the clock."));
    lang.append(&field("LANGUAGE", &locale_dd));
    let tz_row = hbox(14);
    tz_row.set_homogeneous(true);
    tz_row.append(&field("REGION", &region_dd));
    tz_row.append(&field("CITY", &city_dd));
    lang.append(&tz_row);
    let clock_box = vbox(2);
    clock_box.add_css_class("ti-infobox");
    clock_box.append(&label("LOCAL TIME THERE", "ti-label"));
    clock_box.append(&clock);
    lang.append(&clock_box);
    add_page(P_LANG, &lang);

    // 2 Keyboard
    let layouts = catalog::keyboards();
    let layout_search = gtk::SearchEntry::new();
    layout_search.set_placeholder_text(Some("Search layouts — e.g. UK, German, Dvorak"));
    let layout_list = gtk::ListBox::new();
    layout_list.add_css_class("ti-list");
    layout_list.add_css_class("ti-compact");
    for l in &layouts {
        let row = hbox(10);
        let n = label(&l.name, "ti-text");
        n.set_hexpand(true);
        row.append(&n);
        row.append(&label(&l.id, "ti-muted"));
        layout_list.append(&row);
    }
    let layout_scroll = gtk::ScrolledWindow::new();
    layout_scroll.set_child(Some(&layout_list));
    layout_scroll.set_min_content_height(250);
    layout_scroll.set_vexpand(true);
    layout_scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
    let variant_dd = string_dropdown(&["Default".to_string()], true);
    let test = gtk::Entry::new();
    test.set_placeholder_text(Some("Type here to test your keyboard — try £ € @ \" # ~"));
    test.add_css_class("ti-test");
    let keys = vbox(14);
    keys.append(&page_header("KEYBOARD", "Pick the layout printed on your keys. It's used everywhere: the login screen, Hyprland and the console."));
    keys.append(&layout_search);
    keys.append(&layout_scroll);
    keys.append(&field("VARIANT", &variant_dd));
    keys.append(&field("TEST", &test));
    add_page(P_KEYS, &keys);

    // 3 Disk
    let disk_list = gtk::ListBox::new();
    disk_list.add_css_class("ti-list");
    for d in &disk_vec {
        let row = hbox(14);
        let icon = label(if d.transport == "usb" || d.removable { "\u{f287}" } else { "\u{f0a0}" }, "ti-cyan");
        icon.add_css_class("ti-big");
        row.append(&icon);
        let t = vbox(3);
        t.append(&label(&format!("{}   {}", d.title(), d.path), "ti-big"));
        t.append(&wrap(label(&d.summary(), "ti-muted")));
        if let Some(why) = d.unusable_reason() {
            t.append(&label(&why, "ti-warn"));
        }
        t.set_hexpand(true);
        row.append(&t);
        disk_list.append(&row);
        if d.unusable_reason().is_some() {
            if let Some(r) = disk_list.row_at_index(disk_list.observe_children().n_items() as i32 - 1) {
                r.set_sensitive(false);
                r.set_selectable(false);
            }
        }
    }
    if disk_vec.is_empty() {
        let row = label("No disks found.", "ti-warn");
        disk_list.append(&row);
    }
    let erase_opt = gtk::CheckButton::with_label("Erase disk and install Tom OS");
    let manual_opt = gtk::CheckButton::with_label("Install to an existing partition");
    manual_opt.set_group(Some(&erase_opt));
    erase_opt.set_active(true);
    let opt_box = |b: &gtk::CheckButton, detail: &str| {
        let v = vbox(4);
        v.add_css_class("ti-option");
        v.set_hexpand(true);
        v.append(b);
        let d = wrap(label(detail, "ti-muted"));
        d.set_margin_start(28);
        v.append(&d);
        v
    };
    let erase_box = opt_box(&erase_opt, "Simplest. A new GPT layout: BIOS boot, EFI system (512 MB) and Tom OS.");
    let manual_box_frame = opt_box(&manual_opt, "Keep other systems. The chosen partition is formatted; others stay as they are.");
    let mode_row = hbox(14);
    mode_row.append(&erase_box);
    mode_row.append(&manual_box_frame);
    let part_dd = string_dropdown(&[], false);
    let esp_dd = string_dropdown(&[], false);
    ellipsize(&part_dd);
    ellipsize(&esp_dd);
    let manual_box = hbox(14);
    manual_box.set_homogeneous(true);
    manual_box.append(&field("TOM OS PARTITION (FORMATTED)", &part_dd));
    manual_box.append(&field("EFI SYSTEM PARTITION (KEPT)", &esp_dd));
    let fs_dd = string_dropdown(&["ext4 — tried and trusted".to_string(), "btrfs — compression and snapshots".to_string()], false);
    let swap = gtk::SpinButton::with_range(0.0, 16.0, 1.0);
    swap.set_value(2.0);
    let opts_row = hbox(14);
    opts_row.set_homogeneous(true);
    opts_row.append(&field("FILE SYSTEM", &fs_dd));
    opts_row.append(&field("SWAP FILE (GB)", &swap));
    let preview = gtk::DrawingArea::new();
    preview.set_content_height(44);
    preview.set_hexpand(true);
    let disk_page = vbox(14);
    disk_page.append(&page_header("DISK", "Where should Tom OS go? Nothing is written until you confirm on the last page."));
    disk_page.append(&disk_list);
    disk_page.append(&mode_row);
    disk_page.append(&manual_box);
    disk_page.append(&opts_row);
    disk_page.append(&field("AFTER INSTALLING", &preview));
    add_page(P_DISK, &disk_page);

    // 4 You
    let full_name = gtk::Entry::new();
    full_name.set_placeholder_text(Some("Tom Smith"));
    let username = gtk::Entry::new();
    username.set_placeholder_text(Some("tom"));
    let hostname = gtk::Entry::new();
    hostname.set_text("tomos");
    let password = gtk::PasswordEntry::new();
    password.set_show_peek_icon(true);
    password.set_property("placeholder-text", "At least 6 characters");
    let confirm = gtk::PasswordEntry::new();
    confirm.set_show_peek_icon(true);
    confirm.set_property("placeholder-text", "Type it again");
    let strength = gtk::LevelBar::for_interval(0.0, 4.0);
    strength.set_mode(gtk::LevelBarMode::Discrete);
    strength.add_offset_value("low", 1.0);
    strength.add_offset_value("high", 3.0);
    strength.add_offset_value("full", 4.0);
    let strength_label = label("", "ti-muted");
    let autologin = gtk::Switch::new();
    autologin.set_valign(gtk::Align::Center);
    let you = vbox(14);
    you.append(&page_header("YOU", "Create your account. You'll use the password to sign in and for admin tasks (sudo)."));
    let r1 = hbox(14);
    r1.set_homogeneous(true);
    r1.append(&field("YOUR NAME", &full_name));
    r1.append(&field("USERNAME", &username));
    you.append(&r1);
    you.append(&field("COMPUTER NAME", &hostname));
    let r2 = hbox(14);
    r2.set_homogeneous(true);
    r2.append(&field("PASSWORD", &password));
    r2.append(&field("CONFIRM PASSWORD", &confirm));
    you.append(&r2);
    let sb = hbox(12);
    strength.set_hexpand(true);
    strength.set_valign(gtk::Align::Center);
    sb.append(&strength);
    sb.append(&strength_label);
    you.append(&sb);
    let al = hbox(12);
    al.add_css_class("ti-option");
    let alt = vbox(2);
    alt.append(&label("Sign in automatically", "ti-big"));
    alt.append(&wrap(label("Skip the TDM login screen when the computer starts. Anyone with the computer can use it.", "ti-muted")));
    alt.set_hexpand(true);
    al.append(&alt);
    al.append(&autologin);
    you.append(&al);
    add_page(P_USER, &you);

    // 5 Summary
    let summary_page = vbox(14);
    summary_page.append(&page_header("SUMMARY", "Check everything. Go back to change anything."));
    let summary = vbox(14);
    summary_page.append(&summary);
    add_page(P_SUMMARY, &summary_page);

    // 6 Install
    let inst = vbox(16);
    inst.append(&page_header("INSTALLING TOM OS", "Sit back — this takes a few minutes. Keep the computer plugged in."));
    let percent = label("0%", "ti-percent");
    inst.append(&percent);
    let progress = gtk::ProgressBar::new();
    inst.append(&progress);
    let step_label = label("Starting…", "ti-cyan");
    inst.append(&step_label);
    let tipbox = hbox(12);
    tipbox.add_css_class("ti-infobox");
    tipbox.append(&label("\u{f0eb}", "ti-warn"));
    let tip = wrap(label(TIPS[0], "ti-tip"));
    tipbox.append(&tip);
    inst.append(&tipbox);
    let log = gtk::TextView::new();
    log.set_editable(false);
    log.set_monospace(true);
    log.set_cursor_visible(false);
    log.add_css_class("ti-log");
    log.set_left_margin(10);
    log.set_top_margin(8);
    let log_scroll = gtk::ScrolledWindow::new();
    log_scroll.set_child(Some(&log));
    log_scroll.set_min_content_height(210);
    log_scroll.add_css_class("ti-logframe");
    let exp = gtk::Expander::new(Some("DETAILS"));
    exp.set_expanded(true);
    exp.set_child(Some(&log_scroll));
    inst.append(&exp);
    add_page(P_INSTALL, &inst);

    // 7 Done / failed
    let done = vbox(16);
    done.set_valign(gtk::Align::Center);
    done.set_vexpand(true);
    let done_icon = label("\u{f05d}", "ti-done-icon ti-ok");
    done_icon.set_xalign(0.5);
    let done_title = label("TOM OS IS INSTALLED", "ti-title");
    done_title.set_xalign(0.5);
    let done_text = wrap(label("", "ti-text"));
    done_text.set_xalign(0.5);
    done_text.set_justify(gtk::Justification::Center);
    done_text.set_max_width_chars(70);
    let btns = hbox(14);
    btns.set_halign(gtk::Align::Center);
    let close = gtk::Button::with_label("KEEP EXPLORING");
    close.add_css_class("ti-ghost");
    let restart = gtk::Button::with_label("RESTART NOW");
    restart.add_css_class("ti-primary");
    btns.append(&close);
    btns.append(&restart);
    let logo_done = first_existing(LOGOS).map(gtk::Image::from_file);
    if let Some(img) = &logo_done {
        img.set_pixel_size(120);
        done.append(img);
    }
    done.append(&done_icon);
    done.append(&done_title);
    done.append(&done_text);
    done.append(&btns);
    add_page(P_DONE, &done);

    let ui = Rc::new(Ui {
        opts: opts.clone(),
        window: window.clone(),
        stack,
        steps,
        back: back.clone(),
        next: next.clone(),
        nav,
        error,
        page: Cell::new(0),
        plan: RefCell::new(InstallPlan { source: opts.source.clone(), ..InstallPlan::default() }),
        installing: Cell::new(false),
        uefi,
        locales,
        locale_dd: locale_dd.clone(),
        zones,
        regions,
        region_dd: region_dd.clone(),
        city_dd: city_dd.clone(),
        cities: RefCell::new(Vec::new()),
        clock,
        layouts,
        layout_list: layout_list.clone(),
        variant_dd: variant_dd.clone(),
        disks: RefCell::new(disk_vec),
        disk_list: disk_list.clone(),
        erase_opt: erase_opt.clone(),
        manual_opt: manual_opt.clone(),
        erase_box,
        manual_box,
        manual_box_frame,
        part_dd: part_dd.clone(),
        part_paths: RefCell::new(Vec::new()),
        esp_dd,
        esp_paths: RefCell::new(Vec::new()),
        fs_dd,
        swap,
        preview: preview.clone(),
        full_name: full_name.clone(),
        username: username.clone(),
        hostname: hostname.clone(),
        password: password.clone(),
        confirm,
        strength,
        strength_label,
        autologin,
        username_edited: Cell::new(false),
        hostname_edited: Cell::new(false),
        setting: Cell::new(false),
        summary,
        step_label,
        percent,
        progress,
        tip,
        log,
        log_scroll,
        done_title,
        done_text,
        done_icon,
        restart: restart.clone(),
        close: close.clone(),
    });

    // --- Signals ----------------------------------------------------------------------------
    {
        let ui2 = ui.clone();
        next.connect_clicked(move |_| ui2.go_next());
        let ui2 = ui.clone();
        back.connect_clicked(move |_| {
            let n = ui2.page.get();
            if n > 0 && n < P_INSTALL {
                ui2.show_page(n - 1);
            }
        });
    }
    // Language
    let default_locale = ui.locales.iter().position(|c| c.id == "en_GB.UTF-8").unwrap_or(0);
    ui.locale_dd.set_selected(default_locale as u32);
    {
        let ui2 = ui.clone();
        ui.region_dd.connect_selected_notify(move |dd| {
            let region = ui2.regions.get(dd.selected() as usize).cloned().unwrap_or_default();
            let cities: Vec<String> = ui2
                .zones
                .iter()
                .filter(|z| catalog::split_zone(z).0 == region)
                .map(|z| z.split_once('/').map(|(_, c)| c.to_string()).unwrap_or_default())
                .collect();
            let shown: Vec<String> = cities.iter().map(|c| if c.is_empty() { region.clone() } else { c.replace('_', " ") }).collect();
            *ui2.cities.borrow_mut() = cities;
            set_dropdown_items(&ui2.city_dd, &shown);
        });
        let ui2 = ui.clone();
        ui.city_dd.connect_selected_notify(move |_| update_clock(&ui2));
        let r = ui.regions.iter().position(|r| r == "Europe").unwrap_or(0) as u32;
        ui.region_dd.set_selected(r);
        ui.region_dd.notify("selected");
        if let Some(c) = ui.cities.borrow().iter().position(|c| c == "London") {
            ui.city_dd.set_selected(c as u32);
        }
        update_clock(&ui);
        let ui2 = ui.clone();
        glib::timeout_add_seconds_local(20, move || {
            update_clock(&ui2);
            glib::ControlFlow::Continue
        });
    }
    // Keyboard
    {
        let s = layout_search.clone();
        let layouts = ui.layouts.clone();
        layout_list.set_filter_func(move |row| {
            let q = s.text().to_lowercase();
            if q.is_empty() {
                return true;
            }
            layouts.get(row.index() as usize).map(|l| l.name.to_lowercase().contains(&q) || l.id.to_lowercase() == q).unwrap_or(true)
        });
        let list = layout_list.clone();
        layout_search.connect_search_changed(move |_| list.invalidate_filter());
        let ui2 = ui.clone();
        layout_list.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            let Some(l) = ui2.layouts.get(row.index() as usize) else { return };
            let mut names = vec!["Default".to_string()];
            names.extend(l.variants.iter().map(|v| v.name.clone()));
            set_dropdown_items(&ui2.variant_dd, &names);
            ui2.variant_dd.set_selected(0);
            apply_live_layout(&ui2.opts, &l.id, "");
        });
        let ui2 = ui.clone();
        variant_dd.connect_selected_notify(move |dd| {
            if let Some(l) = ui2.selected_layout() {
                let v = dd.selected() as usize;
                let variant = if v == 0 { "" } else { l.variants.get(v - 1).map(|c| c.id.as_str()).unwrap_or("") };
                apply_live_layout(&ui2.opts, &l.id, variant);
            }
        });
        let gb = ui.layouts.iter().position(|l| l.id == "gb").unwrap_or(0);
        if let Some(row) = layout_list.row_at_index(gb as i32) {
            layout_list.select_row(Some(&row));
        }
    }
    // Disk
    {
        let ui2 = ui.clone();
        disk_list.connect_row_selected(move |_, _| ui2.refresh_disk_choices());
        let ui2 = ui.clone();
        erase_opt.connect_toggled(move |_| ui2.update_mode());
        let ui2 = ui.clone();
        part_dd.connect_selected_notify(move |_| ui2.preview.queue_draw());
        let ui2 = ui.clone();
        preview.set_draw_func(move |_, cr, w, h| ui2.draw_preview(cr, w, h));
        let first_ok = ui.disks.borrow().iter().position(|d| d.unusable_reason().is_none());
        if let Some(i) = first_ok {
            if let Some(row) = disk_list.row_at_index(i as i32) {
                disk_list.select_row(Some(&row));
            }
        }
        ui.refresh_disk_choices();
        if opts.demo && std::env::var_os("TOMOS_DEMO_MANUAL").is_some() {
            ui.manual_opt.set_active(true);
            ui.part_dd.set_selected(2);
            ui.fs_dd.set_selected(1);
        }
    }
    // You
    {
        let ui2 = ui.clone();
        full_name.connect_changed(move |e| {
            if !ui2.username_edited.get() {
                ui2.setting.set(true);
                ui2.username.set_text(&plan::suggest_username(&e.text()));
                ui2.setting.set(false);
            }
        });
        let ui2 = ui.clone();
        username.connect_changed(move |e| {
            if ui2.setting.get() {
                if !ui2.hostname_edited.get() {
                    let u = e.text();
                    let h = if u.is_empty() { "tomos".to_string() } else { format!("{u}-tomos") };
                    ui2.setting.set(true);
                    ui2.hostname.set_text(&h);
                    ui2.setting.set(false);
                }
                return;
            }
            ui2.username_edited.set(!e.text().is_empty());
        });
        let ui2 = ui.clone();
        hostname.connect_changed(move |e| {
            if !ui2.setting.get() {
                ui2.hostname_edited.set(!e.text().is_empty());
            }
        });
        let ui2 = ui.clone();
        password.connect_changed(move |e| {
            let s = plan::password_strength(&e.text());
            ui2.strength.set_value(s as f64);
            ui2.strength_label.set_text(match (e.text().is_empty(), s) {
                (true, _) => "",
                (_, 0) => "Too short",
                (_, 1) => "Weak",
                (_, 2) => "Fair",
                (_, 3) => "Good",
                _ => "Strong",
            });
        });
    }
    // Done
    {
        let ui2 = ui.clone();
        restart.connect_clicked(move |_| {
            if ui2.opts.demo {
                ui2.window.close();
            } else {
                let _ = Command::new("systemctl").arg("reboot").spawn();
            }
        });
        let w = window.clone();
        close.connect_clicked(move |_| w.close());
        let ui2 = ui.clone();
        window.connect_close_request(move |_| if ui2.installing.get() { glib::Propagation::Stop } else { glib::Propagation::Proceed });
    }

    // Demo answers, for screenshots and trying it out.
    if opts.demo {
        full_name.set_text("Tom Smith");
        password.set_text("Neon-Punk-2026");
        ui.confirm.set_text("Neon-Punk-2026");
    }
    let start = opts.page.unwrap_or(0);
    if start > 0 && opts.demo {
        // Read every page up to `start` so the plan is complete.
        for p in 1..start.min(P_INSTALL) {
            ui.page.set(p);
            ui.go_next();
        }
        match start {
            P_INSTALL => {
                ui.page.set(P_SUMMARY);
                ui.start_install();
            }
            P_DONE => ui.finish(None),
            n => ui.show_page(n.min(P_SUMMARY)),
        }
    } else {
        ui.show_page(0);
    }
    window.present();
}

fn update_clock(ui: &Rc<Ui>) {
    let tz = ui.selected_zone();
    #[allow(deprecated)]
    let zone = glib::TimeZone::new(Some(&tz));
    let text = glib::DateTime::now(&zone)
        .ok()
        .and_then(|d| d.format("%H:%M  ·  %A %e %B").ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "--:--".into());
    ui.clock.set_text(&text);
}

/// Switches the live session's keyboard so the test box shows the chosen layout.
fn apply_live_layout(opts: &Options, layout: &str, variant: &str) {
    if opts.demo || std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none() {
        return;
    }
    let _ = Command::new("hyprctl")
        .args(["--batch", &format!("keyword input:kb_layout {layout} ; keyword input:kb_variant {variant}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

pub fn run(opts: Options) -> i32 {
    let app = gtk::Application::builder().application_id("org.tomos.Installer").flags(gio::ApplicationFlags::NON_UNIQUE).build();
    app.connect_activate(move |app| build(app, &opts));
    let code = app.run_with_args(&[String::from("tomos-installer")]);
    code.value()
}

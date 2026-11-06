//! The root backend: turns an [`InstallPlan`] into an explicit list of steps and runs them.
//!
//! Every disk operation is an [`Action`] built by [`build_steps`], so the whole install can be
//! printed with `--dry-run` and unit tested without touching a disk. The GUI never runs
//! commands itself; it starts `tomos-installer backend` as root and reads [`Event`]s.

use crate::disks;
use crate::events::Event;
use crate::plan::{Filesystem, InstallPlan, Storage, MIN_TARGET_BYTES, USER_GROUPS};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::FileTypeExt;
use std::path::Path;
use std::process::{Command, Stdio};

/// Where the new system is mounted while installing.
pub const TARGET: &str = "/mnt/tomos-target";
/// The live system's log file (copied into the new system at the end).
pub const LOG_FILE: &str = "/var/log/tomos-installer.log";
/// Places the live system image is looked for, in order.
pub const SOURCE_CANDIDATES: &[&str] = &[
    "/run/live/medium/live/filesystem.squashfs",
    "/lib/live/mount/medium/live/filesystem.squashfs",
    "/cdrom/live/filesystem.squashfs",
];
/// Live-only packages removed from the installed system (when installed).
pub const LIVE_PACKAGES: &[&str] = &[
    "live-boot",
    "live-boot-initramfs-tools",
    "live-boot-doc",
    "live-config",
    "live-config-systemd",
    "live-config-doc",
    "live-tools",
    "calamares",
    "calamares-data",
    "libcalamares3.3",
    "libcalamaresui3.3",
];
/// Live-only files removed from the installed system.
pub const LIVE_FILES: &[&str] = &[
    "/usr/bin/tomos-install",
    "/usr/share/applications/tomos-install.desktop",
    "/usr/share/applications/calamares.desktop",
    "/usr/share/applications/tomos-installer.desktop",
    "/etc/skel/Desktop/tomos-installer.desktop",
];

const CHROOT_PATH: &str = "PATH=/usr/sbin:/usr/bin:/sbin:/bin";

/// A command line. `stdin` is fed to the process; when `secret` is set it is never logged.
#[derive(Debug, Clone, PartialEq)]
pub struct Cmd {
    pub program: String,
    pub args: Vec<String>,
    pub stdin: Option<String>,
    pub secret: bool,
}

impl Cmd {
    pub fn new(program: &str, args: &[&str]) -> Cmd {
        Cmd { program: program.into(), args: args.iter().map(|a| a.to_string()).collect(), stdin: None, secret: false }
    }
    pub fn with_secret_stdin(mut self, input: String) -> Cmd {
        self.stdin = Some(input);
        self.secret = true;
        self
    }
    /// The command as a shell-like line, for logs.
    pub fn display(&self) -> String {
        let mut out = self.program.clone();
        for a in &self.args {
            out.push(' ');
            if a.is_empty() || a.contains(|c: char| c.is_whitespace() || "\"'$`\\".contains(c)) {
                out.push_str(&format!("'{}'", a.replace('\'', "'\\''")));
            } else {
                out.push_str(a);
            }
        }
        if self.stdin.is_some() {
            out.push_str(if self.secret { "  <<< (hidden)" } else { "  <<< (input)" });
        }
        out
    }
}

/// Runs a program inside the new system.
pub fn chroot(target: &str, args: &[&str]) -> Cmd {
    let mut all = vec![target, "/usr/bin/env", "LANG=C.UTF-8", "LC_ALL=C.UTF-8", CHROOT_PATH, "DEBIAN_FRONTEND=noninteractive"];
    all.extend_from_slice(args);
    Cmd::new("chroot", &all)
}

/// Where the system is copied from.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    Squashfs(String),
    Dir(String),
}

impl Source {
    pub fn path(&self) -> &str {
        match self {
            Source::Squashfs(p) | Source::Dir(p) => p,
        }
    }
}

/// One line of `/etc/fstab`. `spec` is either a ready `UUID=…`/path or `probe:` + device,
/// which is resolved with `blkid` when the step runs.
#[derive(Debug, Clone, PartialEq)]
pub struct FstabEntry {
    pub spec: String,
    pub mountpoint: String,
    pub fstype: String,
    pub options: String,
    pub pass: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Safety checks before anything is written (see [`preflight`]).
    Preflight { disk: String, root: String, erase: bool, allow_loop: bool },
    /// A command that must succeed.
    Run(Cmd),
    /// A command whose failure is only logged.
    Try(Cmd),
    /// Waits for partition device nodes to appear.
    WaitFor(Vec<String>),
    Mkdir(String),
    Write { path: String, contents: String },
    Append { path: String, contents: String },
    /// Replaces the first line whose trimmed text starts with `key` (followed by space or `=`),
    /// or appends `line` (after the line starting with `after`, when given). No-op if the file
    /// is missing and `optional` is set.
    SetLine { path: String, key: String, line: String, after: Option<String>, optional: bool },
    /// Replaces `from` with `to` once, unless the file already contains `unless`.
    Replace { path: String, from: String, to: String, unless: String },
    /// `ln -sfn target link`.
    Symlink { target: String, link: String },
    /// Like `Symlink`, only if `when` exists.
    SymlinkIf { when: String, target: String, link: String },
    Remove(String),
    RemoveTree(String),
    /// Removes `path` only if it contains `needle`.
    RemoveIfContains { path: String, needle: String },
    /// Fails with `message` unless `path` exists.
    Require { path: String, message: String },
    /// Copies the system with progress (unsquashfs or rsync).
    CopySystem { source: Source, target: String },
    Fstab { path: String, entries: Vec<FstabEntry> },
    /// `useradd` with the groups from `groups` that exist in the new system.
    AddUser { target: String, username: String, full_name: String, groups: Vec<String> },
    /// `dpkg --purge` the given packages that are installed in the new system.
    PurgeIfInstalled { target: String, packages: Vec<String> },
    /// Copies the install log into the new system.
    CopyLog { to: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub title: String,
    /// Share of the progress bar (relative).
    pub weight: f64,
    pub actions: Vec<Action>,
}

/// Facts about the machine and fresh identifiers, decided before the steps are built.
#[derive(Debug, Clone, PartialEq)]
pub struct Context {
    pub target: String,
    pub source: Source,
    /// Booted in UEFI mode (`/sys/firmware/efi` exists).
    pub uefi: bool,
    /// UUID for the new root file system.
    pub root_uuid: String,
    /// Volume ID for a new EFI partition, 8 hex digits.
    pub esp_id: String,
    /// Manual mode: the disk can take a BIOS boot loader (MBR, or GPT with a BIOS boot partition).
    pub bios_capable: bool,
    pub allow_loop: bool,
}

impl Context {
    /// The `UUID=` form of the EFI volume ID (`ABCD-1234`).
    pub fn esp_uuid(&self) -> String {
        format!("{}-{}", &self.esp_id[..4], &self.esp_id[4..])
    }
}

fn t(target: &str, path: &str) -> String {
    format!("{}{}", target.trim_end_matches('/'), path)
}

/// Builds the full install as steps.
pub fn build_steps(plan: &InstallPlan, ctx: &Context) -> Vec<Step> {
    let tg = ctx.target.as_str();
    let mut steps = Vec::new();
    let btrfs = plan.filesystem == Filesystem::Btrfs;
    let fs_name = plan.filesystem.label();

    // --- Where things go -------------------------------------------------------------------
    let (disk, root, esp, erase) = match &plan.storage {
        Storage::Erase { disk } => (disk.clone(), disks::part_path(disk, 3), Some(disks::part_path(disk, 2)), true),
        Storage::Manual { root, esp, disk } => (disk.clone(), root.clone(), esp.clone(), false),
    };
    let bios = if erase { true } else { ctx.bios_capable };

    steps.push(Step {
        title: "Checking the computer".into(),
        weight: 1.0,
        actions: vec![Action::Preflight { disk: disk.clone(), root: root.clone(), erase, allow_loop: ctx.allow_loop }],
    });

    // --- Partitions ------------------------------------------------------------------------
    if erase {
        let script = format!(
            "label: gpt\n\
             size=1MiB, type={}, name=\"BIOS boot\"\n\
             size=512MiB, type={}, name=\"EFI system\"\n\
             type={}, name=\"Tom OS\"\n",
            disks::GPT_BIOS_BOOT.to_uppercase(),
            disks::GPT_ESP.to_uppercase(),
            disks::GPT_LINUX.to_uppercase()
        );
        let mut sfdisk = Cmd::new("sfdisk", &["--wipe", "always", "--wipe-partitions", "always", &disk]);
        sfdisk.stdin = Some(script);
        steps.push(Step {
            title: "Partitioning the disk".into(),
            weight: 2.0,
            actions: vec![
                Action::Run(Cmd::new("wipefs", &["-a", &disk])),
                Action::Run(sfdisk),
                Action::Try(Cmd::new("partprobe", &[&disk])),
                Action::Try(Cmd::new("udevadm", &["settle", "--timeout=15"])),
                Action::WaitFor(vec![disks::part_path(&disk, 1), disks::part_path(&disk, 2), root.clone()]),
            ],
        });
    }

    // --- File systems ----------------------------------------------------------------------
    let mut format = Vec::new();
    if erase {
        let esp = esp.clone().unwrap_or_default();
        format.push(Action::Run(Cmd::new("mkfs.fat", &["-F", "32", "-n", "TOMOS-EFI", "-i", &ctx.esp_id, &esp])));
    }
    format.push(Action::Try(Cmd::new("wipefs", &["-a", &root])));
    if btrfs {
        format.push(Action::Run(Cmd::new("mkfs.btrfs", &["-f", "-L", "TomOS", "-U", &ctx.root_uuid, &root])));
    } else {
        format.push(Action::Run(Cmd::new("mkfs.ext4", &["-F", "-q", "-L", "TomOS", "-U", &ctx.root_uuid, &root])));
    }
    steps.push(Step { title: format!("Formatting ({fs_name})"), weight: 2.0, actions: format });

    // --- Mount -----------------------------------------------------------------------------
    let btrfs_opts = "compress=zstd:1,noatime";
    let mut mount = vec![Action::Mkdir(tg.into())];
    if btrfs {
        let tmp = format!("{tg}-btrfs");
        mount.push(Action::Mkdir(tmp.clone()));
        mount.push(Action::Run(Cmd::new("mount", &[&root, &tmp])));
        for sv in ["@", "@home", "@swap"] {
            mount.push(Action::Run(Cmd::new("btrfs", &["subvolume", "create", &format!("{tmp}/{sv}")])));
        }
        mount.push(Action::Run(Cmd::new("umount", &[&tmp])));
        mount.push(Action::Try(Cmd::new("rmdir", &[&tmp])));
        mount.push(Action::Run(Cmd::new("mount", &["-o", &format!("subvol=@,{btrfs_opts}"), &root, tg])));
        mount.push(Action::Mkdir(t(tg, "/home")));
        mount.push(Action::Run(Cmd::new("mount", &["-o", &format!("subvol=@home,{btrfs_opts}"), &root, &t(tg, "/home")])));
        mount.push(Action::Mkdir(t(tg, "/swap")));
        mount.push(Action::Run(Cmd::new("mount", &["-o", "subvol=@swap,noatime", &root, &t(tg, "/swap")])));
    } else {
        mount.push(Action::Run(Cmd::new("mount", &[&root, tg])));
    }
    if let Some(esp) = &esp {
        mount.push(Action::Mkdir(t(tg, "/boot/efi")));
        mount.push(Action::Run(Cmd::new("mount", &[esp, &t(tg, "/boot/efi")])));
    }
    steps.push(Step { title: "Mounting".into(), weight: 1.0, actions: mount });

    // --- Copy ------------------------------------------------------------------------------
    steps.push(Step {
        title: "Copying Tom OS".into(),
        weight: 62.0,
        actions: vec![
            Action::CopySystem { source: ctx.source.clone(), target: tg.into() },
            Action::Require { path: t(tg, "/usr/bin/Hyprland"), message: "The copied system looks incomplete (no Hyprland).".into() },
        ],
    });

    // --- Configure -------------------------------------------------------------------------
    let mut cfg = Vec::new();
    let mut fstab = Vec::new();
    let root_spec = format!("UUID={}", ctx.root_uuid);
    if btrfs {
        fstab.push(FstabEntry { spec: root_spec.clone(), mountpoint: "/".into(), fstype: "btrfs".into(), options: format!("subvol=@,{btrfs_opts}"), pass: 0 });
        fstab.push(FstabEntry { spec: root_spec.clone(), mountpoint: "/home".into(), fstype: "btrfs".into(), options: format!("subvol=@home,{btrfs_opts}"), pass: 0 });
        fstab.push(FstabEntry { spec: root_spec.clone(), mountpoint: "/swap".into(), fstype: "btrfs".into(), options: "subvol=@swap,noatime".into(), pass: 0 });
    } else {
        fstab.push(FstabEntry { spec: root_spec, mountpoint: "/".into(), fstype: "ext4".into(), options: "errors=remount-ro".into(), pass: 1 });
    }
    if let Some(esp) = &esp {
        let spec = if erase { format!("UUID={}", ctx.esp_uuid()) } else { format!("probe:{esp}") };
        fstab.push(FstabEntry { spec, mountpoint: "/boot/efi".into(), fstype: "vfat".into(), options: "umask=0077".into(), pass: 1 });
    }
    let swapfile = if btrfs { "/swap/swapfile" } else { "/swapfile" };
    if plan.swap_mib > 0 {
        fstab.push(FstabEntry { spec: swapfile.into(), mountpoint: "none".into(), fstype: "swap".into(), options: "sw".into(), pass: 0 });
    }
    cfg.push(Action::Fstab { path: t(tg, "/etc/fstab"), entries: fstab });
    if plan.swap_mib > 0 {
        let size = format!("{}M", plan.swap_mib);
        if btrfs {
            cfg.push(Action::Run(Cmd::new("btrfs", &["filesystem", "mkswapfile", "--size", &size, &t(tg, swapfile)])));
        } else {
            cfg.push(Action::Run(Cmd::new("fallocate", &["-l", &size, &t(tg, swapfile)])));
            cfg.push(Action::Run(Cmd::new("chmod", &["600", &t(tg, swapfile)])));
            cfg.push(Action::Run(Cmd::new("mkswap", &["-L", "swap", &t(tg, swapfile)])));
        }
    }
    // Machine identity.
    let host = &plan.hostname;
    cfg.push(Action::Write { path: t(tg, "/etc/hostname"), contents: format!("{host}\n") });
    cfg.push(Action::Write {
        path: t(tg, "/etc/hosts"),
        contents: format!(
            "127.0.0.1\tlocalhost\n127.0.1.1\t{host}\n\n::1\tlocalhost ip6-localhost ip6-loopback\nff02::1\tip6-allnodes\nff02::2\tip6-allrouters\n"
        ),
    });
    cfg.push(Action::Remove(t(tg, "/etc/machine-id")));
    cfg.push(Action::Run(Cmd::new("systemd-machine-id-setup", &[&format!("--root={tg}")])));
    cfg.push(Action::Symlink { target: "/etc/machine-id".into(), link: t(tg, "/var/lib/dbus/machine-id") });
    cfg.push(Action::SymlinkIf {
        when: t(tg, "/usr/lib/systemd/systemd-resolved"),
        target: "../run/systemd/resolve/stub-resolv.conf".into(),
        link: t(tg, "/etc/resolv.conf"),
    });
    // Language.
    let locale = &plan.locale;
    let charset = locale.split('.').nth(1).unwrap_or("UTF-8");
    cfg.push(Action::SetLine {
        path: t(tg, "/etc/locale.gen"),
        key: locale.clone(),
        line: format!("{locale} {charset}"),
        after: None,
        optional: false,
    });
    cfg.push(Action::Run(chroot(tg, &["locale-gen", locale])));
    cfg.push(Action::Write { path: t(tg, "/etc/default/locale"), contents: format!("LANG={locale}\n") });
    // Time zone.
    let tz = &plan.timezone;
    cfg.push(Action::Require { path: t(tg, &format!("/usr/share/zoneinfo/{tz}")), message: format!("Unknown time zone {tz}.") });
    cfg.push(Action::Symlink { target: format!("/usr/share/zoneinfo/{tz}"), link: t(tg, "/etc/localtime") });
    cfg.push(Action::Write { path: t(tg, "/etc/timezone"), contents: format!("{tz}\n") });
    // Keyboard: console, Hyprland (new users) and the login screen.
    let (layout, variant) = (&plan.keyboard.layout, &plan.keyboard.variant);
    cfg.push(Action::Write {
        path: t(tg, "/etc/default/keyboard"),
        contents: format!(
            "# Set by the Tom OS installer\nXKBMODEL=\"pc105\"\nXKBLAYOUT=\"{layout}\"\nXKBVARIANT=\"{variant}\"\nXKBOPTIONS=\"\"\n\nBACKSPACE=\"guess\"\n"
        ),
    });
    let hypr = t(tg, "/etc/skel/.config/hypr/hyprland.conf");
    cfg.push(Action::SetLine { path: hypr.clone(), key: "kb_layout".into(), line: format!("    kb_layout = {layout}"), after: None, optional: true });
    cfg.push(Action::SetLine {
        path: hypr,
        key: "kb_variant".into(),
        line: format!("    kb_variant = {variant}"),
        after: Some("kb_layout".into()),
        optional: true,
    });
    let mut xkb_env = format!("XKB_DEFAULT_LAYOUT={layout} ");
    if !variant.is_empty() {
        xkb_env.push_str(&format!("XKB_DEFAULT_VARIANT={variant} "));
    }
    cfg.push(Action::Replace {
        path: t(tg, "/etc/greetd/config.toml"),
        from: "command = \"env ".into(),
        to: format!("command = \"env {xkb_env}"),
        unless: "XKB_DEFAULT_LAYOUT".into(),
    });
    steps.push(Step { title: "Setting up language, time and keyboard".into(), weight: 4.0, actions: cfg });

    // --- User ------------------------------------------------------------------------------
    let u = &plan.user;
    let mut user = vec![
        Action::AddUser {
            target: tg.into(),
            username: u.username.clone(),
            full_name: u.full_name.clone(),
            groups: USER_GROUPS.iter().map(|g| g.to_string()).collect(),
        },
        Action::Run(chroot(tg, &["chpasswd"]).with_secret_stdin(format!("{}:{}\n", u.username, u.password))),
        Action::Try(chroot(tg, &["passwd", "-l", "root"])),
        Action::Mkdir(t(tg, "/var/lib/tdm")),
        Action::Write { path: t(tg, "/var/lib/tdm/state"), contents: format!("user={}\n", u.username) },
        Action::Try(chroot(tg, &["chown", "-R", "_greetd:_greetd", "/var/lib/tdm"])),
    ];
    if u.autologin {
        user.push(Action::Append {
            path: t(tg, "/etc/greetd/config.toml"),
            contents: format!(
                "\n# Automatic login, chosen in the Tom OS installer.\n[initial_session]\ncommand = \"/usr/bin/tomos-session\"\nuser = \"{}\"\n",
                u.username
            ),
        });
    }
    steps.push(Step { title: format!("Creating {}", u.username), weight: 2.0, actions: user });

    // --- From live system to installed system -----------------------------------------------
    let mut binds = vec![
        Action::Run(Cmd::new("mount", &["--bind", "/dev", &t(tg, "/dev")])),
        Action::Run(Cmd::new("mount", &["--bind", "/dev/pts", &t(tg, "/dev/pts")])),
        Action::Run(Cmd::new("mount", &["-t", "proc", "proc", &t(tg, "/proc")])),
        Action::Run(Cmd::new("mount", &["-t", "sysfs", "sysfs", &t(tg, "/sys")])),
        Action::Run(Cmd::new("mount", &["--bind", "/run", &t(tg, "/run")])),
    ];
    if ctx.uefi {
        binds.push(Action::Try(Cmd::new("mount", &["-t", "efivarfs", "efivarfs", &t(tg, "/sys/firmware/efi/efivars")])));
    }
    let mut finish = binds;
    finish.push(Action::PurgeIfInstalled { target: tg.into(), packages: LIVE_PACKAGES.iter().map(|p| p.to_string()).collect() });
    for f in LIVE_FILES {
        finish.push(Action::Remove(t(tg, f)));
    }
    finish.push(Action::RemoveTree(t(tg, "/etc/calamares")));
    finish.push(Action::RemoveIfContains { path: t(tg, "/usr/sbin/policy-rc.d"), needle: "denied by policy".into() });
    finish.push(Action::Run(Cmd::new("systemctl", &[&format!("--root={tg}"), "enable", "greetd.service", "NetworkManager.service"])));
    finish.push(Action::Try(Cmd::new("systemctl", &[&format!("--root={tg}"), "disable", "ly.service"])));
    finish.push(Action::Try(Cmd::new("systemctl", &[&format!("--root={tg}"), "set-default", "graphical.target"])));
    steps.push(Step { title: "Removing the live session tools".into(), weight: 6.0, actions: finish });

    // --- Boot ------------------------------------------------------------------------------
    let grub = t(tg, "/etc/default/grub");
    let set = |key: &str, value: &str| Action::SetLine {
        path: grub.clone(),
        key: key.into(),
        line: format!("{key}={value}"),
        after: None,
        optional: false,
    };
    let mut boot = vec![Action::Run(chroot(tg, &["update-initramfs", "-u", "-k", "all"]))];
    boot.push(set("GRUB_DISTRIBUTOR", "\"Tom OS\""));
    boot.push(set("GRUB_TIMEOUT_STYLE", if erase { "hidden" } else { "menu" }));
    boot.push(set("GRUB_TIMEOUT", if erase { "2" } else { "5" }));
    boot.push(set("GRUB_CMDLINE_LINUX_DEFAULT", "\"quiet splash\""));
    if !erase {
        boot.push(set("GRUB_DISABLE_OS_PROBER", "false"));
    }
    let efi_needed = esp.is_some() && ctx.uefi;
    let bios_needed = bios && !ctx.uefi;
    if bios {
        let c = chroot(tg, &["grub-install", "--target=i386-pc", "--recheck", &disk]);
        boot.push(if bios_needed { Action::Run(c) } else { Action::Try(c) });
    }
    if esp.is_some() {
        let base = ["grub-install", "--target=x86_64-efi", "--efi-directory=/boot/efi", "--bootloader-id=TomOS", "--no-uefi-secure-boot"];
        // The fallback path EFI/BOOT/BOOTX64.EFI boots on any firmware, even if NVRAM is lost.
        let mut removable = base.to_vec();
        removable.extend(["--removable", "--no-nvram"]);
        let c = chroot(tg, &removable);
        boot.push(if efi_needed { Action::Run(c) } else { Action::Try(c) });
        if ctx.uefi {
            boot.push(Action::Try(chroot(tg, &base)));
        }
    }
    boot.push(Action::Run(chroot(tg, &["update-grub"])));
    steps.push(Step { title: "Installing the boot loader".into(), weight: 14.0, actions: boot });

    // --- Done ------------------------------------------------------------------------------
    steps.push(Step {
        title: "Finishing up".into(),
        weight: 2.0,
        actions: vec![
            Action::CopyLog { to: t(tg, LOG_FILE) },
            Action::Try(Cmd::new("sync", &[])),
            Action::Run(Cmd::new("umount", &["-R", tg])),
        ],
    });
    steps
}

// ============================================================================================
// Running
// ============================================================================================

/// Something that carries out actions: [`RealRunner`] or [`DryRunner`].
pub trait Runner {
    /// `progress` reports the fraction (0–1) of the current action, for long copies.
    fn run(&mut self, action: &Action, log: &mut dyn FnMut(String), progress: &mut dyn FnMut(f64)) -> Result<(), String>;
}

/// Prints what would happen.
pub struct DryRunner;

impl Runner for DryRunner {
    fn run(&mut self, action: &Action, log: &mut dyn FnMut(String), _p: &mut dyn FnMut(f64)) -> Result<(), String> {
        log(format!("[dry-run] {}", describe(action)));
        Ok(())
    }
}

/// One line describing an action.
pub fn describe(a: &Action) -> String {
    match a {
        Action::Preflight { disk, root, erase, .. } => {
            format!("preflight disk={disk} root={root} {}", if *erase { "(erase)" } else { "(existing partition)" })
        }
        Action::Run(c) => format!("$ {}", c.display()),
        Action::Try(c) => format!("$ {}  (may fail)", c.display()),
        Action::WaitFor(p) => format!("wait for {}", p.join(" ")),
        Action::Mkdir(p) => format!("mkdir -p {p}"),
        Action::Write { path, contents } => format!("write {path} ({} bytes)", contents.len()),
        Action::Append { path, contents } => format!("append to {path} ({} bytes)", contents.len()),
        Action::SetLine { path, line, .. } => format!("set in {path}: {line}"),
        Action::Replace { path, from, to, .. } => format!("edit {path}: {from:?} → {to:?}"),
        Action::Symlink { target, link } => format!("ln -sfn {target} {link}"),
        Action::SymlinkIf { when, target, link } => format!("ln -sfn {target} {link}  (if {when} exists)"),
        Action::Remove(p) => format!("rm -f {p}"),
        Action::RemoveTree(p) => format!("rm -rf {p}"),
        Action::RemoveIfContains { path, needle } => format!("rm {path} (if it contains {needle:?})"),
        Action::Require { path, .. } => format!("check {path} exists"),
        Action::CopySystem { source, target } => match source {
            Source::Squashfs(s) => format!("unsquashfs {s} → {target}"),
            Source::Dir(s) => format!("rsync {s}/ → {target}/"),
        },
        Action::Fstab { path, entries } => format!("write {path} ({} entries)", entries.len()),
        Action::AddUser { username, .. } => format!("useradd {username}"),
        Action::PurgeIfInstalled { packages, .. } => format!("dpkg --purge (installed ones of) {}", packages.join(" ")),
        Action::CopyLog { to } => format!("copy log → {to}"),
    }
}

/// Really does it. Collects everything it logs, so it can be saved in the new system.
pub struct RealRunner;

fn run_cmd(c: &Cmd, log: &mut dyn FnMut(String)) -> Result<String, String> {
    log(format!("$ {}", c.display()));
    let mut child = Command::new(&c.program)
        .args(&c.args)
        .stdin(if c.stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not start {}: {e}", c.program))?;
    if let Some(input) = &c.stdin {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(input.as_bytes());
        }
    }
    let out = child.wait_with_output().map_err(|e| format!("{} failed: {e}", c.program))?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    for line in text.lines().filter(|l| !l.trim().is_empty()).take(400) {
        log(format!("  {line}"));
    }
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let last = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
        Err(format!("{} failed ({}){}", c.program, out.status, if last.is_empty() { String::new() } else { format!(": {last}") }))
    }
}

fn io<T>(r: std::io::Result<T>, what: &str) -> Result<T, String> {
    r.map_err(|e| format!("{what}: {e}"))
}

/// Applies [`Action::SetLine`] to text.
pub fn set_line(text: &str, key: &str, line: &str, after: Option<&str>) -> String {
    let matches = |l: &str, k: &str| {
        let s = l.trim_start();
        s.starts_with(k) && s[k.len()..].chars().next().map_or(true, |c| c == '=' || c.is_whitespace())
    };
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    if let Some(i) = lines.iter().position(|l| matches(l, key)) {
        lines[i] = line.to_string();
    } else if let Some(i) = after.and_then(|a| lines.iter().position(|l| matches(l, a))) {
        lines.insert(i + 1, line.to_string());
    } else {
        lines.push(line.to_string());
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Formats `/etc/fstab`.
pub fn fstab_text(entries: &[(String, &FstabEntry)]) -> String {
    let mut out = String::from("# /etc/fstab: static file system information, written by the Tom OS installer.\n#\n# <file system>  <mount point>  <type>  <options>  <dump>  <pass>\n");
    for (spec, e) in entries {
        out.push_str(&format!("{spec}\t{}\t{}\t{}\t0\t{}\n", e.mountpoint, e.fstype, e.options, e.pass));
    }
    out
}

fn ensure_parent(path: &str) -> Result<(), String> {
    if let Some(p) = Path::new(path).parent() {
        io(fs::create_dir_all(p), &format!("Could not create {}", p.display()))?;
    }
    Ok(())
}

impl Runner for RealRunner {
    fn run(&mut self, action: &Action, log: &mut dyn FnMut(String), progress: &mut dyn FnMut(f64)) -> Result<(), String> {
        match action {
            Action::Preflight { disk, root, erase, allow_loop } => preflight(disk, root, *erase, *allow_loop, log),
            Action::Run(c) => run_cmd(c, log).map(|_| ()),
            Action::Try(c) => {
                if let Err(e) = run_cmd(c, log) {
                    log(format!("  (ignored) {e}"));
                }
                Ok(())
            }
            Action::WaitFor(paths) => {
                for _ in 0..60 {
                    if paths.iter().all(|p| Path::new(p).exists()) {
                        return Ok(());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
                Err(format!("The new partitions did not appear ({}).", paths.join(", ")))
            }
            Action::Mkdir(p) => io(fs::create_dir_all(p), &format!("Could not create {p}")),
            Action::Write { path, contents } => {
                log(format!("write {path}"));
                ensure_parent(path)?;
                io(fs::write(path, contents), &format!("Could not write {path}"))
            }
            Action::Append { path, contents } => {
                log(format!("append to {path}"));
                let mut f = io(fs::OpenOptions::new().append(true).create(true).open(path), &format!("Could not open {path}"))?;
                io(f.write_all(contents.as_bytes()), &format!("Could not write {path}"))
            }
            Action::SetLine { path, key, line, after, optional } => {
                let text = match fs::read_to_string(path) {
                    Ok(t) => t,
                    Err(_) if *optional => {
                        log(format!("skip {path} (not there)"));
                        return Ok(());
                    }
                    Err(_) => String::new(),
                };
                log(format!("set in {path}: {line}"));
                ensure_parent(path)?;
                io(fs::write(path, set_line(&text, key, line, after.as_deref())), &format!("Could not write {path}"))
            }
            Action::Replace { path, from, to, unless } => {
                let Ok(text) = fs::read_to_string(path) else {
                    log(format!("skip {path} (not there)"));
                    return Ok(());
                };
                if text.contains(unless.as_str()) || !text.contains(from.as_str()) {
                    log(format!("leave {path} as it is"));
                    return Ok(());
                }
                log(format!("edit {path}"));
                io(fs::write(path, text.replacen(from.as_str(), to, 1)), &format!("Could not write {path}"))
            }
            Action::Symlink { target, link } => {
                log(format!("ln -sfn {target} {link}"));
                ensure_parent(link)?;
                let _ = fs::remove_file(link);
                io(std::os::unix::fs::symlink(target, link), &format!("Could not link {link}"))
            }
            Action::SymlinkIf { when, target, link } => {
                if fs::symlink_metadata(when).is_ok() {
                    log(format!("ln -sfn {target} {link}"));
                    let _ = fs::remove_file(link);
                    io(std::os::unix::fs::symlink(target, link), &format!("Could not link {link}"))
                } else {
                    Ok(())
                }
            }
            Action::Remove(p) => {
                if fs::symlink_metadata(p).is_ok() {
                    log(format!("rm {p}"));
                    io(fs::remove_file(p), &format!("Could not remove {p}"))?;
                }
                Ok(())
            }
            Action::RemoveTree(p) => {
                if fs::symlink_metadata(p).is_ok() {
                    log(format!("rm -r {p}"));
                    io(fs::remove_dir_all(p), &format!("Could not remove {p}"))?;
                }
                Ok(())
            }
            Action::RemoveIfContains { path, needle } => {
                if fs::read_to_string(path).map(|t| t.contains(needle.as_str())).unwrap_or(false) {
                    log(format!("rm {path}"));
                    io(fs::remove_file(path), &format!("Could not remove {path}"))?;
                }
                Ok(())
            }
            Action::Require { path, message } => {
                if Path::new(path).exists() { Ok(()) } else { Err(message.clone()) }
            }
            Action::CopySystem { source, target } => copy_system(source, target, log, progress),
            Action::Fstab { path, entries } => {
                let mut resolved = Vec::new();
                for e in entries {
                    let spec = match e.spec.strip_prefix("probe:") {
                        Some(dev) => {
                            let uuid = run_cmd(&Cmd::new("blkid", &["-s", "UUID", "-o", "value", dev]), log)?.trim().to_string();
                            if uuid.is_empty() {
                                return Err(format!("{dev} has no UUID."));
                            }
                            format!("UUID={uuid}")
                        }
                        None => e.spec.clone(),
                    };
                    resolved.push((spec, e));
                }
                log(format!("write {path}"));
                io(fs::write(path, fstab_text(&resolved)), &format!("Could not write {path}"))
            }
            Action::AddUser { target, username, full_name, groups } => {
                let existing = fs::read_to_string(format!("{target}/etc/group")).unwrap_or_default();
                let have: Vec<&str> = existing.lines().filter_map(|l| l.split(':').next()).collect();
                let groups: Vec<&str> = groups.iter().map(|g| g.as_str()).filter(|g| have.contains(g)).collect();
                let joined = groups.join(",");
                let mut args = vec!["useradd", "-m", "-s", "/bin/bash", "-c", full_name.as_str()];
                if !joined.is_empty() {
                    args.extend(["-G", joined.as_str()]);
                }
                args.push(username);
                run_cmd(&chroot(target, &args), log).map(|_| ())
            }
            Action::PurgeIfInstalled { target, packages } => {
                let mut installed = Vec::new();
                for p in packages {
                    let c = chroot(target, &["dpkg-query", "-W", "-f=${db:Status-Status}", p]);
                    let out = Command::new(&c.program).args(&c.args).stderr(Stdio::null()).output();
                    if out.map(|o| String::from_utf8_lossy(&o.stdout).trim() == "installed").unwrap_or(false) {
                        installed.push(p.as_str());
                    }
                }
                if installed.is_empty() {
                    log("no live packages to remove".into());
                    return Ok(());
                }
                let mut args = vec!["dpkg", "--purge", "--force-depends"];
                args.extend(installed);
                run_cmd(&chroot(target, &args), log).map(|_| ())
            }
            Action::CopyLog { to } => {
                ensure_parent(to)?;
                if let Ok(text) = fs::read_to_string(LOG_FILE) {
                    let _ = fs::write(to, text);
                    let _ = fs::set_permissions(to, std::os::unix::fs::PermissionsExt::from_mode(0o600));
                }
                Ok(())
            }
        }
    }
}

/// Effective user id, from `/proc/self/status`.
pub fn euid() -> Option<u32> {
    let s = fs::read_to_string("/proc/self/status").ok()?;
    s.lines().find(|l| l.starts_with("Uid:"))?.split_whitespace().nth(2)?.parse().ok()
}

fn lsblk_value(dev: &str, col: &str) -> String {
    Command::new("lsblk")
        .args(["-n", "-d", "-b", "-o", col, dev])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn is_block(path: &str) -> bool {
    fs::metadata(path).map(|m| m.file_type().is_block_device()).unwrap_or(false)
}

/// Refuses to continue unless it is safe: root, real block devices, not the live disk, big
/// enough, and nothing in use. Desktop automounts (`/media`, `/run/media`) are unmounted and
/// swap on the disk is turned off.
fn preflight(disk: &str, root: &str, erase: bool, allow_loop: bool, log: &mut dyn FnMut(String)) -> Result<(), String> {
    if euid() != Some(0) {
        return Err("The installer backend must run as root.".into());
    }
    let devices: &[&str] = if erase { &[disk] } else { &[disk, root] };
    for dev in devices {
        if !is_block(dev) {
            return Err(format!("{dev} is not a disk."));
        }
    }
    if disk.starts_with("/dev/loop") && !allow_loop {
        return Err("Refusing to install to a loop device (use --allow-loop for testing).".into());
    }
    if let Some(live) = disks::live_disk() {
        if live == disk || root.starts_with(&live) {
            return Err(format!("{disk} holds the running live system. Choose another disk."));
        }
    }
    if lsblk_value(disk, "RO") == "1" {
        return Err(format!("{disk} is read-only."));
    }
    let size: u64 = lsblk_value(if erase { disk } else { root }, "SIZE").parse().unwrap_or(0);
    if size < MIN_TARGET_BYTES {
        return Err(format!(
            "{} is too small ({}). Tom OS needs at least {}.",
            if erase { disk } else { root },
            disks::human_size(size),
            disks::human_size(MIN_TARGET_BYTES)
        ));
    }
    log(format!("{disk}: {} OK", disks::human_size(size)));
    // What is in use? Everything on the disk (erase) or just the root partition.
    let scope = if erase { disk } else { root };
    let out = Command::new("lsblk")
        .args(["-n", "-l", "-p", "-o", "PATH,MOUNTPOINTS", scope])
        .output()
        .map_err(|e| format!("lsblk: {e}"))?;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut it = line.split_whitespace();
        let (Some(dev), Some(mp)) = (it.next(), it.next()) else { continue };
        if mp == "[SWAP]" {
            run_cmd(&Cmd::new("swapoff", &[dev]), log)?;
        } else if mp.starts_with("/media/") || mp.starts_with("/run/media/") || mp.starts_with(TARGET) {
            run_cmd(&Cmd::new("umount", &[dev]), log)?;
        } else {
            return Err(format!("{dev} is in use (mounted at {mp}). Unmount it and try again."));
        }
    }
    let _ = Command::new("umount").args(["-R", TARGET]).stderr(Stdio::null()).status();
    Ok(())
}

fn parse_percent(line: &str) -> Option<f64> {
    // unsquashfs -percentage prints bare numbers; rsync --info=progress2 prints "… 42% …".
    let l = line.trim();
    if let Ok(n) = l.parse::<f64>() {
        return Some(n / 100.0);
    }
    l.split_whitespace().find_map(|w| w.strip_suffix('%')).and_then(|n| n.parse::<f64>().ok()).map(|n| n / 100.0)
}

fn copy_system(source: &Source, target: &str, log: &mut dyn FnMut(String), progress: &mut dyn FnMut(f64)) -> Result<(), String> {
    let cmd = match source {
        Source::Squashfs(img) => Cmd::new("unsquashfs", &["-f", "-percentage", "-d", target, img]),
        Source::Dir(dir) => {
            let src = format!("{}/", dir.trim_end_matches('/'));
            let mut args = vec!["-aHAXx", "--numeric-ids", "--info=progress2", "--no-inc-recursive"];
            let ex: Vec<String> = ["/proc/*", "/sys/*", "/dev/*", "/run/*", "/tmp/*", "/mnt/*", "/media/*", "/lost+found"]
                .iter()
                .map(|e| format!("--exclude={e}"))
                .collect();
            args.extend(ex.iter().map(|s| s.as_str()));
            args.push(&src);
            let dst = format!("{target}/");
            let mut c = Cmd::new("rsync", &args);
            c.args.push(dst);
            c
        }
    };
    log(format!("$ {}", cmd.display()));
    let mut child = Command::new(&cmd.program)
        .args(&cmd.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not start {}: {e}", cmd.program))?;
    let mut stderr = child.stderr.take().unwrap();
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let stdout = child.stdout.take().unwrap();
    // rsync redraws its progress line with \r, so split on both.
    let mut reader = BufReader::new(stdout);
    let mut buf = Vec::new();
    let mut last = -1.0;
    loop {
        buf.clear();
        let mut byte = [0u8; 1];
        let mut got = false;
        while reader.read(&mut byte).map(|n| n == 1).unwrap_or(false) {
            got = true;
            if byte[0] == b'\n' || byte[0] == b'\r' {
                break;
            }
            buf.push(byte[0]);
        }
        if !got {
            break;
        }
        let line = String::from_utf8_lossy(&buf);
        if let Some(p) = parse_percent(&line) {
            if p - last >= 0.005 || p >= 1.0 {
                last = p;
                progress(p.clamp(0.0, 1.0));
            }
        } else if !line.trim().is_empty() && !line.contains("Parallel unsquashfs") {
            log(format!("  {}", line.trim()));
        }
    }
    let _ = reader.fill_buf();
    let status = child.wait().map_err(|e| e.to_string())?;
    let err = err_thread.join().unwrap_or_default();
    for l in err.lines().filter(|l| !l.trim().is_empty()).take(50) {
        log(format!("  {l}"));
    }
    if status.success() {
        progress(1.0);
        Ok(())
    } else {
        Err(format!("Copying the system failed ({status}). {}", err.lines().last().unwrap_or("")))
    }
}

/// Finds the system image to install from.
pub fn resolve_source(plan_source: Option<&str>) -> Result<Source, String> {
    let candidates: Vec<String> = match plan_source {
        Some(s) => vec![s.to_string()],
        None => SOURCE_CANDIDATES.iter().map(|s| s.to_string()).collect(),
    };
    for c in &candidates {
        let p = Path::new(c);
        if p.is_dir() {
            if !p.join("usr/bin").is_dir() {
                return Err(format!("{c} doesn't look like a Tom OS system."));
            }
            return Ok(Source::Dir(c.clone()));
        }
        if p.is_file() {
            return Ok(Source::Squashfs(c.clone()));
        }
    }
    Err(match plan_source {
        Some(s) => format!("{s} doesn't exist."),
        None => "Could not find the Tom OS system image. Run the installer from the Tom OS live USB.".into(),
    })
}

fn random_uuid() -> String {
    fs::read_to_string("/proc/sys/kernel/random/uuid").map(|s| s.trim().to_string()).unwrap_or_else(|_| {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let h = format!("{:032x}", n ^ 0x5a5a_1234_9876_abcd_ef01_2345_6789_abcd);
        format!("{}-{}-4{}-a{}-{}", &h[0..8], &h[8..12], &h[13..16], &h[17..20], &h[20..32])
    })
}

/// Can a BIOS boot loader go on this disk? (MBR, or GPT with a BIOS boot partition.)
fn bios_capable(disk: &str) -> bool {
    match lsblk_value(disk, "PTTYPE").as_str() {
        "dos" => true,
        "gpt" => Command::new("lsblk")
            .args(["-n", "-l", "-o", "PARTTYPE", disk])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).lines().any(|l| l.trim().eq_ignore_ascii_case(disks::GPT_BIOS_BOOT)))
            .unwrap_or(false),
        _ => false,
    }
}

/// Looks at the machine and makes fresh identifiers.
pub fn make_context(plan: &InstallPlan, allow_loop: bool) -> Result<Context, String> {
    let source = resolve_source(plan.source.as_deref())?;
    let esp_id = random_uuid().replace('-', "")[..8].to_uppercase();
    let bios = match &plan.storage {
        Storage::Erase { .. } => true,
        Storage::Manual { disk, .. } => bios_capable(disk),
    };
    Ok(Context {
        target: TARGET.into(),
        source,
        uefi: Path::new("/sys/firmware/efi").is_dir(),
        root_uuid: random_uuid(),
        esp_id,
        bios_capable: bios,
        allow_loop,
    })
}

/// Runs all steps, reporting [`Event`]s. On failure it unmounts the target.
pub fn execute(steps: &[Step], runner: &mut dyn Runner, emit: &mut dyn FnMut(Event), target: &str, cleanup: bool) -> Result<(), String> {
    let total_weight: f64 = steps.iter().map(|s| s.weight).sum::<f64>().max(1.0);
    let mut done_weight = 0.0;
    for (i, step) in steps.iter().enumerate() {
        emit(Event::Step { index: i, total: steps.len(), title: step.title.clone() });
        emit(Event::Log { text: format!("== {} ==", step.title) });
        let n = step.actions.len().max(1) as f64;
        for (j, action) in step.actions.iter().enumerate() {
            let base = done_weight + step.weight * j as f64 / n;
            let share = step.weight / n;
            let result = {
                let emit_cell = std::cell::RefCell::new(&mut *emit);
                let mut log = |s: String| (emit_cell.borrow_mut())(Event::Log { text: s });
                let mut progress = |p: f64| (emit_cell.borrow_mut())(Event::Progress { value: (base + share * p) / total_weight });
                runner.run(action, &mut log, &mut progress)
            };
            if let Err(e) = result {
                emit(Event::Log { text: format!("ERROR: {e}") });
                if cleanup {
                    let _ = Command::new("umount").args(["-R", target]).stderr(Stdio::null()).status();
                    let _ = Command::new("umount").args(["-R", "-l", target]).stderr(Stdio::null()).status();
                }
                emit(Event::Error { text: e.clone() });
                return Err(e);
            }
            emit(Event::Progress { value: (base + share) / total_weight });
        }
        done_weight += step.weight;
    }
    emit(Event::Progress { value: 1.0 });
    emit(Event::Done);
    Ok(())
}

/// Entry point of `tomos-installer backend`: reads the plan from stdin, prints events.
pub fn main(dry_run: bool, allow_loop: bool) -> i32 {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        eprintln!("Could not read the plan from stdin.");
        return 2;
    }
    let out = std::io::stdout();
    let mut logfile = if dry_run { None } else { fs::OpenOptions::new().create(true).append(true).open(LOG_FILE).ok() };
    let mut emit = |e: Event| {
        if let (Some(f), Event::Log { text } | Event::Error { text }) = (logfile.as_mut(), &e) {
            let _ = writeln!(f, "{text}");
        }
        let mut lock = out.lock();
        let _ = writeln!(lock, "{}", e.to_line());
        let _ = lock.flush();
    };
    let plan: InstallPlan = match serde_json::from_str(&input) {
        Ok(p) => p,
        Err(e) => {
            emit(Event::Error { text: format!("The install plan is not valid: {e}") });
            return 2;
        }
    };
    if let Err(e) = plan.validate() {
        emit(Event::Error { text: e });
        return 2;
    }
    emit(Event::Log { text: format!("Tom OS Installer {}", env!("CARGO_PKG_VERSION")) });
    emit(Event::Log { text: format!("Plan: {}", serde_json::to_string(&plan.redacted()).unwrap_or_default()) });
    let ctx = match make_context(&plan, allow_loop) {
        Ok(c) => c,
        Err(e) if dry_run => {
            emit(Event::Log { text: format!("(dry run) {e} — using a placeholder source") });
            Context {
                target: TARGET.into(),
                source: Source::Squashfs(SOURCE_CANDIDATES[0].into()),
                uefi: Path::new("/sys/firmware/efi").is_dir(),
                root_uuid: random_uuid(),
                esp_id: "ABCD1234".into(),
                bios_capable: true,
                allow_loop,
            }
        }
        Err(e) => {
            emit(Event::Error { text: e });
            return 1;
        }
    };
    emit(Event::Log { text: format!("Source: {}  ·  boot mode: {}", ctx.source.path(), if ctx.uefi { "UEFI" } else { "BIOS" }) });
    let steps = build_steps(&plan, &ctx);
    let ok = if dry_run {
        execute(&steps, &mut DryRunner, &mut emit, &ctx.target, false)
    } else {
        execute(&steps, &mut RealRunner, &mut emit, &ctx.target, true)
    };
    if ok.is_ok() { 0 } else { 1 }
}

/// For the GUI's demo mode: the same steps, simulated with small delays.
pub struct SimRunner {
    pub delay_ms: u64,
    /// Fail at the first action whose description contains this (to show the error page).
    pub fail_on: Option<String>,
}

impl Runner for SimRunner {
    fn run(&mut self, action: &Action, log: &mut dyn FnMut(String), progress: &mut dyn FnMut(f64)) -> Result<(), String> {
        log(describe(action));
        if let Some(f) = &self.fail_on {
            if describe(action).contains(f.as_str()) {
                return Err("grub-install failed (exit status: 1): cannot find EFI directory (simulated).".into());
            }
        }
        if let Action::CopySystem { .. } = action {
            for i in 1..=100 {
                std::thread::sleep(std::time::Duration::from_millis(self.delay_ms * 2));
                progress(i as f64 / 100.0);
            }
        } else {
            std::thread::sleep(std::time::Duration::from_millis(self.delay_ms));
        }
        Ok(())
    }
}

/// The steps the demo mode simulates.
pub fn demo_context() -> Context {
    Context {
        target: TARGET.into(),
        source: Source::Squashfs(SOURCE_CANDIDATES[0].into()),
        uefi: true,
        root_uuid: "6f1c2b9e-3d4a-4e5f-8a7b-0c1d2e3f4a5b".into(),
        esp_id: "7E57C0DE".into(),
        bios_capable: true,
        allow_loop: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{Keyboard, User};

    fn plan(storage: Storage, fs: Filesystem) -> InstallPlan {
        InstallPlan {
            storage,
            filesystem: fs,
            swap_mib: 2048,
            locale: "en_GB.UTF-8".into(),
            keyboard: Keyboard { layout: "gb".into(), variant: "extd".into() },
            timezone: "Europe/London".into(),
            hostname: "tomos-pc".into(),
            user: User { full_name: "Tom Smith".into(), username: "tom".into(), password: "s3cret-pass".into(), autologin: true },
            source: None,
        }
    }

    fn ctx(uefi: bool) -> Context {
        Context { uefi, ..demo_context() }
    }

    fn all_cmds(steps: &[Step]) -> Vec<String> {
        steps.iter().flat_map(|s| s.actions.iter().map(describe)).collect()
    }

    #[test]
    fn erase_ext4_uefi_sequence() {
        let steps = build_steps(&plan(Storage::Erase { disk: "/dev/nvme0n1".into() }, Filesystem::Ext4), &ctx(true));
        let cmds = all_cmds(&steps);
        let joined = cmds.join("\n");
        assert!(joined.contains("$ wipefs -a /dev/nvme0n1"));
        assert!(joined.contains("mkfs.fat -F 32 -n TOMOS-EFI -i 7E57C0DE /dev/nvme0n1p2"));
        assert!(joined.contains("mkfs.ext4 -F -q -L TomOS -U 6f1c2b9e-3d4a-4e5f-8a7b-0c1d2e3f4a5b /dev/nvme0n1p3"));
        assert!(joined.contains("mount /dev/nvme0n1p2 /mnt/tomos-target/boot/efi"));
        assert!(joined.contains("unsquashfs /run/live/medium/live/filesystem.squashfs"));
        assert!(joined.contains("grub-install --target=x86_64-efi --efi-directory=/boot/efi --bootloader-id=TomOS --no-uefi-secure-boot --removable --no-nvram"));
        assert!(joined.contains("--target=i386-pc --recheck /dev/nvme0n1  (may fail)"), "BIOS loader optional on UEFI");
        assert!(joined.contains("update-grub"));
        // The partition step comes before formatting, and preflight is first.
        assert!(matches!(steps[0].actions[0], Action::Preflight { erase: true, .. }));
        let pos = |needle: &str| cmds.iter().position(|c| c.contains(needle)).unwrap();
        assert!(pos("sfdisk") < pos("mkfs.ext4"));
        assert!(pos("mkfs.ext4") < pos("unsquashfs"));
        assert!(pos("unsquashfs") < pos("useradd"));
        assert!(pos("dpkg --purge") < pos("update-initramfs"));
        assert!(pos("update-initramfs") < pos("update-grub"));
        assert!(cmds.last().unwrap().contains("umount -R /mnt/tomos-target"));
    }

    #[test]
    fn sfdisk_script_is_gpt_with_three_partitions() {
        let steps = build_steps(&plan(Storage::Erase { disk: "/dev/sda".into() }, Filesystem::Ext4), &ctx(false));
        let sf = steps.iter().flat_map(|s| &s.actions).find_map(|a| match a {
            Action::Run(c) if c.program == "sfdisk" => Some(c.clone()),
            _ => None,
        });
        let script = sf.unwrap().stdin.unwrap();
        assert!(script.starts_with("label: gpt\n"));
        assert_eq!(script.lines().count(), 4);
        assert!(script.contains("size=1MiB, type=21686148-6449-6E6F-744E-656564454649"));
        assert!(script.contains("size=512MiB, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B"));
        // BIOS boot: grub-install i386-pc is required when booted in BIOS mode.
        let joined = all_cmds(&steps).join("\n");
        assert!(joined.contains("$ chroot /mnt/tomos-target /usr/bin/env LANG=C.UTF-8 LC_ALL=C.UTF-8 PATH=/usr/sbin:/usr/bin:/sbin:/bin DEBIAN_FRONTEND=noninteractive grub-install --target=i386-pc --recheck /dev/sda\n"));
        assert!(!joined.contains("efivarfs"));
    }

    #[test]
    fn btrfs_uses_subvolumes_and_swapfile() {
        let steps = build_steps(&plan(Storage::Erase { disk: "/dev/vda".into() }, Filesystem::Btrfs), &ctx(true));
        let joined = all_cmds(&steps).join("\n");
        assert!(joined.contains("btrfs subvolume create /mnt/tomos-target-btrfs/@home"));
        assert!(joined.contains("mount -o subvol=@,compress=zstd:1,noatime /dev/vda3 /mnt/tomos-target"));
        assert!(joined.contains("btrfs filesystem mkswapfile --size 2048M /mnt/tomos-target/swap/swapfile"));
        let fstab = steps.iter().flat_map(|s| &s.actions).find_map(|a| match a {
            Action::Fstab { entries, .. } => Some(entries.clone()),
            _ => None,
        });
        let fstab = fstab.unwrap();
        assert_eq!(fstab.iter().map(|e| e.mountpoint.as_str()).collect::<Vec<_>>(), ["/", "/home", "/swap", "/boot/efi", "none"]);
        assert_eq!(fstab[3].spec, "UUID=7E57-C0DE");
    }

    #[test]
    fn manual_mode_keeps_disk_and_probes_esp() {
        let p = plan(Storage::Manual { root: "/dev/sda5".into(), esp: Some("/dev/sda1".into()), disk: "/dev/sda".into() }, Filesystem::Ext4);
        let steps = build_steps(&p, &Context { bios_capable: false, ..ctx(true) });
        let joined = all_cmds(&steps).join("\n");
        assert!(!joined.contains("sfdisk"));
        assert!(!joined.contains("wipefs -a /dev/sda\n"));
        assert!(!joined.contains("mkfs.fat"), "an existing EFI partition is never formatted");
        assert!(!joined.contains("i386-pc"));
        assert!(joined.contains("GRUB_DISABLE_OS_PROBER=false"));
        assert!(joined.contains("GRUB_TIMEOUT_STYLE=menu"));
        let fstab = steps.iter().flat_map(|s| &s.actions).find_map(|a| match a {
            Action::Fstab { entries, .. } => Some(entries.clone()),
            _ => None,
        });
        assert!(fstab.unwrap().iter().any(|e| e.spec == "probe:/dev/sda1"));
    }

    #[test]
    fn password_is_never_shown() {
        let steps = build_steps(&plan(Storage::Erase { disk: "/dev/vda".into() }, Filesystem::Ext4), &ctx(true));
        let joined = all_cmds(&steps).join("\n");
        assert!(!joined.contains("s3cret-pass"));
        assert!(joined.contains("chpasswd  <<< (hidden)"));
        let chpasswd = steps.iter().flat_map(|s| &s.actions).find_map(|a| match a {
            Action::Run(c) if c.args.iter().any(|x| x == "chpasswd") => Some(c.clone()),
            _ => None,
        });
        assert_eq!(chpasswd.unwrap().stdin.as_deref(), Some("tom:s3cret-pass\n"));
    }

    #[test]
    fn keyboard_locale_and_autologin() {
        let steps = build_steps(&plan(Storage::Erase { disk: "/dev/vda".into() }, Filesystem::Ext4), &ctx(true));
        let acts: Vec<&Action> = steps.iter().flat_map(|s| &s.actions).collect();
        assert!(acts.iter().any(|a| matches!(a, Action::Replace { to, .. } if to == "command = \"env XKB_DEFAULT_LAYOUT=gb XKB_DEFAULT_VARIANT=extd ")));
        assert!(acts.iter().any(|a| matches!(a, Action::SetLine { line, .. } if line == "    kb_variant = extd")));
        assert!(acts.iter().any(|a| matches!(a, Action::SetLine { line, .. } if line == "en_GB.UTF-8 UTF-8")));
        assert!(acts.iter().any(|a| matches!(a, Action::Append { contents, .. } if contents.contains("[initial_session]") && contents.contains("user = \"tom\""))));
        assert!(acts.iter().any(|a| matches!(a, Action::Symlink { target, .. } if target == "/usr/share/zoneinfo/Europe/London")));
    }

    #[test]
    fn set_line_replaces_inserts_and_appends() {
        let conf = "input {\n    kb_layout = us\n    follow_mouse = 1\n}\n";
        assert_eq!(set_line(conf, "kb_layout", "    kb_layout = gb", None), "input {\n    kb_layout = gb\n    follow_mouse = 1\n}\n");
        assert_eq!(
            set_line(conf, "kb_variant", "    kb_variant = extd", Some("kb_layout")),
            "input {\n    kb_layout = us\n    kb_variant = extd\n    follow_mouse = 1\n}\n"
        );
        // Prefix of another key doesn't match: GRUB_TIMEOUT vs GRUB_TIMEOUT_STYLE.
        let grub = "GRUB_TIMEOUT_STYLE=hidden\nGRUB_TIMEOUT=0\n";
        assert_eq!(set_line(grub, "GRUB_TIMEOUT", "GRUB_TIMEOUT=2", None), "GRUB_TIMEOUT_STYLE=hidden\nGRUB_TIMEOUT=2\n");
        assert_eq!(set_line("", "A", "A=1", None), "A=1\n");
    }

    #[test]
    fn fstab_format() {
        let e = FstabEntry { spec: String::new(), mountpoint: "/".into(), fstype: "ext4".into(), options: "errors=remount-ro".into(), pass: 1 };
        let text = fstab_text(&[("UUID=abc".into(), &e)]);
        assert!(text.ends_with("UUID=abc\t/\text4\terrors=remount-ro\t0\t1\n"));
    }

    #[test]
    fn percent_parsing() {
        assert_eq!(parse_percent("42"), Some(0.42));
        assert_eq!(parse_percent("  1,234,567  37%   12.3MB/s    0:00:10 (xfr#1, to-chk=0/3)"), Some(0.37));
        assert_eq!(parse_percent("created 1234 files"), None);
    }

    #[test]
    fn dry_run_emits_steps_and_done() {
        let steps = build_steps(&plan(Storage::Erase { disk: "/dev/vda".into() }, Filesystem::Ext4), &ctx(true));
        let mut events = Vec::new();
        execute(&steps, &mut DryRunner, &mut |e| events.push(e), TARGET, false).unwrap();
        assert_eq!(events.last(), Some(&Event::Done));
        let n_steps = events.iter().filter(|e| matches!(e, Event::Step { .. })).count();
        assert_eq!(n_steps, steps.len());
        let mut last = 0.0;
        for e in &events {
            if let Event::Progress { value } = e {
                assert!(*value + 1e-9 >= last && *value <= 1.0 + 1e-9);
                last = *value;
            }
        }
    }

    #[test]
    fn failure_stops_and_reports() {
        struct FailOn(&'static str);
        impl Runner for FailOn {
            fn run(&mut self, a: &Action, _l: &mut dyn FnMut(String), _p: &mut dyn FnMut(f64)) -> Result<(), String> {
                if describe(a).contains(self.0) { Err("nope".into()) } else { Ok(()) }
            }
        }
        let steps = build_steps(&plan(Storage::Erase { disk: "/dev/vda".into() }, Filesystem::Ext4), &ctx(true));
        let mut events = Vec::new();
        assert!(execute(&steps, &mut FailOn("mkfs.ext4"), &mut |e| events.push(e), TARGET, false).is_err());
        assert_eq!(events.last(), Some(&Event::Error { text: "nope".into() }));
        assert!(!events.iter().any(|e| matches!(e, Event::Log { text } if text.contains("unsquashfs"))));
    }
}

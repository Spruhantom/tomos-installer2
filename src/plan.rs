//! The install plan: everything the user chose, as one JSON document.
//!
//! The GUI builds an [`InstallPlan`], validates it and sends it to the root backend on stdin
//! (never through a file, because it contains the password). The backend validates it again
//! before touching any disk.

use serde::{Deserialize, Serialize};

/// Smallest disk or root partition Tom OS installs to (bytes). The installed system is
/// about 5.5 GB; this leaves room for a swap file, updates and your files.
pub const MIN_TARGET_BYTES: u64 = 12_000_000_000;

/// Groups the first user joins (the ones that don't exist on the target are skipped).
pub const USER_GROUPS: &[&str] =
    &["adm", "cdrom", "dip", "lpadmin", "plugdev", "sudo", "audio", "video", "netdev", "users"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Filesystem {
    #[default]
    Ext4,
    Btrfs,
}

impl Filesystem {
    pub fn label(self) -> &'static str {
        match self {
            Filesystem::Ext4 => "ext4",
            Filesystem::Btrfs => "btrfs",
        }
    }
}

/// How the disk is used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum Storage {
    /// Erase the whole disk: GPT with a BIOS boot, an EFI system and a Tom OS partition.
    Erase { disk: String },
    /// Install to an existing partition (it is formatted); optionally reuse an EFI partition.
    Manual {
        root: String,
        /// Existing EFI system partition to mount at /boot/efi (not formatted), if any.
        #[serde(default)]
        esp: Option<String>,
        /// The disk that holds `root`, for the BIOS boot loader.
        disk: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Keyboard {
    pub layout: String,
    #[serde(default)]
    pub variant: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub full_name: String,
    pub username: String,
    /// Plain text, only ever held in memory and passed to `chpasswd` on stdin.
    pub password: String,
    #[serde(default)]
    pub autologin: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallPlan {
    pub storage: Storage,
    #[serde(default)]
    pub filesystem: Filesystem,
    /// Swap file size in MiB (0 = none).
    #[serde(default)]
    pub swap_mib: u32,
    pub locale: String,
    pub keyboard: Keyboard,
    pub timezone: String,
    pub hostname: String,
    pub user: User,
    /// Where the system comes from: a squashfs image or a directory. `None` finds the live
    /// system's image automatically.
    #[serde(default)]
    pub source: Option<String>,
}

impl Default for InstallPlan {
    fn default() -> Self {
        InstallPlan {
            storage: Storage::Erase { disk: String::new() },
            filesystem: Filesystem::Ext4,
            swap_mib: 2048,
            locale: "en_GB.UTF-8".into(),
            keyboard: Keyboard { layout: "gb".into(), variant: String::new() },
            timezone: "Europe/London".into(),
            hostname: "tomos".into(),
            user: User { full_name: String::new(), username: String::new(), password: String::new(), autologin: false },
            source: None,
        }
    }
}

/// Reserved names `useradd` would accept but that would clash with system accounts.
const RESERVED_USERS: &[&str] = &[
    "root", "daemon", "bin", "sys", "sync", "games", "man", "lp", "mail", "news", "uucp", "proxy",
    "www-data", "backup", "list", "irc", "nobody", "systemd-network", "systemd-resolve",
    "messagebus", "syslog", "_greetd", "greeter", "polkitd", "admin", "sudo", "users",
];

/// Checks a Linux username: lowercase letter first, then lowercase letters, digits, `-` or `_`,
/// at most 32 characters, and not a system account.
pub fn check_username(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Choose a username.".into());
    }
    if name.len() > 32 {
        return Err("The username can be at most 32 characters.".into());
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_lowercase() {
        return Err("The username must start with a lowercase letter.".into());
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
        return Err("Use only lowercase letters, digits, - and _ in the username.".into());
    }
    if RESERVED_USERS.contains(&name) {
        return Err(format!("\"{name}\" is reserved by the system. Choose another username."));
    }
    Ok(())
}

/// Checks a hostname (RFC 1123 label): letters, digits and `-`, 1–63 characters, not starting
/// or ending with `-`.
pub fn check_hostname(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Choose a computer name.".into());
    }
    if name.len() > 63 {
        return Err("The computer name can be at most 63 characters.".into());
    }
    if name.starts_with('-') || name.ends_with('-') {
        return Err("The computer name can't start or end with -.".into());
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err("Use only letters, digits and - in the computer name.".into());
    }
    Ok(())
}

pub fn check_password(password: &str, confirm: &str) -> Result<(), String> {
    if password.chars().count() < 6 {
        return Err("The password needs at least 6 characters.".into());
    }
    if password.contains('\n') || password.contains('\r') {
        return Err("The password can't contain line breaks.".into());
    }
    if password != confirm {
        return Err("The passwords don't match.".into());
    }
    Ok(())
}

/// 0 (weak) to 4 (strong): length plus character variety. Only a hint for the meter.
pub fn password_strength(password: &str) -> u8 {
    let len = password.chars().count();
    if len == 0 {
        return 0;
    }
    let classes = [
        password.chars().any(|c| c.is_lowercase()),
        password.chars().any(|c| c.is_uppercase()),
        password.chars().any(|c| c.is_ascii_digit()),
        password.chars().any(|c| !c.is_alphanumeric()),
    ]
    .iter()
    .filter(|b| **b)
    .count();
    let mut score: u8 = match len {
        0..=5 => 0,
        6..=8 => 1,
        9..=11 => 2,
        _ => 3,
    };
    if classes >= 3 {
        score += 1;
    }
    score.min(4)
}

/// Suggests a username from a full name: "Tom Smith" → "tom".
pub fn suggest_username(full_name: &str) -> String {
    let first = full_name.split_whitespace().next().unwrap_or("");
    let mut s: String = first
        .chars()
        .filter_map(|c| {
            let c = c.to_ascii_lowercase();
            (c.is_ascii_lowercase() || c.is_ascii_digit()).then_some(c)
        })
        .collect();
    while s.starts_with(|c: char| c.is_ascii_digit()) {
        s.remove(0);
    }
    s.truncate(32);
    s
}

/// A device path that is safe to pass to tools: `/dev/` followed by letters, digits, `/`, `-`,
/// `_`, `.` or `:` (no spaces, no `..`).
pub fn check_device(path: &str) -> Result<(), String> {
    let ok = path.starts_with("/dev/")
        && path.len() > 5
        && !path.contains("..")
        && path[5..].chars().all(|c| c.is_ascii_alphanumeric() || "/-_.:".contains(c));
    if ok { Ok(()) } else { Err(format!("\"{path}\" is not a device path.")) }
}

/// A locale such as `en_GB.UTF-8`.
pub fn check_locale(locale: &str) -> Result<(), String> {
    let ok = !locale.is_empty()
        && locale.len() <= 64
        && locale.chars().all(|c| c.is_ascii_alphanumeric() || "_.-@".contains(c));
    if ok { Ok(()) } else { Err(format!("\"{locale}\" is not a locale.")) }
}

/// A time zone such as `Europe/London` (the backend also checks that it exists).
pub fn check_timezone(tz: &str) -> Result<(), String> {
    let ok = !tz.is_empty()
        && !tz.starts_with('/')
        && !tz.contains("..")
        && tz.chars().all(|c| c.is_ascii_alphanumeric() || "/_-+".contains(c));
    if ok { Ok(()) } else { Err(format!("\"{tz}\" is not a time zone.")) }
}

/// An XKB layout or variant name such as `gb` or `extd`.
pub fn check_xkb(name: &str, what: &str) -> Result<(), String> {
    let ok = name.len() <= 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || "_-()".contains(c));
    if ok { Ok(()) } else { Err(format!("\"{name}\" is not a keyboard {what}.")) }
}

impl InstallPlan {
    /// Checks everything that can be checked without looking at the machine.
    pub fn validate(&self) -> Result<(), String> {
        match &self.storage {
            Storage::Erase { disk } => {
                if disk.is_empty() {
                    return Err("Choose a disk to install Tom OS on.".into());
                }
                check_device(disk)?;
            }
            Storage::Manual { root, esp, disk } => {
                check_device(root)?;
                check_device(disk)?;
                if let Some(esp) = esp {
                    check_device(esp)?;
                    if esp == root {
                        return Err("The EFI partition and the Tom OS partition must be different.".into());
                    }
                }
            }
        }
        if self.swap_mib > 64 * 1024 {
            return Err("The swap file can be at most 64 GiB.".into());
        }
        check_locale(&self.locale)?;
        check_timezone(&self.timezone)?;
        check_xkb(&self.keyboard.layout, "layout")?;
        if self.keyboard.layout.is_empty() {
            return Err("Choose a keyboard layout.".into());
        }
        check_xkb(&self.keyboard.variant, "variant")?;
        check_hostname(&self.hostname)?;
        check_username(&self.user.username)?;
        if self.user.full_name.contains([':', ',', '\n']) {
            return Err("The name can't contain : , or line breaks.".into());
        }
        check_password(&self.user.password, &self.user.password)?;
        if let Some(src) = &self.source {
            if !src.starts_with('/') {
                return Err("The source must be an absolute path.".into());
            }
        }
        Ok(())
    }

    /// The plan without the password, for logs and the summary.
    pub fn redacted(&self) -> InstallPlan {
        let mut p = self.clone();
        p.user.password = "********".into();
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good() -> InstallPlan {
        InstallPlan {
            storage: Storage::Erase { disk: "/dev/vda".into() },
            user: User { full_name: "Tom".into(), username: "tom".into(), password: "secret1".into(), autologin: false },
            ..Default::default()
        }
    }

    #[test]
    fn a_complete_plan_validates() {
        assert_eq!(good().validate(), Ok(()));
    }

    #[test]
    fn usernames() {
        assert!(check_username("tom").is_ok());
        assert!(check_username("tom_2-x").is_ok());
        assert!(check_username("").is_err());
        assert!(check_username("Tom").is_err());
        assert!(check_username("2tom").is_err());
        assert!(check_username("to m").is_err());
        assert!(check_username("root").is_err());
        assert!(check_username(&"a".repeat(33)).is_err());
    }

    #[test]
    fn hostnames() {
        assert!(check_hostname("tomos").is_ok());
        assert!(check_hostname("Tom-PC2").is_ok());
        assert!(check_hostname("-x").is_err());
        assert!(check_hostname("a b").is_err());
        assert!(check_hostname("a.b").is_err());
    }

    #[test]
    fn passwords() {
        assert!(check_password("secret1", "secret1").is_ok());
        assert!(check_password("short", "short").is_err());
        assert!(check_password("secret1", "secret2").is_err());
        assert!(check_password("sec\nret1", "sec\nret1").is_err());
        assert_eq!(password_strength(""), 0);
        assert!(password_strength("abcdefghijklmnop") > password_strength("abcdef"));
        assert_eq!(password_strength("Neon-Punk-2026!"), 4);
    }

    #[test]
    fn devices_are_checked() {
        assert!(check_device("/dev/nvme0n1").is_ok());
        assert!(check_device("/dev/disk/by-id/ata-X_1").is_ok());
        assert!(check_device("/dev/sda; rm -rf /").is_err());
        assert!(check_device("/dev/../etc/passwd").is_err());
        assert!(check_device("sda").is_err());
        let mut p = good();
        p.storage = Storage::Erase { disk: "/dev/sda1 x".into() };
        assert!(p.validate().is_err());
    }

    #[test]
    fn an_empty_disk_is_rejected() {
        let mut p = good();
        p.storage = Storage::Erase { disk: String::new() };
        assert_eq!(p.validate(), Err("Choose a disk to install Tom OS on.".into()));
    }

    #[test]
    fn manual_mode_needs_distinct_partitions() {
        let mut p = good();
        p.storage = Storage::Manual { root: "/dev/sda2".into(), esp: Some("/dev/sda2".into()), disk: "/dev/sda".into() };
        assert!(p.validate().is_err());
        p.storage = Storage::Manual { root: "/dev/sda2".into(), esp: Some("/dev/sda1".into()), disk: "/dev/sda".into() };
        assert!(p.validate().is_ok());
    }

    #[test]
    fn other_fields_are_checked() {
        let mut p = good();
        p.timezone = "../../etc/passwd".into();
        assert!(p.validate().is_err());
        let mut p = good();
        p.keyboard.layout = "gb;x".into();
        assert!(p.validate().is_err());
        let mut p = good();
        p.user.full_name = "Tom:0".into();
        assert!(p.validate().is_err());
    }

    #[test]
    fn suggestions() {
        assert_eq!(suggest_username("Tom Smith"), "tom");
        assert_eq!(suggest_username("  Zoë "), "zo");
        assert_eq!(suggest_username("3D Artist"), "d");
        assert_eq!(suggest_username(""), "");
    }

    #[test]
    fn json_round_trip_and_redaction() {
        let p = good();
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("\"mode\":\"erase\""));
        let back: InstallPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
        assert_eq!(p.redacted().user.password, "********");
    }
}

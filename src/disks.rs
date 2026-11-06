//! Disks and partitions, from `lsblk --json`.

use serde::Serialize;
use serde_json::Value;
use std::process::Command;

/// GPT partition type GUIDs.
pub const GPT_BIOS_BOOT: &str = "21686148-6449-6e6f-744e-656564454649";
pub const GPT_ESP: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
pub const GPT_LINUX: &str = "0fc63daf-8483-4772-8e79-3d69d8477de4";
/// MBR type of an EFI system partition.
const MBR_ESP: &str = "0xef";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Partition {
    pub path: String,
    pub size: u64,
    pub fstype: String,
    pub label: String,
    pub parttype: String,
    pub mountpoints: Vec<String>,
}

impl Partition {
    pub fn is_esp(&self) -> bool {
        let t = self.parttype.to_ascii_lowercase();
        t == GPT_ESP || t == MBR_ESP
    }
    pub fn is_bios_boot(&self) -> bool {
        self.parttype.eq_ignore_ascii_case(GPT_BIOS_BOOT)
    }
    pub fn mounted(&self) -> bool {
        !self.mountpoints.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Disk {
    pub path: String,
    pub size: u64,
    pub model: String,
    /// sata, nvme, usb, virtio, …
    pub transport: String,
    pub removable: bool,
    pub read_only: bool,
    pub kind: String,
    pub partitions: Vec<Partition>,
    /// This disk holds the running live system.
    pub live_medium: bool,
}

impl Disk {
    /// A name for the disk list: "Samsung SSD 980 · 1.0 TB · NVMe".
    pub fn title(&self) -> String {
        let model = if self.model.trim().is_empty() { "Disk".to_string() } else { self.model.trim().to_string() };
        let mut parts = vec![model, human_size(self.size)];
        let t = transport_name(&self.transport);
        if !t.is_empty() {
            parts.push(t);
        }
        parts.join(" · ")
    }

    /// Why Tom OS can't go on this disk, if it can't.
    pub fn unusable_reason(&self) -> Option<String> {
        if self.live_medium {
            return Some("This is the Tom OS live USB you are running from.".into());
        }
        if self.read_only {
            return Some("This disk is read-only.".into());
        }
        if self.size < crate::plan::MIN_TARGET_BYTES {
            return Some(format!("Too small: Tom OS needs at least {}.", human_size(crate::plan::MIN_TARGET_BYTES)));
        }
        None
    }

    pub fn summary(&self) -> String {
        if self.partitions.is_empty() {
            return "No partitions".into();
        }
        let n = self.partitions.len();
        let fs: Vec<String> = self
            .partitions
            .iter()
            .filter(|p| !p.fstype.is_empty())
            .map(|p| if p.label.is_empty() { p.fstype.clone() } else { format!("{} \"{}\"", p.fstype, p.label) })
            .collect();
        let head = if n == 1 { "1 partition".to_string() } else { format!("{n} partitions") };
        if fs.is_empty() { head } else { format!("{head}: {}", fs.join(", ")) }
    }
}

pub fn transport_name(t: &str) -> String {
    match t {
        "nvme" => "NVMe".into(),
        "sata" | "ata" => "SATA".into(),
        "usb" => "USB".into(),
        "virtio" => "VirtIO".into(),
        "mmc" => "SD/eMMC".into(),
        "scsi" => "SCSI".into(),
        "" => String::new(),
        other => other.to_uppercase(),
    }
}

/// Decimal sizes, like disk makers and file managers use: 512 GB, 1.0 TB.
pub fn human_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e12 {
        format!("{:.1} TB", b / 1e12)
    } else if b >= 1e9 {
        format!("{:.0} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.0} MB", b / 1e6)
    } else {
        format!("{bytes} B")
    }
}

/// The path of partition `n` on `disk`: `/dev/sda` → `/dev/sda1`, `/dev/nvme0n1` → `/dev/nvme0n1p1`.
pub fn part_path(disk: &str, n: u32) -> String {
    if disk.ends_with(|c: char| c.is_ascii_digit()) {
        format!("{disk}p{n}")
    } else {
        format!("{disk}{n}")
    }
}

fn s(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn u(v: &Value, key: &str) -> u64 {
    match v.get(key) {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

fn b(v: &Value, key: &str) -> bool {
    match v.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "1" || s == "true",
        Some(Value::Number(n)) => n.as_u64() == Some(1),
        _ => false,
    }
}

fn mountpoints(v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(Value::Array(a)) = v.get("mountpoints") {
        out.extend(a.iter().filter_map(|m| m.as_str().map(str::to_owned)));
    }
    if let Some(Value::String(m)) = v.get("mountpoint") {
        out.push(m.clone());
    }
    out
}

/// Collects the partitions under a block device (recursively, so LVM/crypt children still
/// mark their parent as in use).
fn collect_parts(v: &Value, out: &mut Vec<Partition>) {
    if let Some(Value::Array(children)) = v.get("children") {
        for c in children {
            if s(c, "type") == "part" {
                let mut mps = mountpoints(c);
                // A mounted LVM volume or LUKS mapping inside the partition counts as mounted.
                let mut inner = Vec::new();
                collect_any_mounts(c, &mut inner);
                mps.extend(inner);
                out.push(Partition {
                    path: s(c, "path"),
                    size: u(c, "size"),
                    fstype: s(c, "fstype"),
                    label: s(c, "label"),
                    parttype: s(c, "parttype"),
                    mountpoints: mps,
                });
            }
        }
    }
}

fn collect_any_mounts(v: &Value, out: &mut Vec<String>) {
    if let Some(Value::Array(children)) = v.get("children") {
        for c in children {
            out.extend(mountpoints(c));
            collect_any_mounts(c, out);
        }
    }
}

/// Parses `lsblk -J -b -o NAME,PATH,TYPE,SIZE,MODEL,TRAN,RM,RO,FSTYPE,LABEL,PARTTYPE,MOUNTPOINTS`.
/// `allow_loop` also lists loop devices (for testing with disk images).
pub fn parse_lsblk(json: &str, live_disk: Option<&str>, allow_loop: bool) -> Result<Vec<Disk>, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("lsblk output: {e}"))?;
    let mut disks = Vec::new();
    for d in v.get("blockdevices").and_then(Value::as_array).into_iter().flatten() {
        let kind = s(d, "type");
        if !(kind == "disk" || (allow_loop && kind == "loop")) {
            continue;
        }
        let path = s(d, "path");
        let name = s(d, "name");
        // zram swap, floppies and empty card readers aren't install targets.
        if name.starts_with("zram") || name.starts_with("fd") || u(d, "size") == 0 {
            continue;
        }
        let mut partitions = Vec::new();
        collect_parts(d, &mut partitions);
        let mut whole_mounts = mountpoints(d);
        if partitions.is_empty() {
            collect_any_mounts(d, &mut whole_mounts);
        }
        let live = live_disk.is_some_and(|l| l == path)
            || whole_mounts.iter().chain(partitions.iter().flat_map(|p| p.mountpoints.iter())).any(|m| {
                m == "/run/live/medium" || m == "/cdrom" || m == "/run/initramfs/live" || m == "/"
            });
        disks.push(Disk {
            path,
            size: u(d, "size"),
            model: s(d, "model"),
            transport: s(d, "tran"),
            removable: b(d, "rm"),
            read_only: b(d, "ro"),
            kind,
            partitions,
            live_medium: live,
        });
    }
    Ok(disks)
}

/// The disk that holds the live medium (`/run/live/medium`), if we're running live.
pub fn live_disk() -> Option<String> {
    let out = Command::new("findmnt").args(["-n", "-o", "SOURCE", "/run/live/medium"]).output().ok()?;
    let src = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if src.is_empty() {
        return None;
    }
    let out = Command::new("lsblk").args(["-n", "-d", "-o", "PKNAME", &src]).output().ok()?;
    let parent = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Some(if parent.is_empty() { src } else { format!("/dev/{parent}") })
}

/// Lists the disks on this computer.
pub fn list(allow_loop: bool) -> Result<Vec<Disk>, String> {
    let out = Command::new("lsblk")
        .args(["-J", "-b", "-o", "NAME,PATH,TYPE,SIZE,MODEL,TRAN,RM,RO,FSTYPE,LABEL,PARTTYPE,MOUNTPOINTS"])
        .output()
        .map_err(|e| format!("Could not run lsblk: {e}"))?;
    if !out.status.success() {
        return Err(format!("lsblk failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    parse_lsblk(&String::from_utf8_lossy(&out.stdout), live_disk().as_deref(), allow_loop)
}

/// Sample disks for `--demo` (screenshots and trying the installer without real disks).
pub fn demo_disks() -> Vec<Disk> {
    let p = |path: &str, size: u64, fstype: &str, label: &str, parttype: &str| Partition {
        path: path.into(),
        size,
        fstype: fstype.into(),
        label: label.into(),
        parttype: parttype.into(),
        mountpoints: vec![],
    };
    vec![
        Disk {
            path: "/dev/nvme0n1".into(),
            size: 1_000_204_886_016,
            model: "Samsung SSD 980 PRO 1TB".into(),
            transport: "nvme".into(),
            removable: false,
            read_only: false,
            kind: "disk".into(),
            partitions: vec![
                p("/dev/nvme0n1p1", 104_857_600, "vfat", "SYSTEM", GPT_ESP),
                p("/dev/nvme0n1p2", 16_777_216, "", "", "e3c9e316-0b5c-4db8-817d-f92df00215ae"),
                p("/dev/nvme0n1p3", 499_000_000_000, "ntfs", "Windows", "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7"),
                p("/dev/nvme0n1p4", 500_000_000_000, "ext4", "data", GPT_LINUX),
            ],
            live_medium: false,
        },
        Disk {
            path: "/dev/sda".into(),
            size: 256_060_514_304,
            model: "Crucial MX500".into(),
            transport: "sata".into(),
            removable: false,
            read_only: false,
            kind: "disk".into(),
            partitions: vec![],
            live_medium: false,
        },
        Disk {
            path: "/dev/sdb".into(),
            size: 32_017_047_552,
            model: "SanDisk Ultra".into(),
            transport: "usb".into(),
            removable: true,
            read_only: false,
            kind: "disk".into(),
            partitions: vec![p("/dev/sdb1", 32_000_000_000, "iso9660", "TOMOS_2610", "0x0")],
            live_medium: true,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const LSBLK: &str = r#"{
   "blockdevices": [
      {"name":"loop0","path":"/dev/loop0","type":"loop","size":2539282432,"model":null,"tran":null,"rm":false,"ro":true,"fstype":"squashfs","label":null,"parttype":null,"mountpoints":["/run/live/rootfs/filesystem.squashfs"]},
      {"name":"sr0","path":"/dev/sr0","type":"rom","size":2761211904,"model":"QEMU DVD-ROM","tran":"ata","rm":true,"ro":false,"fstype":"iso9660","label":"TOMOS_2610","parttype":null,"mountpoints":["/run/live/medium"]},
      {"name":"sda","path":"/dev/sda","type":"disk","size":32017047552,"model":"SanDisk Ultra","tran":"usb","rm":true,"ro":false,"fstype":"iso9660","label":"TOMOS_2610","parttype":null,"mountpoints":[null],
         "children": [
            {"name":"sda1","path":"/dev/sda1","type":"part","size":2761211904,"model":null,"tran":null,"rm":true,"ro":false,"fstype":"iso9660","label":"TOMOS_2610","parttype":"0x0","mountpoints":["/run/live/medium"]}
         ]
      },
      {"name":"nvme0n1","path":"/dev/nvme0n1","type":"disk","size":512110190592,"model":"WD Blue SN580","tran":"nvme","rm":false,"ro":false,"fstype":null,"label":null,"parttype":null,"mountpoints":[null],
         "children": [
            {"name":"nvme0n1p1","path":"/dev/nvme0n1p1","type":"part","size":536870912,"model":null,"tran":"nvme","rm":false,"ro":false,"fstype":"vfat","label":"EFI","parttype":"c12a7328-f81f-11d2-ba4b-00a0c93ec93b","mountpoints":[null]},
            {"name":"nvme0n1p2","path":"/dev/nvme0n1p2","type":"part","size":511571771392,"model":null,"tran":"nvme","rm":false,"ro":false,"fstype":"ext4","label":"home","parttype":"0fc63daf-8483-4772-8e79-3d69d8477de4","mountpoints":["/media/tom/home"]}
         ]
      },
      {"name":"vda","path":"/dev/vda","type":"disk","size":8589934592,"model":null,"tran":"virtio","rm":false,"ro":false,"fstype":null,"label":null,"parttype":null,"mountpoints":[null]},
      {"name":"zram0","path":"/dev/zram0","type":"disk","size":4096000000,"model":null,"tran":null,"rm":false,"ro":false,"fstype":"swap","label":null,"parttype":null,"mountpoints":["[SWAP]"]}
   ]
}"#;

    #[test]
    fn lsblk_is_parsed() {
        let disks = parse_lsblk(LSBLK, None, false).unwrap();
        let paths: Vec<&str> = disks.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, ["/dev/sda", "/dev/nvme0n1", "/dev/vda"], "no loop, rom or zram");
        let nvme = &disks[1];
        assert_eq!(nvme.partitions.len(), 2);
        assert!(nvme.partitions[0].is_esp());
        assert!(!nvme.partitions[0].mounted(), "null mount points are ignored");
        assert!(nvme.partitions[1].mounted());
        assert_eq!(nvme.title(), "WD Blue SN580 · 512 GB · NVMe");
        assert_eq!(nvme.summary(), "2 partitions: vfat \"EFI\", ext4 \"home\"");
        assert!(nvme.unusable_reason().is_none());
    }

    #[test]
    fn the_live_usb_and_small_disks_are_unusable() {
        let disks = parse_lsblk(LSBLK, None, false).unwrap();
        assert!(disks[0].live_medium, "the disk holding /run/live/medium");
        assert!(disks[0].unusable_reason().unwrap().contains("live USB"));
        assert!(disks[2].unusable_reason().unwrap().contains("Too small"));
        let by_name = parse_lsblk(LSBLK, Some("/dev/nvme0n1"), false).unwrap();
        assert!(by_name[1].live_medium);
    }

    #[test]
    fn loops_only_when_asked() {
        let disks = parse_lsblk(LSBLK, None, true).unwrap();
        assert_eq!(disks[0].path, "/dev/loop0");
    }

    #[test]
    fn partition_names() {
        assert_eq!(part_path("/dev/sda", 3), "/dev/sda3");
        assert_eq!(part_path("/dev/vda", 1), "/dev/vda1");
        assert_eq!(part_path("/dev/nvme0n1", 2), "/dev/nvme0n1p2");
        assert_eq!(part_path("/dev/mmcblk0", 1), "/dev/mmcblk0p1");
        assert_eq!(part_path("/dev/loop7", 3), "/dev/loop7p3");
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(512_110_190_592), "512 GB");
        assert_eq!(human_size(1_000_204_886_016), "1.0 TB");
        assert_eq!(human_size(536_870_912), "537 MB");
    }

    #[test]
    fn old_lsblk_strings_still_parse() {
        let json = r#"{"blockdevices":[{"name":"sda","path":"/dev/sda","type":"disk","size":"64000000000","model":"X","tran":"sata","rm":"0","ro":"0","mountpoint":null}]}"#;
        let d = parse_lsblk(json, None, false).unwrap();
        assert_eq!(d[0].size, 64_000_000_000);
        assert!(!d[0].removable);
    }
}

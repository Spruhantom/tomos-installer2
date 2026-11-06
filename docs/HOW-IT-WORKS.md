# How the Tom OS Installer works

The installer is one program, `tomos-installer`, with two halves:

| Half | Runs as | Job |
|------|---------|-----|
| **Window** (`src/gui/`) | the live user | Asks the questions, builds an *install plan*, shows progress. Never touches a disk. |
| **Backend** (`src/backend.rs`) | root | Reads the plan from stdin, checks it again, runs the install, prints progress as JSON lines. |

The window starts the backend with `sudo -n` (the live user has passwordless sudo), or `pkexec` if that isn't allowed, or directly when it is already root. The plan is sent on **stdin**, never written to a file, because it contains the password.

## The install plan

```json
{
  "storage":    { "mode": "erase", "disk": "/dev/nvme0n1" },
  "filesystem": "ext4",
  "swap_mib":   2048,
  "locale":     "en_GB.UTF-8",
  "keyboard":   { "layout": "gb", "variant": "" },
  "timezone":   "Europe/London",
  "hostname":   "tom-tomos",
  "user":       { "full_name": "Tom Smith", "username": "tom", "password": "…", "autologin": false },
  "source":     null
}
```

`storage` can also be `{ "mode": "manual", "root": "/dev/sda5", "esp": "/dev/sda1", "disk": "/dev/sda" }` to install to an existing partition (formatted) and reuse an existing EFI partition (kept as it is). `filesystem` is `ext4` or `btrfs`. `source` is `null` to use the live USB's `live/filesystem.squashfs`, or a path to a squashfs image or a root directory.

Check a plan without installing:

```sh
tomos-installer check-plan < plan.json
sudo tomos-installer backend --dry-run < plan.json     # prints every step and command
```

## Steps

Every step is a list of explicit actions built by `build_steps()`; `--dry-run` prints them and the unit tests check them.

| # | Step | What happens |
|---|------|--------------|
| 1 | Checking the computer | Must be root; the disk must be a real block device, not the live USB, not read-only, at least 12 GB. Desktop automounts under `/media` are unmounted and swap on the disk is turned off; anything else that's mounted stops the install. |
| 2 | Partitioning (erase only) | `wipefs -a`, then `sfdisk` writes GPT: **1 MiB BIOS boot**, **512 MiB EFI system**, **rest Tom OS**. |
| 3 | Formatting | `mkfs.fat -F 32 -n TOMOS-EFI` (erase only — an existing EFI partition is never formatted) and `mkfs.ext4 -L TomOS` or `mkfs.btrfs -L TomOS`. UUIDs are chosen up front so `/etc/fstab` can be written without probing. |
| 4 | Mounting | Target at `/mnt/tomos-target`. Btrfs gets subvolumes `@`, `@home`, `@swap`, mounted with `compress=zstd:1,noatime`. |
| 5 | Copying Tom OS | `unsquashfs -f -percentage` from the live medium (or `rsync -aHAXx` from a directory). This is ~60% of the progress bar. |
| 6 | Language, time, keyboard | `/etc/fstab`, swap file (`fallocate`+`mkswap`, or `btrfs filesystem mkswapfile`), hostname and hosts, a fresh machine ID, `locale-gen`, `/etc/default/locale`, `/etc/localtime`, `/etc/default/keyboard`, `kb_layout`/`kb_variant` in the Hyprland skeleton config, and `XKB_DEFAULT_LAYOUT` for the TDM login screen. |
| 7 | Creating the user | `useradd -m` with the groups `adm cdrom dip lpadmin plugdev sudo audio video netdev users` that exist, password through `chpasswd` on stdin (shown as `(hidden)` in logs), root locked, TDM remembers the user. Optional greetd `[initial_session]` for automatic sign-in. |
| 8 | Removing the live tools | Binds `/dev`, `/proc`, `/sys`, `/run` (and `efivarfs` on UEFI), purges `live-boot`, `live-config`, `live-tools` and Calamares if installed, removes the live-only launchers and live-build's `policy-rc.d`, enables `greetd` and `NetworkManager`. |
| 9 | Boot loader | `update-initramfs -u -k all`, GRUB defaults (`GRUB_DISTRIBUTOR="Tom OS"`; on dual-boot installs a 5 s menu and os-prober), `grub-install --target=i386-pc` **and** `--target=x86_64-efi --removable` so the disk boots on BIOS and UEFI, plus an NVRAM entry "TomOS" when booted in UEFI mode, then `update-grub`. |
| 10 | Finishing up | Copies the log to `/var/log/tomos-installer.log` in the new system, `sync`, `umount -R`. |

If any required action fails, the backend unmounts everything, reports the error and the window shows the failure page. The full log is always in `/var/log/tomos-installer.log` on the live system.

## Progress protocol

One JSON object per line on stdout:

```json
{"type":"step","index":4,"total":10,"title":"Copying Tom OS"}
{"type":"progress","value":0.42}
{"type":"log","text":"$ mkfs.ext4 -F -q -L TomOS -U … /dev/nvme0n1p3"}
{"type":"done"}
{"type":"error","text":"/dev/sda1 is in use (mounted at /home). Unmount it and try again."}
```

That makes the backend easy to drive from scripts or a different front end.

## Safety

- The window only *describes* what will happen; the backend re-validates the plan (device names, username, hostname, locale, time zone, keyboard names) before running anything.
- The live USB is detected (`/run/live/medium`) and refused, both in the disk list and in the backend.
- Loop devices are refused unless `--allow-loop` is given (for testing).
- Nothing is written before the confirmation dialog on the Summary page.
- Commands are run directly (no shell), so names can't inject commands.

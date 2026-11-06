//! `tomos-installer` — the Tom OS installer.
//!
//!   tomos-installer                     open the installer
//!   tomos-installer --demo [--page N]   try it with sample disks; nothing is changed
//!   tomos-installer backend [--dry-run] [--allow-loop] < plan.json   (as root)
//!   tomos-installer disks [--allow-loop]                 list disks as JSON
//!   tomos-installer check-plan < plan.json               validate a plan

use std::io::Read;
use tomos_installer::{backend, disks, plan::InstallPlan, VERSION};

const HELP: &str = "Tom OS Installer

Usage:
  tomos-installer [--source PATH] [--allow-loop]   Open the installer window
  tomos-installer --demo [--page N]                Try the installer: sample disks, simulated install
  tomos-installer backend [--dry-run] [--allow-loop]
                                                   Install from a JSON plan on stdin (root only).
                                                   Progress is printed as JSON lines.
  tomos-installer disks [--allow-loop]             List disks as JSON
  tomos-installer check-plan                       Validate a JSON plan on stdin
  tomos-installer --version | --help
";

fn value_of(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |f: &str| args.iter().any(|a| a == f);
    if has("--help") || has("-h") {
        print!("{HELP}");
        return;
    }
    if has("--version") || has("-V") {
        println!("tomos-installer {VERSION}");
        return;
    }
    let allow_loop = has("--allow-loop");
    match args.first().map(String::as_str) {
        Some("backend") => std::process::exit(backend::main(has("--dry-run"), allow_loop)),
        Some("disks") => match disks::list(allow_loop) {
            Ok(d) => println!("{}", serde_json::to_string_pretty(&d).unwrap_or_default()),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        },
        Some("check-plan") => {
            let mut s = String::new();
            let _ = std::io::stdin().read_to_string(&mut s);
            match serde_json::from_str::<InstallPlan>(&s).map_err(|e| e.to_string()).and_then(|p| p.validate().map(|_| p)) {
                Ok(p) => println!("OK: {}", serde_json::to_string(&p.redacted()).unwrap_or_default()),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(2);
                }
            }
        }
        Some(other) if !other.starts_with('-') => {
            eprintln!("Unknown command: {other}\n\n{HELP}");
            std::process::exit(2);
        }
        _ => run_gui(&args, allow_loop),
    }
}

#[cfg(feature = "gui")]
fn run_gui(args: &[String], allow_loop: bool) {
    let opts = tomos_installer::gui::Options {
        demo: args.iter().any(|a| a == "--demo"),
        page: value_of(args, "--page").and_then(|p| p.parse().ok()),
        source: value_of(args, "--source"),
        allow_loop,
        demo_fail: args.iter().any(|a| a == "--demo-fail"),
    };
    std::process::exit(tomos_installer::gui::run(opts));
}

#[cfg(not(feature = "gui"))]
fn run_gui(_args: &[String], _allow_loop: bool) {
    let _ = value_of;
    eprintln!("This build has no window (built without the \"gui\" feature). Use `tomos-installer backend`.");
    std::process::exit(2);
}

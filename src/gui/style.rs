//! The Tom OS neon look, shared with the TomOS Display Manager (TDM).

pub const BG: &str = "#0a0a12";
pub const CARD: &str = "#14141f";
pub const FG: &str = "#e8e8f0";
pub const MUTED: &str = "#8a8aa0";
pub const PINK: &str = "#ff2d96";
pub const CYAN: &str = "#00f0ff";
pub const PURPLE: &str = "#c74cff";
pub const WARN: &str = "#ffe14d";
pub const ERROR: &str = "#ff2d78";
pub const OK: &str = "#3dffa8";

/// RGB 0–1 for cairo.
pub fn rgb(hex: &str) -> (f64, f64, f64) {
    let h = hex.trim_start_matches('#');
    let v = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0) as f64 / 255.0;
    (v(0), v(2), v(4))
}

const CSS: &str = r#"
window.ti { background-color: @BG@; color: @FG@;
  font-family: "JetBrainsMono Nerd Font", "JetBrains Mono", "DejaVu Sans Mono", monospace; font-size: 13px; }
.ti-scrim { background-image: linear-gradient(135deg, alpha(@BG@,0.88), alpha(@BG@,0.55) 45%, alpha(@BG@,0.88)); }

/* Sidebar */
.ti-side { background-color: alpha(@BG@,0.72); border-right: 1px solid alpha(@PINK@,0.35); padding: 26px 18px 18px 22px; min-width: 230px; }
.ti-brand { color: @PINK@; font-weight: 900; letter-spacing: 5px; font-size: 17px; text-shadow: 0 0 12px alpha(@PINK@,0.85); }
.ti-brand-sub { color: @CYAN@; font-size: 11px; letter-spacing: 4px; text-shadow: 0 0 10px alpha(@CYAN@,0.7); }
.ti-step { padding: 9px 10px; border-radius: 10px; }
.ti-step .num { min-width: 26px; min-height: 26px; border-radius: 999px; border: 1px solid alpha(@MUTED@,0.6);
  color: @MUTED@; font-weight: 800; font-size: 12px; }
.ti-step .name { color: @MUTED@; font-weight: 700; letter-spacing: 1px; }
.ti-step.active { background-color: alpha(@PINK@,0.14); box-shadow: inset 3px 0 0 @PINK@; }
.ti-step.active .num { background-color: @PINK@; border-color: @PINK@; color: @BG@; box-shadow: 0 0 12px alpha(@PINK@,0.8); }
.ti-step.active .name { color: @FG@; }
.ti-step.done .num { border-color: @CYAN@; color: @CYAN@; }
.ti-step.done .name { color: alpha(@FG@,0.85); }
.ti-version { color: alpha(@MUTED@,0.8); font-size: 11px; letter-spacing: 2px; }

/* Main card */
.ti-card { background-color: alpha(@CARD@,0.86); border: 2px solid alpha(@PINK@,0.75); border-radius: 22px;
  box-shadow: 0 0 26px alpha(@PINK@,0.40), 0 0 70px alpha(@CYAN@,0.15), inset 0 0 0 1px alpha(@CYAN@,0.22); }
.ti-page { padding: 30px 38px 12px 38px; }
.ti-title { color: @PINK@; font-size: 28px; font-weight: 900; letter-spacing: 4px; text-shadow: 0 0 14px alpha(@PINK@,0.8); }
.ti-subtitle { color: @CYAN@; font-size: 13px; letter-spacing: 1px; text-shadow: 0 0 8px alpha(@CYAN@,0.55); }
.ti-label { color: @PURPLE@; font-size: 11px; font-weight: 900; letter-spacing: 3px; }
.ti-text { color: @FG@; }
.ti-muted { color: @MUTED@; font-size: 12px; }
.ti-big { font-size: 15px; font-weight: 800; color: @FG@; }
.ti-error { color: @ERROR@; font-weight: 800; }
.ti-warn { color: @WARN@; font-weight: 800; }
.ti-ok { color: @OK@; font-weight: 800; }
.ti-cyan { color: @CYAN@; font-weight: 800; }
.ti-clock { color: @CYAN@; font-size: 30px; font-weight: 900; text-shadow: 0 0 12px alpha(@CYAN@,0.8); }

.ti-nav { padding: 14px 30px 22px 30px; border-top: 1px solid alpha(@PURPLE@,0.25); }

entry, passwordentry, spinbutton, dropdown > button, searchentry {
  background-image: none; background-color: alpha(@BG@,0.92); color: @FG@;
  border: 1px solid alpha(@PINK@,0.45); border-radius: 10px; min-height: 34px; box-shadow: none; caret-color: @CYAN@; }
entry, passwordentry, searchentry { padding: 0 12px; }
entry:focus-within, passwordentry:focus-within, spinbutton:focus-within, searchentry:focus-within,
dropdown > button:hover, dropdown > button:focus { border-color: @CYAN@; box-shadow: 0 0 10px alpha(@CYAN@,0.55); }
entry > text > placeholder, passwordentry > text > placeholder { color: alpha(@MUTED@,0.85); }
spinbutton > button { background: none; border: none; color: @CYAN@; box-shadow: none; }
popover > contents, popover > arrow { background-color: @CARD@; border: 1px solid @PINK@; color: @FG@; }
popover listview > row:selected, popover row:hover { background-color: alpha(@PINK@,0.30); }

list.ti-list { background-color: transparent; }
list.ti-list > row { background-color: alpha(@BG@,0.55); border: 1px solid alpha(@PURPLE@,0.35); border-radius: 12px;
  margin: 0 0 8px 0; padding: 10px 14px; }
list.ti-list > row:hover { border-color: alpha(@CYAN@,0.7); }
list.ti-list > row:selected { background-color: alpha(@PINK@,0.16); border-color: @PINK@; box-shadow: 0 0 12px alpha(@PINK@,0.45); }
list.ti-list > row:disabled { opacity: 0.55; }
list.ti-compact > row { padding: 6px 12px; margin-bottom: 4px; border-radius: 8px; }

checkbutton { color: @FG@; }
checkbutton check, checkbutton radio { background-image: none; background-color: alpha(@BG@,0.9); border: 1px solid alpha(@CYAN@,0.7); }
checkbutton check:checked, checkbutton radio:checked { background-color: @PINK@; border-color: @PINK@; color: @BG@; }
.ti-option { background-color: alpha(@BG@,0.55); border: 1px solid alpha(@PURPLE@,0.35); border-radius: 12px; padding: 10px 14px; }
.ti-option.selected { border-color: @PINK@; background-color: alpha(@PINK@,0.12); box-shadow: 0 0 12px alpha(@PINK@,0.4); }
switch { background-color: alpha(@MUTED@,0.35); border: 1px solid alpha(@MUTED@,0.6); }
switch:checked { background-color: @PINK@; border-color: @PINK@; }
switch > slider { background-color: @FG@; }

button.ti-primary { background-image: linear-gradient(90deg, @PINK@, @PURPLE@ 55%, @CYAN@); color: @BG@;
  font-weight: 900; letter-spacing: 3px; border: none; border-radius: 10px; padding: 9px 26px; min-height: 22px;
  box-shadow: 0 0 16px alpha(@PINK@,0.6); text-shadow: none; }
button.ti-primary:hover { box-shadow: 0 0 22px alpha(@PINK@,0.9), 0 0 40px alpha(@CYAN@,0.4); }
button.ti-primary:disabled { background-image: none; background-color: alpha(@MUTED@,0.35); color: @FG@; box-shadow: none; }
button.ti-danger { background-image: linear-gradient(90deg, @ERROR@, @PINK@); color: @BG@; font-weight: 900;
  letter-spacing: 3px; border: none; border-radius: 10px; padding: 9px 26px; box-shadow: 0 0 18px alpha(@ERROR@,0.75); }
button.ti-ghost { background-image: none; background-color: transparent; color: @FG@; border: 1px solid alpha(@MUTED@,0.6);
  border-radius: 10px; padding: 9px 20px; box-shadow: none; font-weight: 700; letter-spacing: 2px; }
button.ti-ghost:hover { color: @CYAN@; border-color: @CYAN@; box-shadow: 0 0 10px alpha(@CYAN@,0.4); }
button.ti-ghost:disabled { opacity: 0.35; }

.ti-check { background-color: alpha(@BG@,0.55); border: 1px solid alpha(@PURPLE@,0.30); border-radius: 12px; padding: 10px 14px; }
.ti-check .icon { font-size: 18px; min-width: 28px; }
.ti-warnbox { background-color: alpha(@ERROR@,0.10); border: 1px solid alpha(@ERROR@,0.8); border-radius: 12px; padding: 12px 16px;
  box-shadow: 0 0 14px alpha(@ERROR@,0.35); }
.ti-infobox { background-color: alpha(@CYAN@,0.07); border: 1px solid alpha(@CYAN@,0.55); border-radius: 12px; padding: 12px 16px; }
.ti-summary { background-color: alpha(@BG@,0.55); border: 1px solid alpha(@PURPLE@,0.35); border-radius: 12px; padding: 14px 18px; }
.ti-summary .key { color: @PURPLE@; font-size: 11px; font-weight: 900; letter-spacing: 2px; }
.ti-summary .value { color: @FG@; font-weight: 700; }

progressbar > trough { background-color: alpha(@BG@,0.9); border: 1px solid alpha(@PURPLE@,0.5); border-radius: 999px; min-height: 14px; }
progressbar > trough > progress { background-image: linear-gradient(90deg, @PINK@, @PURPLE@, @CYAN@); border-radius: 999px;
  min-height: 14px; box-shadow: 0 0 14px alpha(@PINK@,0.8); }
levelbar > trough { background: none; border: none; }
levelbar block { min-height: 6px; border-radius: 999px; }
levelbar block.empty { background-color: alpha(@MUTED@,0.3); }
levelbar block.low { background-color: @ERROR@; }
levelbar block.high { background-color: @WARN@; }
levelbar block.full { background-color: @OK@; }
textview.ti-log, textview.ti-log > text { background-color: alpha(@BG@,0.95); color: alpha(@FG@,0.85); font-size: 11px; }
.ti-logframe { border: 1px solid alpha(@PURPLE@,0.4); border-radius: 10px; }
expander title > label { color: @PURPLE@; font-weight: 800; letter-spacing: 2px; }
.ti-tip { color: alpha(@FG@,0.9); font-size: 13px; }
.ti-percent { color: @PINK@; font-size: 44px; font-weight: 900; text-shadow: 0 0 18px alpha(@PINK@,0.85); }
.ti-test { font-size: 15px; }
.ti-done-icon { font-size: 46px; text-shadow: 0 0 16px currentColor; }
scrollbar slider { background-color: alpha(@PINK@,0.55); }
*:focus-visible { outline: 2px solid @CYAN@; outline-offset: 2px; }
"#;

pub fn css() -> String {
    CSS.replace("@BG@", BG)
        .replace("@CARD@", CARD)
        .replace("@FG@", FG)
        .replace("@MUTED@", MUTED)
        .replace("@PINK@", PINK)
        .replace("@CYAN@", CYAN)
        .replace("@PURPLE@", PURPLE)
        .replace("@WARN@", WARN)
        .replace("@ERROR@", ERROR)
        .replace("@OK@", OK)
}

#[cfg(test)]
mod tests {
    #[test]
    fn no_placeholders_left() {
        assert!(!super::css().contains('@'));
        assert_eq!(super::rgb("#ff2d96"), (1.0, 45.0 / 255.0, 150.0 / 255.0));
    }
}

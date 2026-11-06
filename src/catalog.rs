//! Languages, keyboard layouts and time zones, read from the live system's data files with a
//! small built-in fallback.

use std::fs;

/// Reads a data file; `TOMOS_DATA_ROOT` (e.g. a Tom OS rootfs) is tried first, for demos.
fn read_data(path: &str) -> std::io::Result<String> {
    if let Some(root) = std::env::var_os("TOMOS_DATA_ROOT") {
        let p = std::path::Path::new(&root).join(path.trim_start_matches('/'));
        if let Ok(t) = fs::read_to_string(p) {
            return Ok(t);
        }
    }
    fs::read_to_string(path)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub id: String,
    pub name: String,
    pub variants: Vec<Choice>,
}

const LANGS: &[(&str, &str)] = &[
    ("af", "Afrikaans"), ("ar", "Arabic"), ("ast", "Asturian"), ("be", "Belarusian"), ("bg", "Bulgarian"),
    ("bn", "Bengali"), ("bs", "Bosnian"), ("ca", "Catalan"), ("cs", "Czech"), ("cy", "Welsh"), ("da", "Danish"),
    ("de", "German"), ("el", "Greek"), ("en", "English"), ("eo", "Esperanto"), ("es", "Spanish"), ("et", "Estonian"),
    ("eu", "Basque"), ("fa", "Persian"), ("fi", "Finnish"), ("fil", "Filipino"), ("fr", "French"), ("ga", "Irish"),
    ("gd", "Scottish Gaelic"), ("gl", "Galician"), ("gu", "Gujarati"), ("he", "Hebrew"), ("hi", "Hindi"),
    ("hr", "Croatian"), ("hu", "Hungarian"), ("hy", "Armenian"), ("id", "Indonesian"), ("is", "Icelandic"),
    ("it", "Italian"), ("ja", "Japanese"), ("ka", "Georgian"), ("kk", "Kazakh"), ("km", "Khmer"), ("kn", "Kannada"),
    ("ko", "Korean"), ("ku", "Kurdish"), ("lt", "Lithuanian"), ("lv", "Latvian"), ("mk", "Macedonian"),
    ("ml", "Malayalam"), ("mr", "Marathi"), ("ms", "Malay"), ("mt", "Maltese"), ("nb", "Norwegian Bokmål"),
    ("ne", "Nepali"), ("nl", "Dutch"), ("nn", "Norwegian Nynorsk"), ("pa", "Punjabi"), ("pl", "Polish"),
    ("pt", "Portuguese"), ("ro", "Romanian"), ("ru", "Russian"), ("si", "Sinhala"), ("sk", "Slovak"),
    ("sl", "Slovenian"), ("sq", "Albanian"), ("sr", "Serbian"), ("sv", "Swedish"), ("sw", "Swahili"),
    ("ta", "Tamil"), ("te", "Telugu"), ("th", "Thai"), ("tr", "Turkish"), ("uk", "Ukrainian"), ("ur", "Urdu"),
    ("uz", "Uzbek"), ("vi", "Vietnamese"), ("zh", "Chinese"),
];

const COUNTRIES: &[(&str, &str)] = &[
    ("AE", "UAE"), ("AR", "Argentina"), ("AT", "Austria"), ("AU", "Australia"), ("BE", "Belgium"), ("BG", "Bulgaria"),
    ("BR", "Brazil"), ("BY", "Belarus"), ("CA", "Canada"), ("CH", "Switzerland"), ("CL", "Chile"), ("CN", "China"),
    ("CO", "Colombia"), ("CY", "Cyprus"), ("CZ", "Czechia"), ("DE", "Germany"), ("DK", "Denmark"), ("EE", "Estonia"),
    ("EG", "Egypt"), ("ES", "Spain"), ("FI", "Finland"), ("FR", "France"), ("GB", "United Kingdom"), ("GR", "Greece"),
    ("HK", "Hong Kong"), ("HR", "Croatia"), ("HU", "Hungary"), ("ID", "Indonesia"), ("IE", "Ireland"), ("IL", "Israel"),
    ("IN", "India"), ("IR", "Iran"), ("IS", "Iceland"), ("IT", "Italy"), ("JP", "Japan"), ("KR", "South Korea"),
    ("LT", "Lithuania"), ("LU", "Luxembourg"), ("LV", "Latvia"), ("MT", "Malta"), ("MX", "Mexico"), ("MY", "Malaysia"),
    ("NG", "Nigeria"), ("NL", "Netherlands"), ("NO", "Norway"), ("NZ", "New Zealand"), ("PE", "Peru"),
    ("PH", "Philippines"), ("PK", "Pakistan"), ("PL", "Poland"), ("PT", "Portugal"), ("RO", "Romania"),
    ("RS", "Serbia"), ("RU", "Russia"), ("SA", "Saudi Arabia"), ("SE", "Sweden"), ("SG", "Singapore"),
    ("SI", "Slovenia"), ("SK", "Slovakia"), ("TH", "Thailand"), ("TR", "Türkiye"), ("TW", "Taiwan"),
    ("UA", "Ukraine"), ("US", "United States"), ("VE", "Venezuela"), ("VN", "Vietnam"), ("ZA", "South Africa"),
];

fn lookup(table: &[(&str, &'static str)], key: &str) -> Option<&'static str> {
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// "en_GB.UTF-8" → "English (United Kingdom)".
pub fn locale_name(locale: &str) -> String {
    let base = locale.split(['.', '@']).next().unwrap_or(locale);
    let (lang, country) = match base.split_once('_') {
        Some((l, c)) => (l, Some(c)),
        None => (base, None),
    };
    let lname = lookup(LANGS, lang).map(str::to_owned).unwrap_or_else(|| lang.to_owned());
    match country {
        Some(c) => format!("{lname} ({})", lookup(COUNTRIES, c).unwrap_or(c)),
        None => lname,
    }
}

/// UTF-8 locales from glibc's SUPPORTED list ("en_GB.UTF-8 UTF-8" lines), sorted by name.
pub fn parse_supported(text: &str) -> Vec<Choice> {
    let mut out: Vec<Choice> = text
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let id = it.next()?;
            let charset = it.next()?;
            (charset == "UTF-8" && id.ends_with(".UTF-8") && !id.contains('@'))
                .then(|| Choice { id: id.to_owned(), name: format!("{} — {id}", locale_name(id)) })
        })
        .collect();
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out.dedup_by(|a, b| a.id == b.id);
    out
}

pub fn locales() -> Vec<Choice> {
    let list = read_data("/usr/share/i18n/SUPPORTED").map(|t| parse_supported(&t)).unwrap_or_default();
    if !list.is_empty() {
        return list;
    }
    parse_supported(
        "en_GB.UTF-8 UTF-8\nen_US.UTF-8 UTF-8\nde_DE.UTF-8 UTF-8\nfr_FR.UTF-8 UTF-8\nes_ES.UTF-8 UTF-8\n\
         it_IT.UTF-8 UTF-8\nnl_NL.UTF-8 UTF-8\npt_BR.UTF-8 UTF-8\npl_PL.UTF-8 UTF-8\nja_JP.UTF-8 UTF-8\n",
    )
}

/// Layouts and variants from XKB's `evdev.lst`.
pub fn parse_evdev_lst(text: &str) -> Vec<Layout> {
    let mut layouts: Vec<Layout> = Vec::new();
    let mut section = "";
    for line in text.lines() {
        if let Some(name) = line.strip_prefix("! ") {
            section = match name.trim() {
                "layout" => "layout",
                "variant" => "variant",
                _ => "",
            };
            continue;
        }
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let Some((id, rest)) = t.split_once(char::is_whitespace) else { continue };
        let rest = rest.trim();
        match section {
            "layout" => layouts.push(Layout { id: id.into(), name: rest.into(), variants: vec![] }),
            "variant" => {
                // "extd            gb: English (UK, extended, Windows)"
                if let Some((layout, name)) = rest.split_once(": ") {
                    if let Some(l) = layouts.iter_mut().find(|l| l.id == layout) {
                        l.variants.push(Choice { id: id.into(), name: name.into() });
                    }
                }
            }
            _ => {}
        }
    }
    layouts.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    layouts
}

pub fn keyboards() -> Vec<Layout> {
    let list = read_data("/usr/share/X11/xkb/rules/evdev.lst").map(|t| parse_evdev_lst(&t)).unwrap_or_default();
    if !list.is_empty() {
        return list;
    }
    parse_evdev_lst(
        "! layout\n  gb   English (UK)\n  us   English (US)\n  de   German\n  fr   French\n  es   Spanish\n\
         ! variant\n  extd  gb: English (UK, extended, Windows)\n  intl  us: English (US, intl., with dead keys)\n",
    )
}

/// Time zones from `zone1970.tab` (third column), plus UTC.
pub fn parse_zone_tab(text: &str) -> Vec<String> {
    let mut out: Vec<String> = text
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split('\t').nth(2).map(str::to_owned))
        .filter(|z| z.contains('/'))
        .collect();
    out.push("Etc/UTC".into());
    out.sort();
    out.dedup();
    out
}

pub fn timezones() -> Vec<String> {
    let list = read_data("/usr/share/zoneinfo/zone1970.tab").map(|t| parse_zone_tab(&t)).unwrap_or_default();
    if list.len() > 1 {
        return list;
    }
    ["Europe/London", "Europe/Paris", "Europe/Berlin", "America/New_York", "America/Los_Angeles", "Asia/Tokyo", "Australia/Sydney", "Etc/UTC"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// "America/Argentina/Buenos_Aires" → ("America", "Argentina/Buenos Aires").
pub fn split_zone(zone: &str) -> (String, String) {
    match zone.split_once('/') {
        Some((r, c)) => (r.to_owned(), c.replace('_', " ")),
        None => (zone.to_owned(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_names() {
        assert_eq!(locale_name("en_GB.UTF-8"), "English (United Kingdom)");
        assert_eq!(locale_name("pt_BR.UTF-8"), "Portuguese (Brazil)");
        assert_eq!(locale_name("xx_YY.UTF-8"), "xx (YY)");
        assert_eq!(locale_name("eo.UTF-8"), "Esperanto");
    }

    #[test]
    fn supported_keeps_utf8_only() {
        let l = parse_supported("en_GB.UTF-8 UTF-8\nen_GB ISO-8859-1\nde_DE@euro ISO-8859-15\nsr_RS@latin UTF-8\nde_DE.UTF-8 UTF-8\n");
        let ids: Vec<&str> = l.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["en_GB.UTF-8", "de_DE.UTF-8"]);
        assert_eq!(l[0].name, "English (United Kingdom) — en_GB.UTF-8");
    }

    #[test]
    fn evdev_layouts_and_variants() {
        let text = "! model\n  pc105   Generic 105-key PC\n\n! layout\n  us   English (US)\n  gb   English (UK)\n\n! variant\n  extd   gb: English (UK, extended, Windows)\n  dvorak   gb: English (UK, Dvorak)\n  intl   us: English (US, intl., with dead keys)\n\n! option\n  grp   Switching\n";
        let l = parse_evdev_lst(text);
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].id, "gb");
        assert_eq!(l[0].variants.len(), 2);
        assert_eq!(l[0].variants[0], Choice { id: "extd".into(), name: "English (UK, extended, Windows)".into() });
        assert_eq!(l[1].variants[0].id, "intl");
    }

    #[test]
    fn zones() {
        let z = parse_zone_tab("# comment\nGB,GG,IM,JE\t+513030-0000731\tEurope/London\nUS\t+404251-0740023\tAmerica/New_York\tEastern\n");
        assert_eq!(z, ["America/New_York", "Etc/UTC", "Europe/London"]);
        assert_eq!(split_zone("America/Argentina/Buenos_Aires"), ("America".into(), "Argentina/Buenos Aires".into()));
    }
}

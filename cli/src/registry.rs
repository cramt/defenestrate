//! Registry config as .reg files.
//!
//! The text is pure ASCII so the exact same bytes can be merged by
//! hivexregedit (the build-time check) and, re-encoded as UTF-16LE, imported
//! by reg.exe at install time. Anything that isn't printable ASCII goes out
//! as hex(N), which also sidesteps both tools' differing ideas of string
//! escaping.

use std::collections::BTreeMap;

use crate::config::{Hive, RegValue, is_plain};

/// Which hive a .reg file's keys are relative to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Root {
    Software,
    System,
    /// The Default profile's NTUSER.DAT, which specialize.cmd loads under
    /// this name.
    DefaultUser,
}

impl Root {
    pub fn path(self) -> &'static str {
        match self {
            Self::Software => "HKEY_LOCAL_MACHINE\\SOFTWARE",
            Self::System => "HKEY_LOCAL_MACHINE\\SYSTEM",
            Self::DefaultUser => "HKEY_USERS\\DefenestrateDefault",
        }
    }

    /// Where the hive file sits in the image.
    pub fn hive_file(self) -> &'static str {
        match self {
            Self::Software => "/Windows/System32/config/SOFTWARE",
            Self::System => "/Windows/System32/config/SYSTEM",
            Self::DefaultUser => "/Users/Default/NTUSER.DAT",
        }
    }
}

/// The ASCII text of a .reg file. Keys and names are checked to be plain
/// ASCII when the config loads.
pub fn render(root: Root, keys: &Hive) -> String {
    // reg.exe creates missing parents, hivexregedit refuses to. Listing every
    // ancestor as an empty section satisfies both.
    let empty = BTreeMap::new();
    let mut sections: BTreeMap<&str, &BTreeMap<String, RegValue>> = BTreeMap::new();
    for key in keys.keys() {
        for (i, _) in key.match_indices('\\') {
            sections.entry(&key[..i]).or_insert(&empty);
        }
    }
    for (key, values) in keys {
        sections.insert(key, values);
    }
    let mut order: Vec<&str> = sections.keys().copied().collect();
    order.sort_by_cached_key(|k| k.to_ascii_lowercase().split('\\').map(str::to_owned).collect::<Vec<_>>());

    let mut out = String::from("Windows Registry Editor Version 5.00\r\n\r\n");
    for key in order {
        out += &format!("[{}\\{key}]\r\n", root.path());
        for (name, value) in sections[key] {
            out += &format!("{}={}\r\n", render_name(name), render_value(value));
        }
        out += "\r\n";
    }
    out
}

/// What reg.exe wants: UTF-16LE with a BOM.
pub fn utf16(text: &str) -> Vec<u8> {
    let mut out = vec![0xff, 0xfe];
    out.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    out
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn render_name(name: &str) -> String {
    if name.is_empty() { "@".into() } else { quote(name) }
}

fn utf16z(s: &str) -> Vec<u8> {
    s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(",")
}

fn render_value(value: &RegValue) -> String {
    match value {
        RegValue::Delete => "-".into(),
        RegValue::Sz(s) if is_plain(s) => quote(s),
        RegValue::Sz(s) => format!("hex(1):{}", hex(&utf16z(s))),
        RegValue::ExpandSz(s) => format!("hex(2):{}", hex(&utf16z(s))),
        RegValue::MultiSz(v) => {
            let mut bytes: Vec<u8> = v.iter().flat_map(|s| utf16z(s)).collect();
            bytes.extend([0, 0]);
            format!("hex(7):{}", hex(&bytes))
        }
        RegValue::Dword(d) => format!("dword:{d:08x}"),
        RegValue::Qword(q) => format!("hex(b):{}", hex(&q.to_le_bytes())),
        RegValue::Binary(b) => format!("hex:{}", hex(b)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_encodings() {
        assert_eq!(render_value(&RegValue::Dword(1)), "dword:00000001");
        assert_eq!(render_value(&RegValue::Sz("C:\\a \"b\"".into())), r#""C:\\a \"b\"""#);
        assert_eq!(render_value(&RegValue::Sz("é".into())), "hex(1):e9,00,00,00");
        assert_eq!(render_value(&RegValue::MultiSz(vec!["a".into(), "b".into()])), "hex(7):61,00,00,00,62,00,00,00,00,00");
        assert_eq!(render_value(&RegValue::Qword(1)), "hex(b):01,00,00,00,00,00,00,00");
        assert_eq!(render_value(&RegValue::Delete), "-");
    }

    #[test]
    fn parents_get_sections_sorted_case_insensitively_by_component() {
        let mut keys = Hive::new();
        keys.insert("b\\x".into(), BTreeMap::from([("v".into(), RegValue::Dword(0))]));
        keys.insert("A b".into(), BTreeMap::new());
        keys.insert("a".into(), BTreeMap::new());
        let text = render(Root::Software, &keys);
        let sections: Vec<&str> = text.lines().filter(|l| l.starts_with('[')).collect();
        assert_eq!(
            sections,
            [
                "[HKEY_LOCAL_MACHINE\\SOFTWARE\\a]",
                "[HKEY_LOCAL_MACHINE\\SOFTWARE\\A b]",
                "[HKEY_LOCAL_MACHINE\\SOFTWARE\\b]",
                "[HKEY_LOCAL_MACHINE\\SOFTWARE\\b\\x]"
            ]
        );
    }
}

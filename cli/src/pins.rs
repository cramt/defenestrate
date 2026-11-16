//! `dfnstrate pins`: moves every download that says what it tracks
//! (`track`) to the newest version, in the config files themselves, keeping
//! their comments and layout. A tracked source with no pin yet gets one,
//! which is what a Nix build needs: it can't resolve "newest" in its sandbox.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use toml_edit::{DocumentMut, Item, TableLike, Value};

use crate::cache::Cache;
use crate::config::Track;
use crate::track;

#[derive(clap::Args)]
pub struct Args {
    /// TOML config files, each handled on its own: a pin is rewritten in the
    /// file it's written in. The built-in defaults are configs/default.toml
    /// in the repo.
    #[arg(required = true, value_name = "FILE")]
    files: Vec<PathBuf>,
    /// Only say which pins are behind; change nothing.
    #[arg(long)]
    check: bool,
}

pub fn run(args: &Args, cache: &Cache) -> Result<()> {
    // Several pins often track one thing (the four Iosevka fonts are one
    // release asset), and each lookup is a request or two.
    let mut newest: HashMap<Track, String> = HashMap::new();
    let mut behind = 0;
    for file in &args.files {
        let text = std::fs::read_to_string(file).with_context(|| file.display().to_string())?;
        let mut doc: DocumentMut = text.parse().with_context(|| format!("{} isn't TOML", file.display()))?;
        let mut pins = Vec::new();
        find(doc.as_item_mut(), String::new(), &mut |path, table| {
            let track = parse_track(table.get("track").expect("found by its track key")).with_context(|| format!("{}: {path}.track", file.display()))?;
            let url = match newest.get(&track) {
                Some(u) => u.clone(),
                None => {
                    let u = track::resolve(&track)?;
                    newest.insert(track.clone(), u.clone());
                    u
                }
            };
            let current = table.get("url").and_then(Item::as_str);
            if current == Some(url.as_str()) && table.contains_key("hash") {
                println!("{}: {path} is up to date", file.display());
                return Ok(());
            }
            println!("{}: {path}: {} -> {url}", file.display(), current.unwrap_or("(not pinned)"));
            behind += 1;
            if !args.check {
                let (_, hash) = cache.get_unpinned(&url)?;
                table.insert("url", toml_edit::value(url.as_str()));
                table.insert("hash", toml_edit::value(hash.to_string()));
                // New keys in an inline table otherwise land after its last
                // value's trailing space (`… } , url = …`).
                table.fmt();
            }
            pins.push(path.to_owned());
            Ok(())
        })?;
        if !args.check && !pins.is_empty() {
            std::fs::write(file, doc.to_string()).with_context(|| file.display().to_string())?;
        }
    }
    match (behind, args.check) {
        (0, _) => println!("every tracked pin is up to date"),
        (n, true) => bail!("{n} pin(s) behind; `dfnstrate pins` without --check moves them"),
        (n, false) => println!("moved {n} pin(s)"),
    }
    Ok(())
}

/// Calls `f` on every table, inline or not, that has a `track` key, with a
/// readable path to it (`drivers[0]`, `wsl.package`).
fn find(item: &mut Item, path: String, f: &mut impl FnMut(&str, &mut dyn TableLike) -> Result<()>) -> Result<()> {
    match item {
        Item::Table(t) => table(t, &path, f),
        Item::ArrayOfTables(a) => {
            for (i, t) in a.iter_mut().enumerate() {
                table(t, &format!("{path}[{i}]"), f)?;
            }
            Ok(())
        }
        Item::Value(v) => value(v, &path, f),
        Item::None => Ok(()),
    }
}

fn value(v: &mut Value, path: &str, f: &mut impl FnMut(&str, &mut dyn TableLike) -> Result<()>) -> Result<()> {
    match v {
        Value::InlineTable(t) => table(t, path, f),
        Value::Array(a) => {
            for (i, v) in a.iter_mut().enumerate() {
                value(v, &format!("{path}[{i}]"), f)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn table(t: &mut dyn TableLike, path: &str, f: &mut impl FnMut(&str, &mut dyn TableLike) -> Result<()>) -> Result<()> {
    if t.contains_key("track") {
        return f(path, t);
    }
    let keys: Vec<String> = t.iter().map(|(k, _)| k.to_owned()).collect();
    for k in keys {
        let child = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
        find(t.get_mut(&k).expect("listed above"), child, f)?;
    }
    Ok(())
}

/// Through the config's own type, so `pins` takes exactly what a build
/// does.
fn parse_track(item: &Item) -> Result<Track> {
    #[derive(serde::Deserialize)]
    struct Wrap {
        track: Track,
    }
    let text = if item.is_table() { format!("[track]\n{item}") } else { format!("track = {item}") };
    Ok(toml::from_str::<Wrap>(&text)?.track)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_tracks_wherever_they_are() {
        let mut doc: DocumentMut = r#"
fonts = [
  { url = "a", hash = "h", member = "m", track = { github = "o/r", asset = "x.tar.xz" } },
  { path = "local.ttf" },
]
[wsl]
package = { url = "b", hash = "h", track = { github = "microsoft/WSL", asset = "wsl.*.x64.msi" } }
[[drivers]]  # kept
track = { catalog = "VEN_1002&DEV_747E" }
[[drivers]]
url = "c"
hash = "h"
[drivers.track]
catalog = "VEN_10DE"
"#
        .parse()
        .unwrap();
        let mut seen = Vec::new();
        find(doc.as_item_mut(), String::new(), &mut |path, t| {
            seen.push((path.to_owned(), parse_track(t.get("track").unwrap()).unwrap().to_string()));
            t.insert("url", toml_edit::value("new"));
            Ok(())
        })
        .unwrap();
        assert_eq!(
            seen,
            [
                ("fonts[0]".to_owned(), "github o/r x.tar.xz".to_owned()),
                ("wsl.package".to_owned(), "github microsoft/WSL wsl.*.x64.msi".to_owned()),
                ("drivers[0]".to_owned(), "catalog VEN_1002&DEV_747E".to_owned()),
                ("drivers[1]".to_owned(), "catalog VEN_10DE".to_owned()),
            ]
        );
        let out = doc.to_string();
        assert!(out.contains("[[drivers]]  # kept"), "{out}");
        assert!(out.contains(r#"{ path = "local.ttf" }"#), "{out}");
        assert_eq!(out.matches(r#"url = "new""#).count(), 4, "{out}");
    }
}

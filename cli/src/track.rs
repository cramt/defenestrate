//! Finding the newest version of a tracked download (`config::Track`): what
//! a tracked source builds from without Nix, and what `dfnstrate pins
//! update` moves a pin to. Only ever resolves to a URL; the cache downloads
//! and hashes it.

use anyhow::{Context, Result, bail};

use crate::config::Track;

pub fn resolve(track: &Track) -> Result<String> {
    match track {
        Track::Github { github, asset } => github_asset(github.as_str(), asset).with_context(|| track.to_string()),
        Track::Catalog { catalog } => catalog_cab(catalog.as_str()).with_context(|| track.to_string()),
        Track::Update { update } => catalog_update(update.as_str()).with_context(|| track.to_string()),
    }
}

fn github_asset(repo: &str, pattern: &str) -> Result<String> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let mut req = ureq::get(&url).set("Accept", "application/vnd.github+json");
    // Unauthenticated, the API allows 60 requests an hour per IP.
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        req = req.set("Authorization", &format!("Bearer {token}"));
    }
    let release: serde_json::Value = req.call().with_context(|| format!("GET {url}"))?.into_json()?;
    let assets = release["assets"].as_array().context("the release has no assets list")?;
    let matching: Vec<&str> = assets
        .iter()
        .filter(|a| a["name"].as_str().is_some_and(|n| glob(pattern, n)))
        .filter_map(|a| a["browser_download_url"].as_str())
        .collect();
    match matching.as_slice() {
        [one] => Ok((*one).to_owned()),
        [] => bail!("no asset of {repo}'s latest release ({}) matches {pattern:?}", release["tag_name"].as_str().unwrap_or("?")),
        many => bail!("{} assets match {pattern:?}, which should pick one: {}", many.len(), many.join(", ")),
    }
}

/// `*` matches any run of characters; everything else itself.
fn glob(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((head, rest)) => {
            let Some(name) = name.strip_prefix(head) else { return false };
            (0..=name.len()).any(|i| name.is_char_boundary(i) && glob(rest, &name[i..]))
        }
    }
}

const CATALOG: &str = "https://www.catalog.update.microsoft.com";

fn search(query: &str) -> Result<String> {
    Ok(ureq::get(&format!("{CATALOG}/Search.aspx")).query("q", query).call().context("searching the Update Catalog")?.into_string()?)
}

fn downloads(id: &str) -> Result<Vec<String>> {
    let ids = format!(r#"[{{"size":0,"languages":"","uidInfo":"{id}","updateID":"{id}"}}]"#);
    let dialog = ureq::post(&format!("{CATALOG}/DownloadDialog.aspx")).send_form(&[("updateIDs", &ids)]).context("asking the Update Catalog for the download")?.into_string()?;
    Ok(download_urls(&dialog))
}

fn catalog_update(query: &str) -> Result<String> {
    let rows = table(&search(query)?);
    let newest = newest_update(&rows, query).with_context(|| format!("the Update Catalog has no update titled like {query:?}"))?;
    let urls = downloads(&newest.id)?;
    update_msu(&newest, &urls)
}

/// An update's title, with its release date.
#[derive(Debug, PartialEq)]
struct Update {
    id: String,
    title: String,
    /// (year, month, day)
    date: (u32, u32, u32),
}

/// The newest whose title contains `query`, previews left out: they're the
/// optional non-security ones, which a month's real update supersedes.
fn newest_update(rows: &[(String, Vec<String>)], query: &str) -> Option<Update> {
    let query = query.to_lowercase();
    rows.iter()
        .filter_map(|(id, cells)| {
            let title = cells.get(1)?;
            let lower = title.to_lowercase();
            if !lower.contains(&query) || lower.contains("preview") {
                return None;
            }
            Some(Update { id: id.clone(), title: title.clone(), date: date(cells.get(4)?)? })
        })
        .max_by_key(|u| u.date)
}

/// The catalog's M/D/YYYY.
fn date(s: &str) -> Option<(u32, u32, u32)> {
    let mut parts = s.split('/').map(|p| p.parse::<u32>().ok());
    let (m, d, y) = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some((y, m, d))
}

/// Since 24H2 a cumulative update downloads as itself plus the checkpoint
/// updates before it (KB5043080 for all of 24H2 to 26H2): the file named
/// after the title's KB is the update. An ISO older than a checkpoint needs
/// that one too, as another `updates` entry before it, or DISM refuses.
fn update_msu(u: &Update, urls: &[String]) -> Result<String> {
    let kb = u
        .title
        .split(['(', ')'])
        .find(|p| p.starts_with("KB") && p.len() > 2 && p[2..].bytes().all(|b| b.is_ascii_digit()))
        .with_context(|| format!("{} has no KB number in its title", u.title))?
        .to_lowercase();
    let mut msus: Vec<&String> = urls.iter().filter(|url| url.ends_with(".msu") && url.rsplit('/').next().is_some_and(|f| f.contains(&format!("-{kb}-")))).collect();
    msus.dedup();
    match msus.as_slice() {
        [one] => Ok((*one).clone()),
        [] => bail!("{} ({}) has no .msu named after {kb}: {}", u.title, u.id, urls.join(", ")),
        _ => bail!("{} ({}) has several .msu files named after {kb}", u.title, u.id),
    }
}

fn catalog_cab(query: &str) -> Result<String> {
    let rows = search_rows(&search(query)?);
    let newest = newest(&rows).with_context(|| format!("the Update Catalog has no client driver for {query:?}"))?;
    let mut cabs: Vec<String> = downloads(&newest.id)?.into_iter().filter(|u| u.ends_with(".cab")).collect();
    cabs.dedup();
    match cabs.as_slice() {
        [one] => Ok(one.clone()),
        [] => bail!("{} ({}) has no .cab to download", newest.title, newest.id),
        _ => bail!("{} ({}) is several .cabs: {}", newest.title, newest.id, cabs.join(", ")),
    }
}

#[derive(Debug, PartialEq)]
struct Row {
    id: String,
    title: String,
    products: String,
    version: Vec<u32>,
}

/// The newest client driver. Search results come newest first, 25 to a
/// page, so the first page has it.
fn newest(rows: &[Row]) -> Option<&Row> {
    rows.iter().filter(|r| !r.products.contains("Server")).max_by(|a, b| a.version.cmp(&b.version))
}

/// The result table of a catalog search: rows are `<tr id="<update id>_R<n>">`,
/// the cells a checkbox, title, products, classification, date, version, size.
/// The version column is often "n/a", but the title always ends in it.
fn search_rows(html: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    for (id, cells) in table(html) {
        let (Some(title), Some(products)) = (cells.get(1), cells.get(2)) else { continue };
        let Some(version) = title.split([' ', '(', ')']).rev().find_map(dotted) else { continue };
        rows.push(Row { id, title: title.clone(), products: products.clone(), version });
    }
    rows
}

/// Each result row's update ID and cell texts.
fn table(html: &str) -> Vec<(String, Vec<String>)> {
    let mut rows = Vec::new();
    for chunk in html.split("<tr id=\"").skip(1) {
        let Some((id, _)) = chunk.split_once("_R") else { continue };
        if id.len() != 36 || !id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
            continue;
        }
        let body = chunk.split_once("</tr>").map_or(chunk, |(b, _)| b);
        rows.push((id.to_owned(), cells(body)));
    }
    rows
}

fn cells(row: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = row;
    while let Some(start) = rest.find("<td") {
        let after = &rest[start..];
        let Some(open_end) = after.find('>') else { break };
        let inner = &after[open_end + 1..];
        let Some(close) = inner.find("</td>") else { break };
        out.push(text(&inner[..close]));
        rest = &inner[close + 5..];
    }
    out
}

/// Tags dropped, the few entities the catalog uses decoded, whitespace
/// collapsed.
fn text(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out.replace("&amp;", "&").replace("&nbsp;", " ").replace("&quot;", "\"").replace("&#39;", "'").replace("&lt;", "<").replace("&gt;", ">");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A driver version: four dot-separated numbers.
fn dotted(s: &str) -> Option<Vec<u32>> {
    let parts: Vec<u32> = s.split('.').map(|p| p.parse().ok()).collect::<Option<_>>()?;
    (parts.len() == 4).then_some(parts)
}

/// The dialog sets `downloadInformation[0].files[n].url = '<url>';` in a script.
fn download_urls(html: &str) -> Vec<String> {
    html.split(".url = '").skip(1).filter_map(|s| s.split_once('\'').map(|(u, _)| u.to_owned())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob("wsl.*.x64.msi", "wsl.2.6.1.0.x64.msi"));
        assert!(!glob("wsl.*.x64.msi", "wsl.2.6.1.0.arm64.msi"));
        assert!(glob("Iosevka.tar.xz", "Iosevka.tar.xz"));
        assert!(!glob("Iosevka.tar.xz", "IosevkaTerm.tar.xz"));
        assert!(glob("*", "anything"));
    }

    // Trimmed from a real search for VEN_1002&DEV_747E (2026-11-16).
    const SEARCH: &str = r#"<table><tr id="headerRow"><td>Title</td></tr>
<tr id="0d385684-aaaa-bbbb-cccc-000000000001_R1">
  <td class="resultsbottomBorder"><input type="checkbox"/></td>
  <td class="resultsbottomBorder resultspadding"><a id="0d385684_link" href="javascript:void(0);" onclick='goToDetails("0d385684");'>
     Advanced Micro Devices, Inc. - Display - 32.0.12033.1030
  </a></td>
  <td>Windows 11 Client, version 24H2 and later, Servicing Drivers</td>
  <td>Drivers (Video)</td><td>11/26/2024</td><td>n/a</td><td>668.4 MB</td>
</tr>
<tr id="e431ff0d-1013-45cb-9696-e0fff99e797f_R2">
  <td><input type="checkbox"/></td>
  <td><a>Advanced Micro Devices, Inc. Driver Update (32.0.21030.2001)</a></td>
  <td>Windows 11 Client, version 22H2 and later, Servicing Drivers</td>
  <td>Drivers (Video)</td><td>9/24/2025</td><td>32.0.21030.2001</td><td>724.6 MB</td>
</tr>
<tr id="99999999-1013-45cb-9696-e0fff99e797f_R3">
  <td><input type="checkbox"/></td>
  <td><a>Advanced Micro Devices, Inc. - Display - 33.0.1.1</a></td>
  <td>Windows Server 2025 and later, Servicing Drivers</td>
  <td>Drivers (Video)</td><td>10/1/2025</td><td>n/a</td><td>700 MB</td>
</tr></table>"#;

    #[test]
    fn picks_the_newest_client_driver() {
        let rows = search_rows(SEARCH);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].version, [32, 0, 12033, 1030]);
        let newest = newest(&rows).unwrap();
        assert_eq!(newest.id, "e431ff0d-1013-45cb-9696-e0fff99e797f");
        assert_eq!(newest.version, [32, 0, 21030, 2001]);
    }

    // Trimmed from a real search for "Cumulative Update for Windows 11,
    // version 26H2 for x64-based Systems" (2026-11-17).
    const UPDATES: &str = r#"<table>
<tr id="dc390c68-90c7-4f1f-b4ac-989d1f7279f8_R1"><td><input type="checkbox"/></td>
  <td><a>2026-11 Cumulative Update for Windows 11, version 26H2 for x64-based Systems (KB5132061) (26300.9974)</a></td>
  <td>Windows 11</td><td>Security Updates</td><td>11/10/2026</td><td>n/a</td><td>4511.6 MB</td></tr>
<tr id="aaaaaaaa-90c7-4f1f-b4ac-989d1f7279f8_R2"><td><input type="checkbox"/></td>
  <td><a>2026-11 Cumulative Update Preview for Windows 11, version 26H2 for x64-based Systems (KB5136000) (26300.10001)</a></td>
  <td>Windows 11</td><td>Updates</td><td>11/25/2026</td><td>n/a</td><td>4600 MB</td></tr>
<tr id="bbbbbbbb-90c7-4f1f-b4ac-989d1f7279f8_R3"><td><input type="checkbox"/></td>
  <td><a>2026-10 Cumulative Update for Windows 11, version 26H2 for x64-based Systems (KB5130000) (26300.9800)</a></td>
  <td>Windows 11</td><td>Security Updates</td><td>10/13/2026</td><td>n/a</td><td>4400 MB</td></tr>
<tr id="cccccccc-90c7-4f1f-b4ac-989d1f7279f8_R4"><td><input type="checkbox"/></td>
  <td><a>2026-11 Cumulative Update for Windows 11, version 25H2 for x64-based Systems (KB5132061) (26200.9974)</a></td>
  <td>Windows 11</td><td>Security Updates</td><td>11/12/2026</td><td>n/a</td><td>4511.6 MB</td></tr>
</table>"#;

    #[test]
    fn picks_the_newest_non_preview_update() {
        let rows = table(UPDATES);
        let u = newest_update(&rows, "Cumulative Update for Windows 11, version 26H2 for x64-based Systems").unwrap();
        assert_eq!(u.id, "dc390c68-90c7-4f1f-b4ac-989d1f7279f8");
        assert_eq!(u.date, (2026, 11, 10));
        assert!(newest_update(&rows, "Cumulative Update for Windows 11, version 27H2").is_none());
    }

    #[test]
    fn takes_the_updates_own_msu_not_the_checkpoint() {
        let u = Update { id: "x".into(), title: "2026-11 Cumulative Update for Windows 11, version 26H2 for x64-based Systems (KB5132061) (26300.9974)".into(), date: (2026, 11, 10) };
        let urls = [
            "https://catalog.sf.dl.delivery.mp.microsoft.com/filestreamingservice/files/5775e69a/public/windows11.0-kb5132061-x64_aedc878e.msu".to_owned(),
            "https://catalog.sf.dl.delivery.mp.microsoft.com/filestreamingservice/files/d8b7f92b/public/windows11.0-kb5043080-x64_95344967.msu".to_owned(),
        ];
        assert_eq!(update_msu(&u, &urls).unwrap(), urls[0]);
        assert!(update_msu(&u, &urls[1..]).is_err());
    }

    #[test]
    fn reads_download_urls() {
        let dialog = "var downloadInformation = new Array();\ndownloadInformation[0].files[0].url = 'https://catalog.s.download.windowsupdate.com/d/msdownload/update/driver/drvs/2025/12/x_dd87.cab';\ndownloadInformation[0].files[0].digest = 'abc';";
        assert_eq!(download_urls(dialog), ["https://catalog.s.download.windowsupdate.com/d/msdownload/update/driver/drvs/2025/12/x_dd87.cab"]);
    }
}

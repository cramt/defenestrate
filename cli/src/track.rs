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

fn catalog_cab(query: &str) -> Result<String> {
    let html = ureq::get(&format!("{CATALOG}/Search.aspx")).query("q", query).call().context("searching the Update Catalog")?.into_string()?;
    let rows = search_rows(&html);
    let newest = newest(&rows).with_context(|| format!("the Update Catalog has no client driver for {query:?}"))?;
    let ids = format!(r#"[{{"size":0,"languages":"","uidInfo":"{0}","updateID":"{0}"}}]"#, newest.id);
    let dialog = ureq::post(&format!("{CATALOG}/DownloadDialog.aspx")).send_form(&[("updateIDs", &ids)]).context("asking the Update Catalog for the download")?.into_string()?;
    let mut cabs: Vec<String> = download_urls(&dialog).into_iter().filter(|u| u.ends_with(".cab")).collect();
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
    for chunk in html.split("<tr id=\"").skip(1) {
        let Some((id, _)) = chunk.split_once("_R") else { continue };
        if id.len() != 36 || !id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
            continue;
        }
        let body = chunk.split_once("</tr>").map_or(chunk, |(b, _)| b);
        let cells = cells(body);
        let (Some(title), Some(products)) = (cells.get(1), cells.get(2)) else { continue };
        let Some(version) = title.split([' ', '(', ')']).rev().find_map(dotted) else { continue };
        rows.push(Row { id: id.to_owned(), title: title.clone(), products: products.clone(), version });
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

    #[test]
    fn reads_download_urls() {
        let dialog = "var downloadInformation = new Array();\ndownloadInformation[0].files[0].url = 'https://catalog.s.download.windowsupdate.com/d/msdownload/update/driver/drvs/2025/12/x_dd87.cab';\ndownloadInformation[0].files[0].digest = 'abc';";
        assert_eq!(download_urls(dialog), ["https://catalog.s.download.windowsupdate.com/d/msdownload/update/driver/drvs/2025/12/x_dd87.cab"]);
    }
}

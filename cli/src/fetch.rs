//! Getting Microsoft's stock ISO.
//!
//! The download links come from microsoft.com's download page and expire
//! after a day. The page's API rejects plain HTTP clients ("Sentinel marked
//! this request as rejected"), so a headless Chromium clicks through the
//! page like a person would and hands back the link, which is then
//! downloaded and checked against the pin.

use std::ffi::OsStr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use headless_chrome::{Browser, LaunchOptions};

use crate::cache::Cache;
use crate::config::IsoPin;
use crate::tools;

const PAGE: &str = "https://www.microsoft.com/en-us/software-download/windows11";

#[derive(clap::Args)]
pub struct SourceArgs {
    /// The stock ISO, if you have it already. Must be the one the config pins.
    #[arg(long, value_name = "FILE")]
    iso: Option<PathBuf>,
    #[command(flatten)]
    browser: BrowserArgs,
}

impl SourceArgs {
    /// The pinned ISO on disk: `--iso`, the cache, or a fresh download.
    pub fn resolve(&self, pin: &IsoPin, cache: &Cache) -> Result<PathBuf> {
        if let Some(path) = &self.iso {
            return cache.add_local(path, &pin.hash);
        }
        if let Some(p) = cache.lookup(&pin.hash) {
            return Ok(p);
        }
        if cache.offline {
            bail!("{} ({}) isn't in the cache at {} and --offline is set", pin.name, pin.hash, cache.dir.display());
        }
        download(pin, cache, &self.browser, false)
    }
}

#[derive(clap::Args)]
pub struct BrowserArgs {
    /// Chromium or Chrome to drive [default: chromium, chromium-browser,
    /// google-chrome-stable or google-chrome on PATH]
    #[arg(long, env = "DFNSTRATE_BROWSER", value_name = "PATH")]
    browser: Option<PathBuf>,
}

#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    browser: BrowserArgs,
    /// Microsoft has moved on from the pinned build: download whatever the
    /// page serves now and print the pin to put in the config.
    #[arg(long)]
    update: bool,
    /// Print a fresh download link for the pinned ISO and stop, to download
    /// it with something else. Links expire after a day.
    #[arg(long, conflicts_with_all = ["update", "out"])]
    link_only: bool,
    /// Also copy the ISO here (the Nix fetcher's $out).
    #[arg(short, long, value_name = "FILE")]
    out: Option<PathBuf>,
}

pub fn run(pin: &IsoPin, cache: &Cache, args: &Args) -> Result<()> {
    if args.link_only {
        let url = link(&pin.language, &args.browser)?;
        if served_name(&url) != pin.name {
            bail!("Microsoft now serves {}, not the pinned {}", served_name(&url), pin.name);
        }
        println!("{url}");
        return Ok(());
    }
    let path = match (args.update, cache.lookup(&pin.hash)) {
        (false, Some(p)) => {
            eprintln!("{} is already in the cache", pin.name);
            p
        }
        _ => download(pin, cache, &args.browser, args.update)?,
    };
    if let Some(out) = &args.out {
        // A hard link where possible: it's ~8 GB.
        if std::fs::hard_link(&path, out).is_err() {
            std::fs::copy(&path, out).with_context(|| format!("copying to {}", out.display()))?;
        }
    }
    println!("{}", path.display());
    Ok(())
}

fn download(pin: &IsoPin, cache: &Cache, browser: &BrowserArgs, update: bool) -> Result<PathBuf> {
    let url = link(&pin.language, browser)?;
    let served = served_name(&url);
    if !update && served != pin.name {
        bail!(
            "Microsoft now serves {served}, not the pinned {}. Run `dfnstrate fetch --update` for the new pin \
             (and rebuild: a new build can need config changes).",
            pin.name
        );
    }
    let got = cache.download(&url)?;
    if update {
        eprintln!("pin for the config:\n\n[iso]\nname = \"{served}\"\nhash = \"{}\"\nlanguage = \"{}\"\n", got.hash, pin.language);
        return cache.keep(got);
    }
    if got.hash != pin.hash {
        let _ = std::fs::remove_file(&got.path);
        bail!("{served} downloaded with hash {}, but the config pins {}", got.hash, pin.hash);
    }
    cache.keep(got)
}

fn served_name(url: &str) -> String {
    url.split('?').next().and_then(|u| u.rsplit('/').next()).unwrap_or_default().to_owned()
}

/// A fresh download link for the x64 ISO in `language`.
fn link(language: &str, args: &BrowserArgs) -> Result<String> {
    let path = match &args.browser {
        Some(p) => p.clone(),
        None => ["chromium", "chromium-browser", "google-chrome-stable", "google-chrome"]
            .iter()
            .find_map(tools::which)
            .context("no Chromium or Chrome on PATH; install chromium or pass --browser")?,
    };
    eprintln!("asking {PAGE} for a link, with {}", path.display());
    let browser = Browser::new(
        LaunchOptions::default_builder()
            .path(Some(path))
            .headless(true)
            // Chromium's own sandbox needs user namespaces, which the Nix
            // build sandbox doesn't give it. It only ever loads microsoft.com.
            .sandbox(false)
            .idle_browser_timeout(Duration::from_secs(300))
            .args(vec![OsStr::new("--disable-dev-shm-usage")])
            .build()
            .map_err(|e| anyhow::anyhow!("{e}"))?,
    )
    .context("starting the browser")?;
    let tab = browser.new_tab()?;
    // "HeadlessChrome" in the user agent is the obvious tell.
    let ua = browser.get_version()?.user_agent.replace("HeadlessChrome", "Chrome");
    tab.set_user_agent(&ua, None, None)?;
    tab.set_default_timeout(Duration::from_secs(90));
    tab.navigate_to(PAGE)?.wait_until_navigated()?;
    let result = tab.evaluate(&script(language), true)?;
    // Objects come back by reference, strings by value, so the script
    // returns JSON.
    let json = result.value.and_then(|v| v.as_str().map(str::to_owned)).context("the page script returned nothing")?;
    let value: serde_json::Value = serde_json::from_str(&json)?;
    if let Some(href) = value.get("href").and_then(|v| v.as_str()) {
        return Ok(href.to_owned());
    }
    bail!("the download page didn't give a link: {}", value.get("error").and_then(|v| v.as_str()).unwrap_or("(no message)"))
}

/// Picks the x64 multi-edition ISO, then the language, and waits for the
/// download button, the same clicks a person makes.
fn script(language: &str) -> String {
    let language = serde_json::to_string(language).unwrap();
    format!(
        r#"(async () => {{
  const language = {language};
  const answer = async () => {{
  const sleep = ms => new Promise(r => setTimeout(r, ms));
  const visible = async (sel, ms) => {{
    for (const end = Date.now() + ms; Date.now() < end; await sleep(250)) {{
      const e = document.querySelector(sel);
      if (e && e.offsetParent !== null) return e;
    }}
    throw new Error('the page never showed ' + sel);
  }};
  try {{
    const edition = await visible('#product-edition', 30000);
    const iso = [...edition.options].find(o => /x64/.test(o.text));
    if (!iso) return {{ error: 'no x64 ISO among: ' + [...edition.options].map(o => o.text).join(', ') }};
    edition.value = iso.value;
    edition.dispatchEvent(new Event('change', {{ bubbles: true }}));
    document.querySelector('#submit-product-edition').click();
    const languages = await visible('#product-languages', 30000);
    await sleep(500);
    const option = [...languages.options].find(o => o.text.trim() === language);
    if (!option) return {{ error: 'no language "' + language + '" among: ' + [...languages.options].map(o => o.text.trim()).join(', ') }};
    languages.value = option.value;
    languages.dispatchEvent(new Event('change', {{ bubbles: true }}));
    document.querySelector('#submit-sku').click();
    for (const end = Date.now() + 30000; Date.now() < end; await sleep(500)) {{
      const a = [...document.querySelectorAll('a')].find(a => /\.iso(\?|$)/.test(a.href));
      if (a) return {{ href: a.href }};
    }}
    return {{ error: document.body.innerText.replace(/\s+/g, ' ').slice(0, 800) }};
  }} catch (e) {{
    return {{ error: String(e) }};
  }}
  }};
  return JSON.stringify(await answer());
}})()"#
    )
}

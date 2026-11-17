// macOS, Linux and Windows replace themselves. Elsewhere the install half
// still compiles, so the workspace drives one state machine on every platform,
// and goes unused.
#![cfg_attr(
    not(any(target_os = "macos", target_os = "linux", windows)),
    allow(dead_code)
)]

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use rust_i18n::t;
use serde::Deserialize;

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
mod download;
#[cfg(any(target_os = "linux", windows, all(test, target_os = "macos")))]
mod identity;
// Also built into the macOS tests, which are the ones a Mac can run.
#[cfg(any(target_os = "linux", all(test, target_os = "macos")))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(windows, all(test, target_os = "macos")))]
#[cfg_attr(not(windows), allow(dead_code))]
mod windows;

#[cfg(target_os = "linux")]
pub(crate) use linux::{Installer, install};
#[cfg(target_os = "macos")]
pub(crate) use macos::{Installer, install};
#[cfg(windows)]
pub(crate) use windows::{Installer, install};

/// Where the check reads release metadata from, unless the settings name
/// another. The asset and signature URLs come from that JSON, so a proxy or a
/// mirror of it replaces every address the update uses.
pub(crate) const DEFAULT_FEED: &str =
    "https://api.github.com/repos/nutlet85/dbdelve/releases/latest";

const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// How often a running app asks again, so a session left open for days still
/// hears about a release.
pub(crate) const RECHECK: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Clone, Debug)]
pub(crate) struct Release {
    pub(crate) version: String,
    pub(crate) url: String,
}

/// A newer release, and how far this install has got with putting it in place.
pub(crate) struct Update {
    pub(crate) release: Release,
    pub(crate) state: State,
}

pub(crate) enum State {
    /// This install cannot replace itself, so the release page is all there is.
    Link,
    Ready(Arc<Installer>),
    Running(Arc<Progress>),
    /// Why it stopped, in words for the dropdown; the detail is in the log.
    /// The installer is kept so the same update can be tried again.
    Failed(&'static str, Arc<Installer>),
    /// Another DBDelve has replaced this install since it started, so trying
    /// again cannot succeed; restarting into the install at this path runs
    /// what it put there.
    #[cfg_attr(not(any(target_os = "linux", windows)), allow(dead_code))]
    Superseded(PathBuf),
}

impl State {
    /// What an install that stopped for `reason` leaves.
    pub(crate) fn stopped(reason: &'static str, installer: Arc<Installer>) -> Self {
        #[cfg(any(target_os = "linux", windows))]
        if reason == identity::ALREADY_UPDATED {
            return State::Superseded(installer.installed());
        }
        State::Failed(reason, installer)
    }
}

/// Written by the install on its own thread, read by the titlebar on a tick.
#[derive(Default)]
pub(crate) struct Progress {
    received: AtomicU64,
    total: AtomicU64,
    installing: AtomicBool,
}

impl Progress {
    pub(crate) fn downloaded(&self, received: u64, total: u64) {
        self.total.store(total, Ordering::Relaxed);
        self.received.store(received, Ordering::Relaxed);
    }

    pub(crate) fn installing(&self) {
        self.installing.store(true, Ordering::Relaxed);
    }

    pub(crate) fn is_installing(&self) -> bool {
        self.installing.load(Ordering::Relaxed)
    }

    /// What the titlebar pill reads while the update runs. Three stages
    /// rather than one word: before the first byte there is no percentage to
    /// show, and saying the app is installing before the download has started
    /// reads as if the replacement were already under way.
    pub(crate) fn label(&self) -> String {
        if self.is_installing() {
            return t!("update.installing").to_string();
        }
        let total = self.total.load(Ordering::Relaxed);
        if total == 0 {
            return t!("update.preparing").to_string();
        }
        let received = self.received.load(Ordering::Relaxed);
        let percent = ((received * 100) / total).min(100);
        t!("update.downloading", percent = percent).to_string()
    }
}

#[derive(Deserialize)]
struct Latest {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    assets: Vec<LatestAsset>,
}

#[derive(Deserialize)]
struct LatestAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

/// An update file and its minisign signature, as the release lists them.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Asset {
    pub(crate) name: String,
    pub(crate) url: String,
    pub(crate) size: u64,
    pub(crate) signature_url: String,
}

/// The latest release, if it is newer than this build, and whether this install
/// can put it in place itself. Blocking, so it belongs on the background
/// executor. Any failure is `None`: being offline is not something to tell
/// anyone about. `custom_feed` comes from the settings, empty for GitHub
/// itself; a mirror is safe to read, since whatever it serves still has to
/// verify against the release key compiled in.
pub(crate) fn newer_release(custom_feed: &str, proxy: &str) -> Option<Update> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .user_agent(concat!("dbdelve/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let feed = feed(custom_feed);
    let mut latest: Latest = match agent
        .get(&feed)
        .header("Accept", "application/vnd.github+json")
        .call()
        .and_then(|mut response| response.body_mut().read_json())
    {
        Ok(latest) => latest,
        Err(error) => {
            tracing::info!(feed, "update check failed: {error}");
            return None;
        }
    };
    through_proxy(proxy, &mut latest.assets);
    let version = latest.tag_name.trim_start_matches('v').to_string();
    if !is_newer(&version, CURRENT) {
        return None;
    }
    tracing::info!(version, current = CURRENT, "a newer release is out");
    if !proxy.trim().is_empty() {
        tracing::info!(
            prefix = proxy.trim(),
            "its downloads will go through a proxy"
        );
    }
    Some(Update {
        state: installer(&version, &latest.assets)
            .map_or(State::Link, |installer| State::Ready(Arc::new(installer))),
        release: Release {
            version,
            url: latest.html_url,
        },
    })
}

/// Points the release's downloads at a proxy prefix when one is set: the
/// metadata still comes from the feed, but every asset and its signature is
/// fetched as `<prefix>/<github url>`. Only `github.com` addresses are
/// rewritten, so a mirror's own URLs and the release page stay as they are.
fn through_proxy(prefix: &str, assets: &mut [LatestAsset]) {
    let prefix = prefix.trim().trim_end_matches('/');
    if prefix.is_empty() {
        return;
    }
    for asset in assets {
        if asset
            .browser_download_url
            .starts_with("https://github.com/")
        {
            asset.browser_download_url = format!("{prefix}/{}", asset.browser_download_url);
        }
    }
}

/// Just ahead of `App::restart` into an install.
pub(crate) fn before_restart() {
    #[cfg(target_os = "linux")]
    linux::close_inherited_files();
}

/// Notes what this process was installed as, before any update check can find
/// that another process has replaced it since. Once, at startup.
pub(crate) fn remember_install() {
    #[cfg(target_os = "linux")]
    linux::remember_install();
    #[cfg(windows)]
    windows::remember_install();
}

/// The feed the check reads: the settings' source when one is set, GitHub
/// itself otherwise. Only a test build also takes it from the environment, so
/// a shipped one changes sources through its own settings, and whatever it is
/// pointed at still has to satisfy the release key.
fn feed(custom: &str) -> String {
    #[cfg(feature = "update-test-hooks")]
    if let Some(feed) = std::env::var("DBDELVE_UPDATE_FEED")
        .ok()
        .filter(|feed| !feed.is_empty())
    {
        return feed;
    }
    let custom = custom.trim();
    if custom.is_empty() {
        DEFAULT_FEED.to_string()
    } else {
        custom.to_string()
    }
}

/// Whether a found update installs without anyone clicking: only a test build
/// told to by `DBDELVE_UPDATE_AUTO=1`, for an end-to-end test of the updater
/// that has nobody at the screen.
pub(crate) fn installs_unattended() -> bool {
    #[cfg(feature = "update-test-hooks")]
    if std::env::var("DBDELVE_UPDATE_AUTO").is_ok_and(|value| value == "1") {
        return true;
    }
    false
}

/// Exactly the asset called `name` and its `.minisig`, or nothing: an update
/// without its signature is not one this app will install.
fn asset(name: &str, assets: &[LatestAsset]) -> Option<Asset> {
    let signature = format!("{name}.minisig");
    let find = |wanted: &str| assets.iter().find(|asset| asset.name == wanted);
    let (asset, signature) = (find(name)?, find(&signature)?);
    Some(Asset {
        name: asset.name.clone(),
        url: asset.browser_download_url.clone(),
        size: asset.size,
        signature_url: signature.browser_download_url.clone(),
    })
}

#[cfg(target_os = "macos")]
fn installer(version: &str, assets: &[LatestAsset]) -> Option<Installer> {
    macos::installer(version, asset(&format!("DBDelve-{version}.dmg"), assets))
}

#[cfg(target_os = "linux")]
fn installer(version: &str, assets: &[LatestAsset]) -> Option<Installer> {
    linux::installer(version, |name| asset(name, assets))
}

#[cfg(windows)]
fn installer(version: &str, assets: &[LatestAsset]) -> Option<Installer> {
    windows::installer(version, |name| asset(name, assets))
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
pub(crate) enum Installer {}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn installer(_: &str, _: &[LatestAsset]) -> Option<Installer> {
    None
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
pub(crate) fn install(installer: &Installer, _: &Progress) -> Result<PathBuf, &'static str> {
    match *installer {}
}

/// Numeric, so 0.2.10 beats 0.2.9, and a build ahead of the latest release
/// (a local one, say) is not told to "update" backwards.
fn is_newer(candidate: &str, current: &str) -> bool {
    let parse = |version: &str| {
        version
            .split('.')
            .map(|part| part.parse::<u64>().ok())
            .collect::<Option<Vec<_>>>()
    };
    matches!((parse(candidate), parse(current)), (Some(candidate), Some(current)) if candidate > current)
}

#[cfg(test)]
mod tests {
    use super::{Asset, Latest, Progress, asset, is_newer};

    #[test]
    fn compares_versions_numerically() {
        assert!(is_newer("0.2.1", "0.2.0"));
        assert!(is_newer("0.2.10", "0.2.9"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.1.9", "0.2.0"));
        assert!(!is_newer("0.3.0-beta", "0.2.0"));
    }

    /// Trimmed from a real `releases/latest` response.
    const LATEST: &str = r#"{
        "tag_name": "v0.8.0",
        "html_url": "https://github.com/ShayanAbbas1/dbdelve/releases/tag/v0.8.0",
        "assets": [
            {"name": "DBDelve-0.8.0.dmg.minisig", "size": 310,
             "browser_download_url": "https://example.test/DBDelve-0.8.0.dmg.minisig"},
            {"name": "DBDelve-0.7.9.dmg", "size": 1,
             "browser_download_url": "https://example.test/DBDelve-0.7.9.dmg"},
            {"name": "dbdelve-0.8.0-linux-x86_64.tar.gz", "size": 2,
             "browser_download_url": "https://example.test/dbdelve-0.8.0-linux-x86_64.tar.gz"},
            {"name": "dbdelve-0.8.0-linux-x86_64.tar.gz.minisig", "size": 310,
             "browser_download_url": "https://example.test/dbdelve-0.8.0-linux-x86_64.tar.gz.minisig"},
            {"name": "dbdelve-0.8.0-x86_64.AppImage", "size": 3,
             "browser_download_url": "https://example.test/dbdelve-0.8.0-x86_64.AppImage"},
            {"name": "dbdelve-0.8.0-x86_64.AppImage.minisig", "size": 310,
             "browser_download_url": "https://example.test/dbdelve-0.8.0-x86_64.AppImage.minisig"},
            {"name": "dbdelve-0.8.0-aarch64.AppImage", "size": 4,
             "browser_download_url": "https://example.test/dbdelve-0.8.0-aarch64.AppImage"},
            {"name": "DBDelve-0.8.0.dmg", "size": 41943040,
             "browser_download_url": "https://example.test/DBDelve-0.8.0.dmg"}
        ]
    }"#;

    #[test]
    fn picks_the_named_asset_and_its_signature() {
        let latest: Latest = serde_json::from_str(LATEST).unwrap();
        let listed = |name: &str, size: u64| Asset {
            name: name.into(),
            url: format!("https://example.test/{name}"),
            size,
            signature_url: format!("https://example.test/{name}.minisig"),
        };
        for (name, size) in [
            ("DBDelve-0.8.0.dmg", 41943040),
            ("dbdelve-0.8.0-linux-x86_64.tar.gz", 2),
            ("dbdelve-0.8.0-x86_64.AppImage", 3),
        ] {
            assert_eq!(asset(name, &latest.assets), Some(listed(name, size)));
        }
        // Another version's dmg is not this release's update.
        assert_eq!(asset("DBDelve-0.7.9.dmg", &latest.assets), None);
        // Nor is an asset the release does not have.
        assert_eq!(
            asset("dbdelve-0.8.0-linux-aarch64.tar.gz", &latest.assets),
            None
        );
    }

    #[test]
    fn an_asset_without_its_signature_is_not_an_update() {
        let latest: Latest = serde_json::from_str(LATEST).unwrap();
        // Listed, but its .minisig is not.
        assert_eq!(
            asset("dbdelve-0.8.0-aarch64.AppImage", &latest.assets),
            None
        );

        let mut unsigned: Latest = serde_json::from_str(LATEST).unwrap();
        unsigned
            .assets
            .retain(|asset| !asset.name.ends_with(".minisig"));
        for name in [
            "DBDelve-0.8.0.dmg",
            "dbdelve-0.8.0-linux-x86_64.tar.gz",
            "dbdelve-0.8.0-x86_64.AppImage",
        ] {
            assert_eq!(asset(name, &unsigned.assets), None, "{name}");
        }

        let without_assets: Latest =
            serde_json::from_str(r#"{"tag_name": "v0.8.0", "html_url": "https://example.test"}"#)
                .unwrap();
        assert_eq!(asset("DBDelve-0.8.0.dmg", &without_assets.assets), None);
    }

    #[test]
    fn progress_reads_preparing_then_a_percentage_then_installing() {
        let progress = Progress::default();
        assert_eq!(progress.label(), "Preparing\u{2026}");
        progress.downloaded(21, 50);
        assert_eq!(progress.label(), "Downloading\u{2026} 42%");
        progress.installing();
        assert_eq!(progress.label(), "Installing\u{2026}");
    }

    #[test]
    fn the_feed_is_the_settings_source_or_github() {
        use super::{DEFAULT_FEED, feed};
        assert_eq!(feed(""), DEFAULT_FEED);
        assert_eq!(feed("   "), DEFAULT_FEED);
        assert_eq!(
            feed(" https://mirror.test/latest "),
            "https://mirror.test/latest"
        );
    }

    #[test]
    fn a_proxy_prefix_rewrites_github_downloads_only() {
        use super::{LatestAsset, through_proxy};

        let github = "https://github.com/nutlet85/dbdelve/releases/download/v9.9.9/x.zip";
        let asset = |url: &str| LatestAsset {
            name: "x".into(),
            browser_download_url: url.into(),
            size: 1,
        };
        let mut assets = vec![
            asset(github),
            asset(&format!("{github}.minisig")),
            asset("https://mirror.test/other"),
        ];
        // Spaces and trailing slashes are forgiven; one slash joins.
        through_proxy(" https://ghfast.top/// ", &mut assets);
        assert_eq!(
            assets[0].browser_download_url,
            format!("https://ghfast.top/{github}")
        );
        assert_eq!(
            assets[1].browser_download_url,
            format!("https://ghfast.top/{github}.minisig")
        );
        assert_eq!(assets[2].browser_download_url, "https://mirror.test/other");

        // Empty means direct.
        let mut direct = vec![asset(github)];
        through_proxy("   ", &mut direct);
        assert_eq!(direct[0].browser_download_url, github);
    }
}

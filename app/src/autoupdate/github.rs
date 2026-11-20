//! Galaxy's public, signed GitHub update source. Release channels are fixed by the binary.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, bail, ensure, Context, Result};
use channel_versions::{GalaxyUpdate, VersionInfo};
use futures::StreamExt;
use futures_lite::io::AsyncWriteExt;
use galaxy_core::channel::{Channel, ChannelState};
use http_client::{Client, Response};
use lazy_static::lazy_static;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::build_info;

const REPOSITORY: &str = "rkw6086/galaxy";
const API: &str = "https://api.github.com/repos/rkw6086/galaxy";
const RELEASES: &str = "https://github.com/rkw6086/galaxy/releases";
const PUBLIC_KEY: &str = include_str!("../../../resources/galaxy-update-ed25519.pub");
const MAX_JSON: usize = 2 * 1024 * 1024;
const MAX_MANIFEST: usize = 64 * 1024;
const MAX_PAYLOAD: u64 = 2 * 1024 * 1024 * 1024;
const NORMAL_INTERVAL: i64 = 6 * 60 * 60;
const MANUAL_INTERVAL: i64 = 5 * 60;
static CHECK_LOCK: futures::lock::Mutex<()> = futures::lock::Mutex::new(());

lazy_static! {
    static ref SOURCE: Regex = Regex::new(r"^[0-9a-f]{40}$").unwrap();
    static ref HASH: Regex = Regex::new(r"^[0-9a-f]{64}$").unwrap();
    static ref VERSION: Regex = Regex::new(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$").unwrap();
    static ref BUILD: Regex = Regex::new(r"^([0-9a-f]{40})_([1-9][0-9]{0,18})-([1-9][0-9]{0,8})$").unwrap();
    static ref IDENTITY: Regex = Regex::new(r"^Nightly_([0-9]{8})_([0-9]{6})_CT_r([1-9][0-9]*)-([1-9][0-9]*)$").unwrap();
    static ref NIGHTLY_MARKER: Regex = Regex::new(r"<!-- galaxy-nightly:v1 commit=([0-9a-f]{40}) build=([0-9a-f]{40}_[1-9][0-9]{0,18}-[1-9][0-9]{0,8}) -->").unwrap();
    static ref STABLE_MARKER: Regex = Regex::new(r"<!-- galaxy-release:v1 tag=([0-9.]+) commit=([0-9a-f]{40}) -->").unwrap();
    static ref ASSET_MARKER: Regex = Regex::new(r"<!-- galaxy-nightly-assets:v1 build=([0-9a-f]{40}_[1-9][0-9]{0,18}-[1-9][0-9]{0,8}) identity=(Nightly_[A-Za-z0-9_-]+) -->").unwrap();
    static ref UPDATE_MARKER: Regex = Regex::new(r"<!-- galaxy-update:v1 manifest=(Galaxy_Update_[A-Za-z0-9_.-]+\.json) signature=(Galaxy_Update_[A-Za-z0-9_.-]+\.sig) -->").unwrap();
}

#[derive(Clone, Debug, Deserialize)]
struct Release {
    id: u64,
    tag_name: String,
    name: Option<String>,
    draft: bool,
    prerelease: bool,
    target_commitish: String,
    body: Option<String>,
    assets: Vec<Asset>,
}

#[derive(Clone, Debug, Deserialize)]
struct Asset {
    id: u64,
    name: String,
    size: u64,
    state: String,
    browser_download_url: String,
    digest: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: u32,
    channel: String,
    version: String,
    source_commit: String,
    build_id: Option<String>,
    artifact_identity: String,
    built_at: String,
    assets: BTreeMap<String, ManifestAsset>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestAsset {
    name: String,
    size: u64,
    sha256: String,
}

#[derive(Default, Serialize, Deserialize)]
struct CheckCache {
    checked_at: i64,
    attempted_at: i64,
    blocked_until: i64,
    version: Option<VersionInfo>,
}

pub fn is_galaxy_updater() -> bool {
    cfg!(all(feature = "autoupdate", not(target_family = "wasm")))
        && ChannelState::channel() == Channel::Oss
        && ChannelState::is_release_bundle()
        && build_info::official_github_build()
}

fn track() -> Result<&'static str> {
    match build_info::build_channel() {
        "beta" | "nightly" => Ok("nightly"),
        "release" => Ok("release"),
        _ => bail!("This build has no supported update channel."),
    }
}

fn endpoint() -> Result<&'static str> {
    Ok(if track()? == "nightly" {
        "/releases/tags/nightly"
    } else {
        "/releases/latest"
    })
}

pub fn release_page(version: Option<&VersionInfo>) -> String {
    if let Some(update) = version.and_then(|version| version.galaxy_update.as_ref()) {
        if validate_update(update).is_ok() {
            return format!("{RELEASES}/tag/{}", update.tag);
        }
    }
    if matches!(build_info::build_channel(), "beta" | "nightly") {
        format!("{RELEASES}/tag/nightly")
    } else {
        format!("{RELEASES}/latest")
    }
}

fn clock() -> i64 {
    chrono::Utc::now().timestamp()
}

fn cache_directory() -> Result<std::path::PathBuf> {
    let path = galaxy_core::paths::state_dir().join("github-updates");
    if let Ok(metadata) = std::fs::symlink_metadata(&path) {
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "Update cache directory is invalid."
        );
    } else {
        std::fs::create_dir_all(&path)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(path)
}

fn read_cache() -> CheckCache {
    let result = (|| -> Result<CheckCache> {
        let path = cache_directory()?.join(format!("{}.json", track()?));
        let metadata = std::fs::symlink_metadata(&path)?;
        ensure!(
            metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.len() <= 512 * 1024,
            "Invalid update cache file."
        );
        Ok(serde_json::from_slice(&std::fs::read(path)?)?)
    })();
    result.unwrap_or_default()
}

fn write_cache(cache: &CheckCache) -> Result<()> {
    let directory = cache_directory()?;
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
    serde_json::to_writer(temporary.as_file_mut(), cache)?;
    temporary.as_file_mut().sync_all()?;
    temporary.persist(directory.join(format!("{}.json", track()?)))?;
    Ok(())
}

fn invalidate_discovery() {
    let mut cache = read_cache();
    cache.checked_at = 0;
    cache.version = None;
    // Keep the last attempt and any server-imposed backoff intact.
    let _ = write_cache(&cache);
}

fn record_backoff(response: &Response) {
    let now = clock();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
        .map(|seconds| now.saturating_add(seconds.clamp(60, 86400)));
    let reset = response
        .headers()
        .get("x-ratelimit-reset")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > now)
        .map(|value| value.min(now + 86400));
    let mut cache = read_cache();
    cache.blocked_until = retry_after
        .into_iter()
        .chain(reset)
        .max()
        .unwrap_or(now + 3600);
    let _ = write_cache(&cache);
}

fn allowed_url(url: &url::Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none_or(|port| port == 443)
        && url.host_str().is_some_and(|host| {
            host == "github.com"
                || host == "api.github.com"
                || host.ends_with(".githubusercontent.com")
        })
}

fn client() -> Result<Client> {
    let builder = reqwest::Client::builder().user_agent(concat!(
        "Galaxy/",
        env!("CARGO_PKG_VERSION"),
        " updater"
    ));
    #[cfg(not(target_family = "wasm"))]
    let builder = builder
        .https_only(true)
        .connect_timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 || !allowed_url(attempt.url()) {
                attempt.error("Update redirect is outside GitHub HTTPS downloads")
            } else {
                attempt.follow()
            }
        }));
    // This dedicated client has no account, provider or GitHub authentication hooks.
    Ok(Client::from_client_builder(builder)?)
}

async fn request(client: &Client, url: &str) -> Result<Response> {
    ensure!(
        allowed_url(&url::Url::parse(url)?),
        "Invalid GitHub update URL."
    );
    let response = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2026-03-10")
        .timeout(Duration::from_secs(600))
        .send()
        .await?;
    if matches!(response.status().as_u16(), 403 | 429) {
        record_backoff(&response);
        bail!("GitHub's update limit was reached. Galaxy will retry after the cooldown.");
    }
    ensure!(
        response.status().is_success(),
        "GitHub update request failed (HTTP {}).",
        response.status().as_u16()
    );
    ensure!(
        allowed_url(response.url()),
        "GitHub update redirect is invalid."
    );
    Ok(response)
}

async fn bounded_bytes(response: Response, limit: usize) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let stream = response.bytes_stream();
    futures::pin_mut!(stream);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        ensure!(
            output.len().saturating_add(chunk.len()) <= limit,
            "Update metadata exceeded its size limit."
        );
        output.extend_from_slice(&chunk);
    }
    Ok(output)
}

async fn current_release(client: &Client) -> Result<Release> {
    let bytes = bounded_bytes(
        request(client, &format!("{API}{}", endpoint()?)).await?,
        MAX_JSON,
    )
    .await?;
    let release: Release =
        serde_json::from_slice(&bytes).context("GitHub returned invalid release metadata")?;
    if let Err(error) = validate_release(&release) {
        invalidate_discovery();
        return Err(error);
    }
    Ok(release)
}

fn validate_release(release: &Release) -> Result<()> {
    ensure!(
        release.id > 0 && !release.draft && release.assets.len() <= 100,
        "GitHub release is not a complete published update."
    );
    let body = release.body.as_deref().unwrap_or("");
    ensure!(
        !body.contains("<!-- galaxy-nightly-transition:"),
        "The Nightly update is still publishing. Try again shortly."
    );
    if track()? == "nightly" {
        ensure!(
            release.tag_name == "nightly"
                && release.name.as_deref() == Some("Nightly")
                && release.prerelease,
            "The release does not belong to the Nightly channel."
        );
        ensure!(
            body.matches("<!-- galaxy-nightly:").count() == 1
                && NIGHTLY_MARKER.captures_iter(body).count() == 1,
            "Nightly source identity is invalid."
        );
    } else {
        ensure!(
            !release.prerelease && VERSION.is_match(&release.tag_name),
            "The release is not a stable version tag."
        );
        ensure!(
            body.matches("<!-- galaxy-release:").count() == 1
                && STABLE_MARKER.captures_iter(body).count() == 1,
            "Stable source identity is invalid."
        );
    }
    ensure!(
        body.matches("<!-- galaxy-update:").count() == 1
            && UPDATE_MARKER.captures_iter(body).count() == 1,
        "This release does not provide signed Galaxy updates."
    );
    Ok(())
}

fn asset<'a>(release: &'a Release, name: &str) -> Result<&'a Asset> {
    let matches: Vec<_> = release
        .assets
        .iter()
        .filter(|asset| asset.name == name)
        .collect();
    ensure!(
        matches.len() == 1,
        "Release download is missing or ambiguous."
    );
    let asset = matches[0];
    ensure!(
        asset.id > 0 && asset.state == "uploaded" && asset.size > 0 && asset.size <= MAX_PAYLOAD,
        "Release download metadata is invalid."
    );
    let expected = format!("{RELEASES}/download/{}/{}", release.tag_name, name);
    ensure!(
        asset.browser_download_url == expected,
        "Release download is outside the expected repository."
    );
    Ok(asset)
}

fn verify_signature(manifest: &[u8], signature: &[u8]) -> Result<()> {
    ensure!(
        !manifest.is_empty() && manifest.len() <= MAX_MANIFEST && signature.len() == 64,
        "Signed update metadata has invalid size."
    );
    let key = hex_bytes(PUBLIC_KEY.trim())?;
    ensure!(
        key.len() == 32,
        "Galaxy's update verification key is invalid."
    );
    #[cfg(not(target_family = "wasm"))]
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
        .verify(manifest, signature)
        .map_err(|_| anyhow!("Galaxy update signature verification failed."))?;
    #[cfg(target_family = "wasm")]
    bail!("Desktop updates are unavailable in this environment.");
    Ok(())
}

fn hex_bytes(value: &str) -> Result<Vec<u8>> {
    ensure!(
        value.len() % 2 == 0 && value.is_ascii(),
        "Invalid update digest encoding."
    );
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair)?;
            u8::from_str_radix(text, 16).map_err(Into::into)
        })
        .collect()
}

fn version_tuple(version: &str) -> Result<(u64, u64, u64)> {
    let parts = VERSION
        .captures(version)
        .context("Invalid stable update version")?;
    Ok((parts[1].parse()?, parts[2].parse()?, parts[3].parse()?))
}

fn build_tuple(build: &str) -> Result<(u64, u64)> {
    let parts = BUILD
        .captures(build)
        .context("Invalid Nightly build identity")?;
    Ok((parts[2].parse()?, parts[3].parse()?))
}

fn platform() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("macos-aarch64"),
        ("windows", "x86_64") => Ok("windows-x86_64"),
        ("linux", "x86_64") => Ok("linux-x86_64"),
        _ => bail!("No Galaxy update package is available for this platform."),
    }
}

fn validate_manifest(manifest: &Manifest) -> Result<()> {
    ensure!(
        manifest.schema == 1
            && manifest.channel == track()?
            && SOURCE.is_match(&manifest.source_commit),
        "Signed update identity is invalid."
    );
    version_tuple(&manifest.version)?;
    let built = chrono::DateTime::parse_from_rfc3339(&manifest.built_at)
        .context("Invalid signed build time")?;
    ensure!(
        built.timestamp() <= clock() + 86400,
        "Signed build time is in the future."
    );
    if manifest.channel == "nightly" {
        let build = manifest
            .build_id
            .as_deref()
            .context("Nightly update has no build identity")?;
        let parts = BUILD
            .captures(build)
            .context("Invalid signed Nightly build identity")?;
        let label = IDENTITY
            .captures(&manifest.artifact_identity)
            .context("Invalid readable Nightly identity")?;
        ensure!(
            parts[1] == manifest.source_commit && parts[3] == label[4],
            "Nightly source or prepared attempt differs from its signed identity."
        );
        chrono::NaiveDateTime::parse_from_str(
            &format!("{}{}", &label[1], &label[2]),
            "%m%d%Y%H%M%S",
        )
        .context("Invalid readable Nightly build date")?;
        build_tuple(build)?;
    } else {
        ensure!(
            manifest.build_id.is_none() && manifest.artifact_identity == manifest.version,
            "Stable update identity is invalid."
        );
    }
    let suffixes = [
        ("macos-aarch64", "aarch64.dmg"),
        ("windows-x86_64", "x86_64-setup.exe"),
        ("linux-x86_64", "amd64.deb"),
        ("linux-portable-x86_64", "linux_x86_64.tar.gz"),
    ];
    ensure!(
        manifest.assets.len() == suffixes.len(),
        "Signed update manifest has an unexpected platform set."
    );
    for (platform, suffix) in suffixes {
        let asset = manifest
            .assets
            .get(platform)
            .context("Signed platform package is missing")?;
        ensure!(
            asset.name == format!("Galaxy_{}_{}", manifest.artifact_identity, suffix)
                && asset.size > 0
                && asset.size <= MAX_PAYLOAD
                && HASH.is_match(&asset.sha256),
            "Signed package metadata is invalid."
        );
    }
    Ok(())
}

fn validate_update(update: &GalaxyUpdate) -> Result<Manifest> {
    verify_signature(&update.manifest, &update.signature)?;
    let manifest: Manifest = serde_json::from_slice(&update.manifest)
        .context("Invalid signed Galaxy update manifest")?;
    validate_manifest(&manifest)?;
    let selected = manifest
        .assets
        .get(platform()?)
        .context("Signed platform package is missing")?;
    let tag = if manifest.channel == "nightly" {
        "nightly"
    } else {
        &manifest.version
    };
    ensure!(
        update.release_id > 0
            && update.asset_id > 0
            && update.channel == manifest.channel
            && update.tag == tag
            && update.source_commit == manifest.source_commit
            && update.build_id == manifest.build_id
            && update.artifact_identity == manifest.artifact_identity
            && update.asset_name == selected.name
            && update.asset_size == selected.size
            && update.asset_sha256 == selected.sha256
            && update.asset_url == format!("{RELEASES}/download/{tag}/{}", selected.name),
        "Cached update differs from its signed manifest."
    );
    Ok(manifest)
}

fn bind_release(release: &Release, manifest: &Manifest) -> Result<()> {
    validate_release(release)?;
    let body = release.body.as_deref().unwrap_or("");
    ensure!(
        release.target_commitish == manifest.source_commit,
        "Release source differs from the signed update."
    );
    if manifest.channel == "nightly" {
        let marker = NIGHTLY_MARKER
            .captures(body)
            .context("Missing Nightly source marker")?;
        ensure!(
            marker[1] == manifest.source_commit && Some(&marker[2]) == manifest.build_id.as_deref(),
            "Nightly marker differs from signed metadata."
        );
        ensure!(
            body.matches("<!-- galaxy-nightly-assets:").count() == 1
                && ASSET_MARKER.captures_iter(body).count() == 1,
            "Nightly filename identity is ambiguous."
        );
        let names = ASSET_MARKER.captures(body).unwrap();
        ensure!(
            Some(&names[1]) == manifest.build_id.as_deref()
                && names[2] == manifest.artifact_identity,
            "Nightly names differ from their signed identity."
        );
    } else {
        let marker = STABLE_MARKER
            .captures(body)
            .context("Missing stable source marker")?;
        ensure!(
            marker[1] == manifest.version
                && marker[2] == manifest.source_commit
                && release.tag_name == manifest.version,
            "Stable release differs from signed metadata."
        );
    }
    let marker = UPDATE_MARKER
        .captures(body)
        .context("Missing signed update link")?;
    ensure!(
        marker[1] == format!("Galaxy_Update_{}.json", manifest.artifact_identity)
            && marker[2] == format!("Galaxy_Update_{}.sig", manifest.artifact_identity),
        "Signed update links have another build identity."
    );
    for signed in manifest.assets.values() {
        let actual = asset(release, &signed.name)?;
        ensure!(
            actual.size == signed.size,
            "Release package size differs from its signed manifest."
        );
        if let Some(digest) = &actual.digest {
            ensure!(
                *digest == format!("sha256:{}", signed.sha256),
                "Release package hash differs from its signed manifest."
            );
        }
    }
    Ok(())
}

fn bind_download(release: &Release, manifest: &Manifest, update: &GalaxyUpdate) -> Result<()> {
    let result = (|| {
        bind_release(release, manifest)?;
        let actual = asset(release, &update.asset_name)?;
        ensure!(
            release.id == update.release_id && actual.id == update.asset_id,
            "The release changed; check for updates again."
        );
        Ok(())
    })();
    if result.is_err() {
        invalidate_discovery();
    }
    result
}

async fn fetch_from_github(client: &Client) -> Result<VersionInfo> {
    let release = current_release(client).await?;
    let body = release.body.as_deref().unwrap_or("");
    let marker = UPDATE_MARKER
        .captures(body)
        .context("Signed update links are unavailable")?;
    let json = asset(&release, &marker[1])?;
    let sig = asset(&release, &marker[2])?;
    ensure!(
        json.size <= MAX_MANIFEST as u64 && sig.size == 64,
        "Signed metadata size is invalid."
    );
    let manifest_bytes = bounded_bytes(
        request(client, &json.browser_download_url).await?,
        MAX_MANIFEST,
    )
    .await?;
    let signature = bounded_bytes(request(client, &sig.browser_download_url).await?, 64).await?;
    ensure!(
        manifest_bytes.len() as u64 == json.size && signature.len() as u64 == sig.size,
        "Signed update download size changed."
    );
    verify_signature(&manifest_bytes, &signature)?;
    let manifest: Manifest =
        serde_json::from_slice(&manifest_bytes).context("Invalid signed Galaxy manifest")?;
    validate_manifest(&manifest)?;
    bind_release(&release, &manifest)?;
    let selected = manifest
        .assets
        .get(platform()?)
        .context("Update package is missing")?
        .clone();
    let selected_asset = asset(&release, &selected.name)?.clone();
    let mut version_info = VersionInfo::new(manifest.version.clone());
    version_info.galaxy_update = Some(GalaxyUpdate {
        channel: manifest.channel,
        tag: release.tag_name,
        release_id: release.id,
        source_commit: manifest.source_commit,
        build_id: manifest.build_id,
        artifact_identity: manifest.artifact_identity,
        manifest: manifest_bytes,
        signature,
        asset_id: selected_asset.id,
        asset_name: selected.name.clone(),
        asset_size: selected.size,
        asset_sha256: selected.sha256.clone(),
        asset_url: selected_asset.browser_download_url.clone(),
    });
    Ok(version_info)
}

pub async fn fetch_version(manual: bool) -> Result<VersionInfo> {
    ensure!(
        is_galaxy_updater(),
        "Automatic updates are available in installed Galaxy release builds."
    );
    let _lock = CHECK_LOCK.lock().await;
    let now = clock();
    let mut cache = read_cache();
    if cache.checked_at > now + 300 || cache.attempted_at > now + 300 {
        cache.checked_at = 0;
        cache.attempted_at = 0;
        cache.version = None;
    }
    cache.blocked_until = cache.blocked_until.min(now + 86400);
    ensure!(
        cache.blocked_until <= now,
        "GitHub updates are temporarily rate limited. Try again after the cooldown."
    );
    if !manual && now.saturating_sub(cache.checked_at) < NORMAL_INTERVAL {
        if let Some(version) = cache.version.as_ref() {
            if validate_version(version).is_ok() {
                return Ok(version.clone());
            }
        }
    }
    ensure!(
        now.saturating_sub(cache.attempted_at) >= MANUAL_INTERVAL,
        "Galaxy checked recently. Try again in a few minutes."
    );
    cache.attempted_at = now;
    write_cache(&cache)?;
    let result = fetch_from_github(&client()?).await;
    if let Ok(version) = &result {
        cache.checked_at = now;
        cache.version = Some(version.clone());
        cache.blocked_until = 0;
        write_cache(&cache)?;
    }
    result
}

pub fn should_update(version: &VersionInfo) -> Result<bool> {
    let manifest = validate_version(version)?;
    if manifest.channel == "release" {
        return Ok(version_tuple(&manifest.version)? > version_tuple(env!("CARGO_PKG_VERSION"))?);
    }
    let available = build_tuple(
        manifest
            .build_id
            .as_deref()
            .context("Nightly build identity is missing")?,
    )?;
    let current = build_info::nightly_id().context(
        "This Nightly has no installed build identity; install a current Nightly manually",
    )?;
    Ok(available > build_tuple(current)?)
}

fn validate_version(version: &VersionInfo) -> Result<Manifest> {
    let update = version
        .galaxy_update
        .as_ref()
        .context("Galaxy update identity is unavailable")?;
    let manifest = validate_update(update)?;
    ensure!(
        version.version == manifest.version,
        "Update version differs from Galaxy's signed manifest."
    );
    Ok(manifest)
}

fn file_digest(path: &Path) -> Result<(u64, String)> {
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= MAX_PAYLOAD,
        "Downloaded update file is invalid."
    );
    let mut input = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut count = 0_u64;
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let bytes = input.read(&mut buffer)?;
        if bytes == 0 {
            break;
        }
        count += bytes as u64;
        ensure!(
            count <= MAX_PAYLOAD,
            "Downloaded update exceeds its size limit."
        );
        digest.update(&buffer[..bytes]);
    }
    Ok((count, format!("{:x}", digest.finalize())))
}

pub fn verify_download(version: &VersionInfo, path: &Path) -> Result<()> {
    let update = version
        .galaxy_update
        .as_ref()
        .context("Galaxy update identity is missing")?;
    validate_version(version)?;
    let (size, digest) = file_digest(path)?;
    ensure!(
        size == update.asset_size && digest == update.asset_sha256,
        "Downloaded update does not match Galaxy's signed checksum."
    );
    Ok(())
}

pub async fn download_verified(version: &VersionInfo, path: &Path) -> Result<()> {
    ensure!(
        is_galaxy_updater(),
        "This build cannot install Galaxy updates."
    );
    let update = version
        .galaxy_update
        .as_ref()
        .context("Galaxy update identity is missing")?;
    let manifest = validate_version(version)?;
    let client = client()?;
    let release = current_release(&client).await?;
    bind_download(&release, &manifest, update)?;
    let parent = path
        .parent()
        .context("Update download has no parent directory")?;
    let parent_metadata = std::fs::symlink_metadata(parent)?;
    ensure!(
        parent_metadata.is_dir() && !parent_metadata.file_type().is_symlink(),
        "Update download directory is invalid."
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            parent_metadata.permissions().mode() & 0o077 == 0,
            "Update download directory must be private."
        );
    }
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "Update download destination is invalid."
        );
    }
    // Write to a new private sibling, then replace the destination only after verification.
    let temporary = tempfile::NamedTempFile::new_in(parent)?;
    let mut output = async_fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(temporary.path())
        .await?;
    let response = request(&client, &update.asset_url).await?;
    let stream = response.bytes_stream();
    futures::pin_mut!(stream);
    let mut count = 0_u64;
    let mut digest = Sha256::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        count = count
            .checked_add(chunk.len() as u64)
            .context("Update size overflow")?;
        ensure!(
            count <= update.asset_size && count <= MAX_PAYLOAD,
            "Update download exceeded its signed size."
        );
        digest.update(&chunk);
        output.write_all(&chunk).await?;
    }
    output.sync_all().await?;
    drop(output);
    ensure!(
        count == update.asset_size && format!("{:x}", digest.finalize()) == update.asset_sha256,
        "Galaxy update checksum verification failed."
    );
    let latest = current_release(&client).await?;
    bind_download(&latest, &manifest, update)?;
    temporary.persist(path)?;
    verify_download(version, path)?;
    Ok(())
}

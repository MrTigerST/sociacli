// Self-update: queries the GitHub Releases of `sociacli/sociacli`, picks the
// asset whose name contains the current build's target triple, downloads it,
// and atomically replaces the running binary.
//
// Release artifacts must match `sociacli-<target>.zip` (Windows) or
// `sociacli-<target>.tar.gz` (Linux/macOS). The release workflow uploads
// these alongside the installer-flavoured artifacts (.exe, .dmg, .deb).

use anyhow::Result;
use self_update::{cargo_crate_version, Status};

// GitHub repo holding the release artifacts. Override with env at build
// time (`SOCIACLI_UPDATE_REPO="owner/name"`) when forking.
pub const REPO_OWNER: &str = match option_env!("SOCIACLI_UPDATE_OWNER") {
    Some(v) => v,
    None => "MrTigerST",
};
pub const REPO_NAME: &str = match option_env!("SOCIACLI_UPDATE_REPO") {
    Some(v) => v,
    None => "sociacli",
};
pub const BIN_NAME: &str = "sociacli";

/// Synchronous — call from `tokio::task::spawn_blocking`.
pub fn apply_latest() -> Result<Status> {
    let status = self_update::backends::github::Update::configure()
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .bin_name(BIN_NAME)
        .current_version(cargo_crate_version!())
        .show_download_progress(true)
        .show_output(false)
        .no_confirm(true)
        .build()?
        .update()?;
    Ok(status)
}

/// Returns `Some(version)` if a newer release exists, else `None`.
/// Synchronous — call from `tokio::task::spawn_blocking`.
pub fn latest_if_newer() -> Result<Option<String>> {
    let releases = self_update::backends::github::ReleaseList::configure()
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .build()?
        .fetch()?;
    let current = cargo_crate_version!();
    for r in releases {
        if self_update::version::bump_is_greater(current, &r.version).unwrap_or(false) {
            return Ok(Some(r.version));
        }
    }
    Ok(None)
}

pub fn current_version() -> &'static str {
    cargo_crate_version!()
}

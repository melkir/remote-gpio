pub const GIT_SHA: &str = env!("VERGEN_GIT_SHA");
pub const BUILD_DATE: &str = env!("VERGEN_BUILD_DATE");
pub const CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const GITHUB_REPO: &str = "melkir/remote-gpio";

pub fn short_sha() -> &'static str {
    let sha = GIT_SHA;
    if sha.len() >= 7 {
        &sha[..7]
    } else {
        sha
    }
}

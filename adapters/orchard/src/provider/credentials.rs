//! Where the adapter gets its controller credentials.
//!
//! Not from `config` or `input`. The protocol is explicit that secret values
//! must not appear there, and a manifest is committed to git, so the token has
//! to reach the adapter by another route entirely.
//!
//! The controller is unauthenticated only while it has no service accounts. On
//! first start it mints a random `bootstrap-admin` account, after which every
//! API call answers 401 -- so an adapter with no credentials stops working the
//! moment the controller stops being wide open. Measured: HTTP Basic with the
//! service account name and token answers 200.
//!
//! The file format is two lines -- name, then token -- deliberately, rather
//! than reading orchard's own YAML. A generated token can contain `": "`, and a
//! line-oriented YAML reader would silently truncate it; a real YAML parser
//! would be a new dependency for six lines of config. Here the token is
//! everything after the first newline, so no character in it can be
//! misinterpreted.
use std::path::PathBuf;

/// Environment variable naming a credentials file.
const PATH_VAR: &str = "WORKENV_ORCHARD_CREDENTIALS";
/// Environment variables carrying credentials directly, for CI.
const NAME_VAR: &str = "WORKENV_ORCHARD_ACCOUNT";
const TOKEN_VAR: &str = "WORKENV_ORCHARD_TOKEN";

/// A service account the controller will accept.
pub(super) struct Credentials {
    pub(super) account: String,
    pub(super) token: String,
}

/// Read credentials, or nothing when the controller needs none.
///
/// Absence is not an error: a controller with no service accounts accepts
/// unauthenticated calls, and that is a legitimate first-run state.
pub(super) fn load() -> Option<Credentials> {
    if let (Ok(account), Ok(token)) = (std::env::var(NAME_VAR), std::env::var(TOKEN_VAR)) {
        return credentials(&account, &token);
    }
    let contents = std::fs::read_to_string(path()?).ok()?;
    parse(&contents)
}

/// Split the two-line credentials format.
pub(super) fn parse(contents: &str) -> Option<Credentials> {
    let (account, token) = contents.split_once('\n')?;
    credentials(account, token)
}

/// Build credentials, refusing a half-populated pair.
///
/// A blank account with a real token would send `Authorization: Basic` for the
/// empty user and read as a wrong password rather than as missing config.
fn credentials(account: &str, token: &str) -> Option<Credentials> {
    let account = account.trim();
    // Only the trailing newline is stripped: a token may legitimately contain
    // spaces, and trimming them would send a different secret than was stored.
    let token = token.strip_suffix('\n').unwrap_or(token);
    (!account.is_empty() && !token.is_empty()).then(|| Credentials {
        account: account.to_owned(),
        token: token.to_owned(),
    })
}

/// The credentials file, from the environment or the default location.
fn path() -> Option<PathBuf> {
    if let Ok(configured) = std::env::var(PATH_VAR) {
        return Some(PathBuf::from(configured));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".orchard/workenv-credentials"))
}

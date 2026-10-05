//! Configuration, layered: env vars beat the TOML config file, which beats the compiled-in default
//! ([`presets/redhat.toml`](../../presets/redhat.toml)).
//!
//! The token is resolved separately and never has to appear in any of them — `token_file` (default
//! `~/.jiratoken`) keeps the secret out of config that gets backed up, synced, or pasted into an
//! issue. A jira-cli config, if you have one, supplies the site and login for free.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The shipped defaults, compiled in so a zero-config install still knows the custom-field map.
const DEFAULTS: &str = include_str!("../presets/redhat.toml");
/// Config path under `$XDG_CONFIG_HOME` (or `~/.config`).
const CONFIG_PATH: &str = "ujira/config.toml";
/// The pre-rename config path, honored when only it exists.
const LEGACY_CONFIG_PATH: &str = "jira-mcp/config.toml";
/// The jira-cli config we can borrow `server:` / `login:` from.
const CLI_CONFIG: &str = ".config/.jira/.config.yml";

/// The TOML file's shape. Every field optional: a partial config layers over the defaults.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    url: Option<String>,
    username: Option<String>,
    /// An inline token. Works, but prefer `token_file`.
    token: Option<String>,
    token_file: Option<String>,
    /// Look the token up in the OS credential store. Default true.
    keychain: Option<bool>,
    /// `read-only`, `read-comment`, or `read-write`. Default read-write.
    access: Option<Access>,
    /// v0.1's boolean, still honored so an older config doesn't fail to parse on upgrade.
    /// `deny_unknown_fields` means dropping it outright would be a hard error, not a shrug.
    read_only: Option<bool>,
    /// friendly name -> `customfield_NNNNN`. Replaces the default map wholesale when present.
    custom_fields: Option<BTreeMap<String, String>>,
}

/// How much authority the tools have. A bool can't express the middle case, and the middle case is
/// the interesting one: an embedding host that wants an agent to READ requirements and leave a
/// comment, but never create, edit, transition, or link anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Access {
    /// Reads only. Every mutating tool refuses.
    ReadOnly,
    /// Reads and comments. `jira_add_comment` works; create/update/transition/link refuse.
    ReadComment,
    /// Everything.
    ReadWrite,
}

impl Access {
    /// Does this level grant what `need` requires? Levels are ordered, so this is a `>=`.
    pub fn allows(self, need: Access) -> bool {
        self.rank() >= need.rank()
    }

    fn rank(self) -> u8 {
        match self {
            Self::ReadOnly => 0,
            Self::ReadComment => 1,
            Self::ReadWrite => 2,
        }
    }
}

impl std::fmt::Display for Access {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ReadOnly => "read-only",
            Self::ReadComment => "read-comment",
            Self::ReadWrite => "read-write",
        })
    }
}

/// Where the API token was found, in the order they're tried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenSource {
    /// `JIRA_API_TOKEN`.
    Env,
    /// The OS credential store, under the account email.
    Keychain,
    /// A file on disk (`token_file`).
    File(PathBuf),
    /// `token` in the config file.
    Inline,
}

impl std::fmt::Display for TokenSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Env => write!(f, "JIRA_API_TOKEN"),
            Self::Keychain => write!(f, "OS keychain"),
            Self::File(p) => write!(f, "{}", p.display()),
            Self::Inline => write!(f, "config file (inline)"),
        }
    }
}

/// Everything the server needs to talk to one JIRA Cloud site.
///
/// `Debug` is hand-written to redact the token: this struct is one careless `{:?}` away from
/// printing a live API credential into a log.
#[derive(Clone)]
pub struct Config {
    /// Site base url, no trailing slash (e.g. `https://your-site.atlassian.net`).
    pub base: String,
    /// Atlassian account email (the basic-auth user).
    pub email: String,
    /// Cloud API token.
    pub token: String,
    /// Where that token came from — reported by `--check`, because "which of the four places is it
    /// reading?" is the first question when an install misbehaves.
    pub token_source: TokenSource,
    /// How much the tools may do. Enforced in the client, under every tool.
    pub access: Access,
    /// Custom fields surfaced by `jira_get_issue`, friendly name -> field id.
    pub custom_fields: Vec<(String, String)>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("base", &self.base)
            .field("email", &self.email)
            .field("token", &"<redacted>")
            .field("token_source", &self.token_source)
            .field("access", &self.access)
            .field("custom_fields", &self.custom_fields.len())
            .finish()
    }
}

impl Config {
    /// Strict env-only resolution: `JIRA_URL` + the account (`JIRA_USERNAME`, else `JIRA_EMAIL`) +
    /// `JIRA_API_TOKEN`, or `None`.
    ///
    /// For an embedding host that injects credentials itself and wants "JIRA is off" to be a normal
    /// state (report `disabled`) rather than a startup failure. It reads no files: it must not
    /// wander off to the operator's home directory looking for a token.
    pub fn from_env() -> Option<Self> {
        Some(Self {
            base: env("JIRA_URL")?.trim_end_matches('/').to_string(),
            email: env_account()?,
            token: env("JIRA_API_TOKEN")?,
            token_source: TokenSource::Env,
            access: env_access().unwrap_or(Access::ReadWrite),
            custom_fields: defaults().custom_fields.map(pairs).unwrap_or_default(),
        })
    }

    /// The full layered resolution used by the binary: env > config file > compiled-in defaults,
    /// with the jira-cli config as a last resort for the site and login.
    pub fn load() -> Result<Self> {
        tracing::debug!(path = ?config_path(), "loading config");
        Self::resolve(&file_config()?, &defaults())
    }

    /// The layering itself, with both sides injected so it can be tested without depending on the
    /// developer's home directory.
    fn resolve(file: &File, defaults: &File) -> Result<Self> {
        let base = env("JIRA_URL")
            .or_else(|| file.url.clone())
            .or_else(|| cli_config_value("server"))
            .context("no JIRA site url: set JIRA_URL or `url` in the config file")?
            .trim_end_matches('/')
            .to_string();
        let email = env_account()
            .or_else(|| file.username.clone())
            .or_else(|| cli_config_value("login"))
            .context("no JIRA account email: set JIRA_USERNAME (or JIRA_EMAIL) or `username` in the config file")?;
        let (token, token_source) = resolve_token(file, defaults, &email)?;
        tracing::debug!(site = %base, account = %email, token = %token_source, "config resolved");
        let custom_fields = file
            .custom_fields
            .clone()
            .or_else(|| defaults.custom_fields.clone())
            .map(pairs)
            .unwrap_or_default();
        Ok(Self {
            base,
            email,
            token,
            token_source,
            access: env_access()
                .or(file.access)
                .or_else(|| legacy_read_only(file))
                .or(defaults.access)
                .unwrap_or(Access::ReadWrite),
            custom_fields,
        })
    }
}

/// `JIRA_API_TOKEN`, else the OS keychain, else `token_file`, else an inline `token`.
///
/// Ordered most-explicit to least: an env var is someone overriding on purpose, the keychain is the
/// managed store, and the file and inline forms are the plain-text fallbacks.
fn resolve_token(file: &File, defaults: &File, account: &str) -> Result<(String, TokenSource)> {
    if let Some(t) = env("JIRA_API_TOKEN") {
        return Ok((t, TokenSource::Env));
    }
    let use_keychain = flag("UJIRA_KEYCHAIN")
        .or_else(|| flag("JIRA_MCP_KEYCHAIN"))
        .or(file.keychain)
        .or(defaults.keychain)
        .unwrap_or(true);
    if use_keychain {
        #[cfg(feature = "keychain")]
        match crate::keychain::get(account) {
            Some(t) if !t.trim().is_empty() => return Ok((t, TokenSource::Keychain)),
            Some(_) => tracing::debug!(account, "keychain entry is empty, trying the next source"),
            None => tracing::debug!(account, "no keychain entry, trying the next source"),
        }
        #[cfg(not(feature = "keychain"))]
        tracing::debug!(
            account,
            "built without the keychain feature, trying the next source"
        );
    }
    let path = env("JIRA_API_TOKEN_FILE")
        .or_else(|| file.token_file.clone())
        .or_else(|| defaults.token_file.clone());
    let mut file_err = None;
    if let Some(path) = path {
        let path = expand_home(&path);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let token = non_empty(text.trim(), &format!("token file {}", path.display()))?;
                return Ok((token, TokenSource::File(path)));
            }
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "token file unreadable, trying the inline token");
                file_err = Some((path, e));
            }
        }
    }
    if let Some(t) = &file.token {
        return Ok((
            non_empty(t, "`token` in the config file")?,
            TokenSource::Inline,
        ));
    }
    match file_err {
        Some((path, e)) => Err(e).with_context(|| {
            format!(
                "no JIRA API token: run `ujira set-token`, set JIRA_API_TOKEN, or write one to {}",
                path.display()
            )
        }),
        None => {
            bail!("no JIRA API token: run `ujira set-token`, or set JIRA_API_TOKEN / `token_file`")
        }
    }
}

fn non_empty(s: &str, what: &str) -> Result<String> {
    if s.is_empty() {
        bail!("{what} is empty");
    }
    Ok(s.to_string())
}

/// The compiled-in defaults. A parse failure here is a build-time mistake in `presets/redhat.toml`,
/// so it degrades to "no defaults" rather than taking the server down.
fn defaults() -> File {
    toml::from_str(DEFAULTS).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "built-in defaults are malformed; continuing without them");
        File::default()
    })
}

/// Read the user's config file, if there is one. A malformed file IS fatal: silently ignoring it
/// would answer with someone else's field mapping and no explanation.
fn file_config() -> Result<File> {
    let Some(path) = config_path() else {
        return Ok(File::default());
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(File::default());
    };
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// `$UJIRA_CONFIG` (or the pre-rename `$JIRA_MCP_CONFIG`), else `ujira/config.toml` under
/// `$XDG_CONFIG_HOME` or `~/.config` — falling back to the pre-rename `jira-mcp/config.toml`
/// when only that one exists.
pub fn config_path() -> Option<PathBuf> {
    if let Some(p) = env("UJIRA_CONFIG").or_else(|| env("JIRA_MCP_CONFIG")) {
        return Some(expand_home(&p));
    }
    let config_dir = match env("XDG_CONFIG_HOME") {
        Some(dir) => expand_home(&dir),
        None => home()?.join(".config"),
    };
    let new = config_dir.join(CONFIG_PATH);
    if !new.exists() {
        let legacy = config_dir.join(LEGACY_CONFIG_PATH);
        if legacy.exists() {
            return Some(legacy);
        }
    }
    Some(new)
}

/// Expand a leading `~/`, which people write in config files and `PathBuf` does not understand.
fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/").zip(home()) {
        Some((rest, home)) => home.join(rest),
        None => PathBuf::from(path),
    }
}

/// Map -> ordered pairs. `BTreeMap` already sorts by name, which keeps rendered issues stable.
fn pairs(map: BTreeMap<String, String>) -> Vec<(String, String)> {
    map.into_iter().collect()
}

/// A non-empty env var.
/// The env vars naming the account email, most preferred first.
const ACCOUNT_VARS: [&str; 2] = ["JIRA_USERNAME", "JIRA_EMAIL"];

fn env_account() -> Option<String> {
    account_from(env)
}

/// The first of [`ACCOUNT_VARS`] that `lookup` finds.
fn account_from(lookup: impl Fn(&str) -> Option<String>) -> Option<String> {
    ACCOUNT_VARS.iter().find_map(|k| lookup(k))
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

/// Honor a v0.1 config's `read_only = true` as `access = "read-only"`, once, loudly. `read_only =
/// false` maps to nothing: it was the old default, so treating it as an explicit "read-write" would
/// let a stale file silently outrank a deliberate `access` in the shipped defaults.
fn legacy_read_only(file: &File) -> Option<Access> {
    match file.read_only? {
        true => {
            tracing::warn!("`read_only` is deprecated; use `access = \"read-only\"`");
            Some(Access::ReadOnly)
        }
        false => None,
    }
}

/// `UJIRA_ACCESS=read-only|read-comment|read-write` (or the pre-rename `JIRA_MCP_ACCESS`), or the
/// older boolean `JIRA_MCP_READ_ONLY=1`.
/// An unparseable value fails CLOSED (read-only): a typo in a lock-down knob must not unlock it.
fn env_access() -> Option<Access> {
    if let Some(v) = env("UJIRA_ACCESS").or_else(|| env("JIRA_MCP_ACCESS")) {
        return Some(match v.to_ascii_lowercase().replace('_', "-").as_str() {
            "read-write" => Access::ReadWrite,
            "read-comment" => Access::ReadComment,
            "read-only" => Access::ReadOnly,
            other => {
                tracing::warn!(
                    access = other,
                    "unknown access level; falling back to read-only"
                );
                Access::ReadOnly
            }
        });
    }
    match flag("JIRA_MCP_READ_ONLY")? {
        true => Some(Access::ReadOnly),
        false => None,
    }
}

/// A boolean env var: `1`/`true`/`yes` is on, anything else set is off.
fn flag(key: &str) -> Option<bool> {
    let v = env(key)?.to_ascii_lowercase();
    Some(v == "1" || v == "true" || v == "yes")
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Scrape one top-level `key: value` out of a [jira-cli](https://github.com/ankitpokhrel/jira-cli)
/// config. Deliberately not a YAML parser: we want exactly two scalars, and the maintained-YAML-crate
/// situation is not worth a dependency for that.
fn cli_config_value(key: &str) -> Option<String> {
    let path: PathBuf = match env("JIRA_CLI_CONFIG") {
        Some(p) => expand_home(&p),
        None => home()?.join(CLI_CONFIG),
    };
    let text = std::fs::read_to_string(path).ok()?;
    scrape(&text, key)
}

fn scrape(text: &str, key: &str) -> Option<String> {
    text.lines()
        .filter_map(|line| line.strip_prefix(key)?.strip_prefix(':'))
        .map(|v| v.trim().trim_matches('"').to_string())
        .find(|v| !v.is_empty())
}

/// Resolve just the account email, without needing a token.
///
/// The token-management commands need this: you can't require a working config to store the
/// credential the config is missing.
pub fn account() -> Result<String> {
    env_account()
        .or_else(|| file_config().ok().and_then(|f| f.username))
        .or_else(|| cli_config_value("login"))
        .context("no JIRA account email: set JIRA_USERNAME (or JIRA_EMAIL) or `username` in the config file")
}

/// The shipped config template, verbatim — what `--write-config` drops on disk.
pub fn default_config_toml() -> &'static str {
    DEFAULTS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped preset must parse, and must actually carry the field map — this file is the
    /// product's only source of custom-field truth, and `deny_unknown_fields` means a typo'd key
    /// fails here rather than silently doing nothing at runtime.
    #[test]
    fn shipped_defaults_parse() {
        let d: File = toml::from_str(DEFAULTS).expect("presets/redhat.toml must parse");
        let fields = d.custom_fields.expect("a custom_fields table");
        assert_eq!(
            fields.get("team").map(String::as_str),
            Some("customfield_10001")
        );
        assert!(fields.len() > 10, "the curated map should be generous");
        assert_eq!(d.token_file.as_deref(), Some("~/.jiratoken"));
    }

    /// A user's config replaces the default map wholesale — no merging. Half your ids and half Red
    /// Hat's would be worse than either.
    #[test]
    fn user_custom_fields_replace_defaults() {
        let file: File = toml::from_str(
            r#"
            url = "https://mine.atlassian.net"
            username = "me@mine.com"
            token = "abc"
            [custom_fields]
            squad = "customfield_1"
        "#,
        )
        .expect("parses");
        let cfg = Config::resolve(&file, &File::default()).expect("resolves");
        assert_eq!(cfg.base, "https://mine.atlassian.net");
        assert_eq!(
            cfg.custom_fields,
            vec![("squad".to_string(), "customfield_1".to_string())]
        );
    }

    /// An unknown key is a typo, and a typo that silently does nothing is a bad afternoon.
    #[test]
    fn unknown_keys_are_rejected() {
        let err = toml::from_str::<File>("urll = \"https://x\"").expect_err("must reject");
        assert!(err.to_string().contains("unknown field"), "{err}");
    }

    #[test]
    fn home_expands_only_at_the_front() {
        let home = home().expect("a HOME");
        assert_eq!(expand_home("~/.jiratoken"), home.join(".jiratoken"));
        assert_eq!(expand_home("/etc/tok"), PathBuf::from("/etc/tok"));
        assert_eq!(expand_home("a/~/b"), PathBuf::from("a/~/b"));
    }

    /// The ordering is the whole contract: read-comment must permit a comment and refuse a create,
    /// or an embedding host's "read + comment" promise is decoration.
    #[test]
    fn access_levels_are_ordered() {
        assert!(Access::ReadComment.allows(Access::ReadComment));
        assert!(Access::ReadComment.allows(Access::ReadOnly));
        assert!(!Access::ReadComment.allows(Access::ReadWrite));
        assert!(!Access::ReadOnly.allows(Access::ReadComment));
        assert!(Access::ReadWrite.allows(Access::ReadWrite));
    }

    #[test]
    fn access_parses_from_config() {
        let f: File = toml::from_str(r#"access = "read-comment""#).expect("parses");
        assert_eq!(f.access, Some(Access::ReadComment));
        assert!(
            toml::from_str::<File>(r#"access = "read-everything""#).is_err(),
            "an unknown level must fail loudly, not default to something permissive"
        );
    }

    #[test]
    fn cli_config_scrape_finds_scalars() {
        let yaml = "installation: cloud\nserver: https://x.atlassian.net\nlogin: me@x.com\n";
        assert_eq!(
            scrape(yaml, "server").as_deref(),
            Some("https://x.atlassian.net")
        );
        assert_eq!(scrape(yaml, "login").as_deref(), Some("me@x.com"));
        assert_eq!(scrape(yaml, "nope"), None);
    }

    #[test]
    fn account_prefers_jira_username_over_jira_email() {
        let vars = |set: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                set.iter()
                    .find(|(name, _)| *name == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(account_from(vars(&[])), None);
        assert_eq!(
            account_from(vars(&[("JIRA_EMAIL", "e@x.com")])),
            Some("e@x.com".into())
        );
        assert_eq!(
            account_from(vars(&[("JIRA_USERNAME", "u@x.com")])),
            Some("u@x.com".into())
        );
        assert_eq!(
            account_from(vars(&[
                ("JIRA_EMAIL", "e@x.com"),
                ("JIRA_USERNAME", "u@x.com")
            ])),
            Some("u@x.com".into())
        );
    }
}

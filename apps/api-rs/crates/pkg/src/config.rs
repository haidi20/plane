// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Environment parity with Django.
//!
//! Names, defaults, and parsing rules mirror `apps/api/plane/settings/`
//! (`common.py`, `production.py`, `storage.py`). A deployment switch must not
//! change behaviour during cutover, so the odd corners are copied on purpose:
//! Django compares most booleans with `== "1"` (never `true`/`yes`), parses
//! `DEBUG` with `int()`, and splits lists on a bare comma.
//!
//! Fail-fast rules that are Rust-only, documented in `docs/rust-api.md` 18.1:
//!
//! * `SECRET_KEY` is required. Django generates a random key when it is unset,
//!   which silently invalidates sessions, password-reset tokens, and invite
//!   links on every restart; refusing to start is the honest equivalent.
//! * `ENABLE_READ_REPLICA=1` is refused: Rust runs one read-write pool (7.5).
//!
//! Secrets never reach a log line: [`Secret`] redacts itself, and
//! [`Config::redacted`] is the only snapshot the binaries emit.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fmt;

/// Default settings module, matching `manage.py` and `plane/wsgi.py`.
pub const DEFAULT_SETTINGS_MODULE: &str = "plane.settings.production";

/// `TIME_ZONE` in `common.py`; a constant, not an environment variable.
pub const TIME_ZONE: &str = "UTC";

/// Errors raised while reading the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// A variable Rust requires is unset or blank.
    Missing(&'static str),
    /// A variable is set but does not parse the way Django parses it.
    Invalid(&'static str, &'static str),
    /// A documented Django feature this service does not implement.
    Unsupported(&'static str),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(name) => write!(f, "{name} is required but unset"),
            Self::Invalid(name, expected) => write!(f, "{name} must be {expected}"),
            Self::Unsupported(what) => write!(f, "unsupported: {what}"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// A value that must never be printed. `Debug` and `Display` both redact, so an
/// accidental `{value:?}` in a log statement cannot leak it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wrap the value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Read the value. Callers keep it out of logs.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// True when the environment still carries a placeholder or published key,
    /// mirroring `_INSECURE_SECRET_KEYS` in `common.py`.
    #[must_use]
    pub fn is_insecure(&self) -> bool {
        matches!(
            self.0.as_str(),
            "60gp0byfz2dvffa45cxl20p1scy9xbpf6d8c5y0geejgkyp1b5" | "change-this-key-on-deployment"
        )
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}
/// Where variables come from: the process, or a map in tests.
///
/// Injection keeps tests hermetic without `std::env::set_var`, which is unsafe
/// to call once another thread may read the environment.
pub trait EnvSource {
    /// Read one variable; blank values are kept (Django distinguishes them).
    fn get(&self, name: &str) -> Option<String>;
}

/// The real process environment.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn get(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

impl EnvSource for HashMap<String, String> {
    fn get(&self, name: &str) -> Option<String> {
        HashMap::get(self, name).cloned()
    }
}

impl EnvSource for BTreeMap<String, String> {
    fn get(&self, name: &str) -> Option<String> {
        BTreeMap::get(self, name).cloned()
    }
}

/// How a variable is treated when the snapshot is logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sensitivity {
    /// Safe to print.
    Plain,
    /// Masked in [`Config::redacted`].
    Secret,
}

/// One documented variable from `docs/rust-api.md` 18.1.
#[derive(Debug, Clone, Copy)]
pub struct EnvSpec {
    /// Variable name, unchanged from Django.
    pub name: &'static str,
    /// Django's default, or `None` when Django treats it as unset.
    pub default: Option<&'static str>,
    /// Whether the value is masked in logs.
    pub sensitivity: Sensitivity,
    /// Whether Rust refuses to start without it.
    pub required: bool,
}

const fn spec(name: &'static str, default: Option<&'static str>) -> EnvSpec {
    EnvSpec {
        name,
        default,
        sensitivity: Sensitivity::Plain,
        required: false,
    }
}

const fn secret(name: &'static str, required: bool) -> EnvSpec {
    EnvSpec {
        name,
        default: None,
        sensitivity: Sensitivity::Secret,
        required,
    }
}
/// Every variable from `docs/rust-api.md` 18.1, plus the ones this service reads.
///
/// This table is the single place a name or default may change; the typed
/// fields below are derived from it, and `Config::redacted` prints it.
pub const ENV_SPECS: &[EnvSpec] = &[
    // Database (7.5: one read-write pool, no replica)
    spec("DATABASE_URL", None),
    spec("POSTGRES_DB", None),
    spec("POSTGRES_USER", None),
    secret("POSTGRES_PASSWORD", false),
    spec("POSTGRES_HOST", None),
    spec("POSTGRES_PORT", Some("5432")),
    spec("ENABLE_READ_REPLICA", Some("0")),
    spec("DATABASE_READ_REPLICA_URL", None),
    // Redis
    spec("REDIS_URL", None),
    spec("REDIS_HOST", None),
    secret("REDIS_PASSWORD", false),
    spec("REDIS_PORT", None),
    spec("REDIS_SSL_CERT_REQS", None),
    spec("REDIS_SSL_CA_CERTS", None),
    spec("REDIS_QUEUE_DB", Some("1")),
    spec("REDIS_CACHE_DB", Some("2")),
    spec("REDIS_PREFIX", Some("plane-rs:")),
    // Queue (Celery stays authoritative during the transition)
    spec("RABBITMQ_HOST", Some("localhost")),
    spec("RABBITMQ_PORT", Some("5672")),
    spec("RABBITMQ_USER", Some("guest")),
    secret("RABBITMQ_PASSWORD", false),
    spec("RABBITMQ_VHOST", Some("/")),
    spec("AMQP_URL", None),
    // Storage
    secret("AWS_ACCESS_KEY_ID", false),
    secret("AWS_SECRET_ACCESS_KEY", false),
    spec("AWS_S3_BUCKET_NAME", Some("uploads")),
    spec("AWS_REGION", Some("")),
    spec("AWS_S3_ENDPOINT_URL", None),
    spec("USE_MINIO", Some("0")),
    spec("MINIO_ENDPOINT_URL", None),
    spec("MINIO_ENDPOINT_SSL", Some("0")),
    spec("SIGNED_URL_EXPIRATION", Some("3600")),
    spec("FILE_SIZE_LIMIT", Some("5242880")),
    // Auth and web
    secret("SECRET_KEY", true),
    spec("SESSION_COOKIE_NAME", Some("session-id")),
    spec("SESSION_COOKIE_AGE", Some("604800")),
    spec("SESSION_SAVE_EVERY_REQUEST", Some("0")),
    spec("COOKIE_DOMAIN", None),
    spec("ADMIN_SESSION_COOKIE_AGE", Some("3600")),
    spec("CORS_ALLOWED_ORIGINS", Some("")),
    spec("WEB_URL", None),
    spec("APP_BASE_URL", None),
    spec("ADMIN_BASE_URL", None),
    spec("SPACE_BASE_URL", None),
    spec("LIVE_BASE_URL", None),
    spec("APP_BASE_PATH", Some("/")),
    spec("ADMIN_BASE_PATH", Some("/god-mode/")),
    spec("SPACE_BASE_PATH", Some("/spaces/")),
    spec("LIVE_BASE_PATH", Some("/live/")),
    spec("NUM_PROXIES", None),
    spec("API_KEY_RATE_LIMIT", Some("60/minute")),
    spec("AUTHENTICATION_RATE_LIMIT", Some("10/minute")),
    // Frontend (build-time; the FE code is not changed)
    spec("VITE_API_BASE_URL", Some("")),
    spec("VITE_WEB_BASE_URL", None),
    spec("VITE_ADMIN_BASE_URL", None),
    spec("VITE_SPACE_BASE_URL", None),
    spec("VITE_LIVE_BASE_URL", None),
    // Email
    spec("EMAIL_HOST", None),
    spec("EMAIL_PORT", Some("587")),
    spec("EMAIL_HOST_USER", None),
    secret("EMAIL_HOST_PASSWORD", false),
    spec("EMAIL_USE_TLS", Some("1")),
    spec("EMAIL_USE_SSL", Some("0")),
    spec("EMAIL_FROM", Some("Team Plane <team@mailer.plane.so>")),
    // OAuth
    spec("GOOGLE_CLIENT_ID", None),
    secret("GOOGLE_CLIENT_SECRET", false),
    spec("GITHUB_CLIENT_ID", None),
    secret("GITHUB_CLIENT_SECRET", false),
    spec("GITLAB_CLIENT_ID", None),
    secret("GITLAB_CLIENT_SECRET", false),
    spec("GITLAB_HOST", Some("https://gitlab.com")),
    spec("GITEA_CLIENT_ID", None),
    secret("GITEA_CLIENT_SECRET", false),
    spec("GITEA_HOST", None),
    // Read by settings/common.py and settings/production.py at import time
    spec("SKIP_ENV_VAR", Some("1")),
    spec("WEBHOOK_ALLOWED_IPS", Some("")),
    spec("WEBHOOK_ALLOWED_HOSTS", Some("")),
    spec("WEBHOOK_DISALLOWED_DOMAINS", Some("")),
    spec("ANALYTICS_BASE_API", None),
    secret("ANALYTICS_SECRET_KEY", false),
    secret("GITHUB_ACCESS_TOKEN", false),
    secret("UNSPLASH_ACCESS_KEY", false),
    spec("INSTANCE_CHANGELOG_URL", Some("")),
    spec("SCOUT_MONITOR", None),
    secret("SCOUT_KEY", false),
    // Read by settings/local.py only; kept here so a full .env maps 1:1.
    spec("EMAIL_BACKEND", None),
    // Read by settings/common.py when ENABLE_READ_REPLICA is on; Rust refuses
    // that switch (7.5) yet still names the variables it would have used.
    spec("POSTGRES_READ_REPLICA_DB", None),
    spec("POSTGRES_READ_REPLICA_USER", None),
    secret("POSTGRES_READ_REPLICA_PASSWORD", false),
    spec("POSTGRES_READ_REPLICA_HOST", None),
    spec("POSTGRES_READ_REPLICA_PORT", None),
    // Other
    spec("ENABLE_DRF_SPECTACULAR", Some("0")),
    spec("METRICS_PUSH_INTERVAL_MINUTES", Some("360")),
    spec("HARD_DELETE_AFTER_DAYS", Some("60")),
    spec("API_ACTIVITY_LOG_RETENTION_DAYS", Some("14")),
    spec("WEBHOOK_LOG_RETENTION_DAYS", Some("14")),
    spec("EMAIL_LOG_RETENTION_DAYS", Some("7")),
    spec("ALLOWED_HOSTS", Some("*")),
    spec("DEBUG", Some("0")),
    spec("DJANGO_SETTINGS_MODULE", Some(DEFAULT_SETTINGS_MODULE)),
    spec("PLANE_API_BIND", Some("0.0.0.0:8000")),
    spec("RUST_LOG", None),
];
/// `ALLOWED_HOSTS` as Django splits it: a bare comma, no trimming.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowedHosts(Vec<String>);

impl AllowedHosts {
    /// The hosts Django would keep.
    #[must_use]
    pub fn as_slice(&self) -> &[String] {
        &self.0
    }
}

/// Database settings. One read-write pool (7.5), so there is no replica group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseConfig {
    /// `DATABASE_URL`, or empty when the discrete `POSTGRES_*` names are used.
    pub url: String,
    /// `POSTGRES_DB`.
    pub name: Option<String>,
    /// `POSTGRES_USER`.
    pub user: Option<String>,
    /// `POSTGRES_HOST`.
    pub host: Option<String>,
    /// `POSTGRES_PORT`, default `5432`.
    pub port: String,
}

/// Redis settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedisConfig {
    /// `REDIS_URL`; unset is allowed, as in Django.
    pub url: Option<String>,
    /// `"rediss" in REDIS_URL`, the test Django uses for TLS.
    pub tls: bool,
    /// `ssl_cert_reqs` from the URL query, else `REDIS_SSL_CERT_REQS`, else
    /// `required`, lowercased as `_redis_ssl_kwargs` does.
    pub cert_reqs: String,
    /// `ssl_check_hostname`: false only when `cert_reqs` is `none`.
    pub check_hostname: bool,
    /// `REDIS_SSL_CA_CERTS`; set only when the variable is non-empty.
    pub ca_certs: Option<String>,
    /// `REDIS_QUEUE_DB`, default `1`.
    pub queue_db: i64,
    /// `REDIS_CACHE_DB`, default `2`.
    pub cache_db: i64,
    /// `REDIS_PREFIX`, default `plane-rs:`.
    pub prefix: String,
}

/// Object storage settings from `common.py` and `storage.py`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageConfig {
    /// `AWS_S3_BUCKET_NAME`, default `uploads`.
    pub bucket: String,
    /// `AWS_S3_ENDPOINT_URL`, falling back to `MINIO_ENDPOINT_URL`.
    pub endpoint_url: Option<String>,
    /// True when the endpoint came from `MINIO_ENDPOINT_URL`.
    pub endpoint_from_minio: bool,
    /// `USE_MINIO` parsed as `int(...) == 1`.
    pub use_minio: bool,
    /// `MINIO_ENDPOINT_SSL == "1"`.
    pub minio_ssl: bool,
    /// `SIGNED_URL_EXPIRATION`, default 3600.
    pub signed_url_expiration: i64,
}

/// Session and admin cookie settings that decide `Set-Cookie` bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CookieConfig {
    /// `SESSION_COOKIE_NAME`, default `session-id`.
    pub session_cookie_name: String,
    /// `SESSION_COOKIE_AGE`, default 604800.
    pub session_cookie_age: i64,
    /// `SESSION_SAVE_EVERY_REQUEST == "1"`.
    pub save_every_request: bool,
    /// Constant `admin-session-id` from `common.py`.
    pub admin_cookie_name: String,
    /// `ADMIN_SESSION_COOKIE_AGE`, default 3600.
    pub admin_cookie_age: i64,
    /// `COOKIE_DOMAIN`; Django defaults to `None`.
    pub domain: Option<String>,
    /// `_cookies_secure(...)` from `common.py`.
    pub secure: bool,
}

/// CORS settings produced by `_cors_settings`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorsConfig {
    /// Origins kept in order and de-duplicated, as Django does.
    pub allowed_origins: Vec<String>,
    /// True when no origin is configured (Django then allows every origin).
    pub allow_all: bool,
    /// True only when at least one origin is configured.
    pub allow_credentials: bool,
}

/// Limits and retention windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitsConfig {
    /// `FILE_SIZE_LIMIT`, default 5242880.
    pub file_size_limit: i64,
    /// `DATA_UPLOAD_MAX_MEMORY_SIZE`, which reuses `FILE_SIZE_LIMIT`.
    pub data_upload_max_memory_size: i64,
    /// `API_KEY_RATE_LIMIT`, default `60/minute`.
    pub api_key_rate_limit: String,
    /// `AUTHENTICATION_RATE_LIMIT`, read from the environment at import time
    /// by `plane/authentication/rate_limit.py`; default `10/minute`.
    pub authentication_rate_limit: String,
    /// `NUM_PROXIES` when decimal and at most three digits.
    pub num_proxies: Option<u32>,
    /// `HARD_DELETE_AFTER_DAYS`, default 60.
    pub hard_delete_after_days: i64,
    /// `API_ACTIVITY_LOG_RETENTION_DAYS`, default 14.
    pub api_activity_log_retention_days: i64,
    /// `WEBHOOK_LOG_RETENTION_DAYS`, default 14.
    pub webhook_log_retention_days: i64,
    /// `EMAIL_LOG_RETENTION_DAYS`, default 7.
    pub email_log_retention_days: i64,
}
/// Settings read once at process start.
#[derive(Debug, Clone)]
pub struct Config {
    /// `DJANGO_SETTINGS_MODULE`, default `plane.settings.production`.
    pub settings_module: String,
    /// `int(DEBUG) == 1`, as Django parses it.
    pub debug: bool,
    /// Socket address for the HTTP server (`PLANE_API_BIND`).
    pub bind: String,
    /// Required key; redacted by `Debug`.
    pub secret_key: Secret,
    /// Database group.
    pub database: DatabaseConfig,
    /// Redis group.
    pub redis: RedisConfig,
    /// Storage group.
    pub storage: StorageConfig,
    /// Cookie group.
    pub cookies: CookieConfig,
    /// CORS group.
    pub cors: CorsConfig,
    /// Limits and retention.
    pub limits: LimitsConfig,
    /// `ALLOWED_HOSTS`, split the way Django splits it.
    pub allowed_hosts: AllowedHosts,
    /// `TIME_ZONE`; a constant in Django.
    pub time_zone: String,
}

impl Config {
    /// Read the process environment and fail fast on anything unusable.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when a required value is missing or blank, a
    /// value does not parse, or a documented unsupported feature is on.
    pub fn load() -> Result<Self, ConfigError> {
        Self::from_source(&ProcessEnv)
    }

    /// Read from any [`EnvSource`]; tests pass a map.
    ///
    /// # Errors
    ///
    /// Same as [`Config::load`].
    pub fn from_source(source: &impl EnvSource) -> Result<Self, ConfigError> {
        for spec in ENV_SPECS {
            if spec.required && read(source, spec.name, None).trim().is_empty() {
                return Err(ConfigError::Missing(spec.name));
            }
        }

        // 7.5: one read-write pool. Refuse the switch instead of ignoring it.
        if read(source, "ENABLE_READ_REPLICA", Some("0")) == "1" {
            return Err(ConfigError::Unsupported(
                "ENABLE_READ_REPLICA=1 (read replica not implemented)",
            ));
        }

        let debug = py_int(&read(source, "DEBUG", Some("0")))
            .ok_or(ConfigError::Invalid("DEBUG", "an integer like 0 or 1"))?
            == 1;

        let file_size_limit = required_int(source, "FILE_SIZE_LIMIT", Some("5242880"))?;
        let session_cookie_age = required_int(source, "SESSION_COOKIE_AGE", Some("604800"))?;
        let admin_cookie_age = required_int(source, "ADMIN_SESSION_COOKIE_AGE", Some("3600"))?;
        let signed_url_expiration = required_int(source, "SIGNED_URL_EXPIRATION", Some("3600"))?;

        let web_url = non_empty(source.get("WEB_URL"));
        let app_base_url = valid_url(source.get("APP_BASE_URL"));
        let admin_base_url = valid_url(source.get("ADMIN_BASE_URL"));
        let space_base_url = valid_url(source.get("SPACE_BASE_URL"));
        let live_base_url = valid_url(source.get("LIVE_BASE_URL"));

        // `cors_allowed_origins` in `common.py`: trim, drop blanks, keep order.
        let configured_origins: Vec<String> = read(source, "CORS_ALLOWED_ORIGINS", Some(""))
            .split(',')
            .map(str::trim)
            .filter(|origin| !origin.is_empty())
            .map(str::to_string)
            .collect();
        let cors = cors_settings(
            &configured_origins,
            [
                web_url.as_deref(),
                app_base_url.as_deref(),
                admin_base_url.as_deref(),
                space_base_url.as_deref(),
                live_base_url.as_deref(),
            ],
        );
        let secure = cookies_secure(
            web_url.as_deref(),
            app_base_url.as_deref(),
            &configured_origins,
        );

        let endpoint_env = source.get("AWS_S3_ENDPOINT_URL");
        let minio_env = source.get("MINIO_ENDPOINT_URL");
        let endpoint_from_minio = endpoint_env.is_none() && minio_env.is_some();

        let settings_module = {
            let value = read(
                source,
                "DJANGO_SETTINGS_MODULE",
                Some(DEFAULT_SETTINGS_MODULE),
            );
            if value.trim().is_empty() {
                DEFAULT_SETTINGS_MODULE.to_string()
            } else {
                value
            }
        };

        Ok(Self {
            settings_module,
            debug,
            bind: read(source, "PLANE_API_BIND", Some("0.0.0.0:8000")),
            secret_key: Secret::new(read(source, "SECRET_KEY", None)),
            database: DatabaseConfig {
                url: read(source, "DATABASE_URL", None),
                name: non_empty(source.get("POSTGRES_DB")),
                user: non_empty(source.get("POSTGRES_USER")),
                host: non_empty(source.get("POSTGRES_HOST")),
                port: read(source, "POSTGRES_PORT", Some("5432")),
            },
            redis: {
                let url = source.get("REDIS_URL");
                let ssl = redis_ssl(url.as_deref(), source);
                RedisConfig {
                    tls: url.as_deref().is_some_and(|url| url.contains("rediss")),
                    url,
                    cert_reqs: ssl.cert_reqs,
                    check_hostname: ssl.check_hostname,
                    ca_certs: ssl.ca_certs,
                    queue_db: required_int(source, "REDIS_QUEUE_DB", Some("1"))?,
                    cache_db: required_int(source, "REDIS_CACHE_DB", Some("2"))?,
                    prefix: read(source, "REDIS_PREFIX", Some("plane-rs:")),
                }
            },
            storage: StorageConfig {
                bucket: read(source, "AWS_S3_BUCKET_NAME", Some("uploads")),
                endpoint_url: endpoint_env.or(minio_env),
                endpoint_from_minio,
                use_minio: required_int(source, "USE_MINIO", Some("0"))? == 1,
                minio_ssl: read(source, "MINIO_ENDPOINT_SSL", Some("0")) == "1",
                signed_url_expiration,
            },
            cookies: CookieConfig {
                session_cookie_name: read(source, "SESSION_COOKIE_NAME", Some("session-id")),
                session_cookie_age,
                save_every_request: read(source, "SESSION_SAVE_EVERY_REQUEST", Some("0")) == "1",
                admin_cookie_name: "admin-session-id".to_string(),
                admin_cookie_age,
                domain: non_empty(source.get("COOKIE_DOMAIN")),
                secure,
            },
            cors,
            limits: LimitsConfig {
                file_size_limit,
                data_upload_max_memory_size: file_size_limit,
                api_key_rate_limit: read(source, "API_KEY_RATE_LIMIT", Some("60/minute")),
                authentication_rate_limit: read(
                    source,
                    "AUTHENTICATION_RATE_LIMIT",
                    Some("10/minute"),
                ),
                num_proxies: num_proxies(source.get("NUM_PROXIES")),
                hard_delete_after_days: retention_days(source, "HARD_DELETE_AFTER_DAYS", 60),
                api_activity_log_retention_days: retention_days(
                    source,
                    "API_ACTIVITY_LOG_RETENTION_DAYS",
                    14,
                ),
                webhook_log_retention_days: retention_days(
                    source,
                    "WEBHOOK_LOG_RETENTION_DAYS",
                    14,
                ),
                email_log_retention_days: retention_days(source, "EMAIL_LOG_RETENTION_DAYS", 7),
            },
            allowed_hosts: AllowedHosts(
                read(source, "ALLOWED_HOSTS", Some("*"))
                    .split(',')
                    .map(str::to_string)
                    .collect(),
            ),
            time_zone: TIME_ZONE.to_string(),
        })
    }

    /// Settings module with the Django default applied.
    #[must_use]
    pub fn settings_module(&self) -> &str {
        &self.settings_module
    }

    /// Variables worth logging: every set `ENV_SPECS` name, secrets masked.
    #[must_use]
    pub fn redacted(&self, source: &impl EnvSource) -> BTreeMap<&'static str, String> {
        let mut snapshot = BTreeMap::new();
        for spec in ENV_SPECS {
            let value = source
                .get(spec.name)
                .or_else(|| spec.default.map(str::to_string));
            let Some(value) = value else { continue };
            let rendered = match spec.sensitivity {
                Sensitivity::Secret => "***".to_string(),
                Sensitivity::Plain => value,
            };
            snapshot.insert(spec.name, rendered);
        }
        snapshot
    }
}
/// Read a variable, falling back to Django's default.
fn read(source: &impl EnvSource, name: &str, default: Option<&str>) -> String {
    source
        .get(name)
        .or_else(|| default.map(str::to_string))
        .unwrap_or_default()
}

/// Read an integer the way `int()` does, with a clear error.
fn required_int(
    source: &impl EnvSource,
    name: &'static str,
    default: Option<&str>,
) -> Result<i64, ConfigError> {
    py_int(&read(source, name, default)).ok_or(ConfigError::Invalid(name, "an integer"))
}

/// Trim, and treat a blank result as unset (Django uses `or None`).
fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}
/// Django keeps a base URL only when it has a scheme and a netloc.
fn valid_url(value: Option<String>) -> Option<String> {
    let value = value?;
    if is_valid_url(&value) {
        Some(value)
    } else {
        None
    }
}

/// Port of `plane.utils.url.is_valid_url`: `urlparse` yields a scheme and a
/// non-empty netloc.
///
/// Django's check is exactly `all([result.scheme, result.netloc])`, so the
/// netloc rule is "everything after `//` up to `/`, `?`, or `#` is non-empty".
/// A host is *not* required: `https://:8080` passes in Python, and `https:x`
/// (no `//`) fails even though it looks like a URL.
#[must_use]
pub fn is_valid_url(url: &str) -> bool {
    let Some((_scheme, rest)) = split_scheme(url) else {
        return false;
    };
    let Some(authority) = rest.strip_prefix("//") else {
        return false;
    };
    !authority
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .is_empty()
}

/// The `urlparse` scheme split: a `:` before any `/`, `?`, or `#`, whose left
/// side starts with a letter and holds only alphanumerics or `+`, `-`, `.`.
fn split_scheme(url: &str) -> Option<(&str, &str)> {
    let mut colon = None;
    for (index, ch) in url.char_indices() {
        match ch {
            ':' => {
                colon = Some(index);
                break;
            }
            '/' | '?' | '#' => return None,
            _ => {}
        }
    }
    let end = colon?;
    let scheme = &url[..end];
    let mut chars = scheme.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    if !chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.')) {
        return None;
    }
    Some((scheme, &url[end + 1..]))
}
/// Port of `_cors_settings`: configured origins win; otherwise the instance's
/// own base URLs contribute; only when there are none is every origin allowed.
fn cors_settings(configured: &[String], base_urls: [Option<&str>; 5]) -> CorsConfig {
    // Django keeps the configured list verbatim (`list(cors_allowed_origins)`),
    // duplicates included, and only de-duplicates the base-URL fallback.
    let mut origins: Vec<String> = configured.to_vec();

    if origins.is_empty() {
        for base_url in base_urls.into_iter().flatten() {
            if let Some(origin) = base_origin(base_url) {
                if !origins.contains(&origin) {
                    origins.push(origin);
                }
            }
        }
    }

    let allow_credentials = !origins.is_empty();
    CorsConfig {
        allow_all: !allow_credentials,
        allow_credentials,
        allowed_origins: origins,
    }
}

/// `f"{scheme}://{netloc}".lower()`, or `None` when either part is empty.
///
/// `_cors_settings` builds this from `urlparse(...)`, so the netloc rule is the
/// same one [`is_valid_url`] uses, and the scheme is lowercased by `urlparse`
/// before the f-string is lowered again.
fn base_origin(url: &str) -> Option<String> {
    let (scheme, rest) = split_scheme(url.trim())?;
    let authority = rest
        .strip_prefix("//")?
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    if authority.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{authority}").to_lowercase())
}
/// Port of `_cookies_secure`.
///
/// Django tests `(web_url or app_base_url or "")`, so a set-but-blank `WEB_URL`
/// falls through to `APP_BASE_URL` while a non-empty non-https `WEB_URL` wins
/// and blocks it. The origin list is the *configured* `CORS_ALLOWED_ORIGINS`,
/// not the resolved one: the base-URL fallback never makes cookies Secure.
fn cookies_secure(
    web_url: Option<&str>,
    app_base_url: Option<&str>,
    configured_origins: &[String],
) -> bool {
    let serving_url = web_url
        .filter(|value| !value.is_empty())
        .or(app_base_url)
        .unwrap_or_default();
    if serving_url.trim().to_lowercase().starts_with("https://") {
        return true;
    }
    !configured_origins.is_empty()
        && !configured_origins
            .iter()
            .any(|origin| origin.contains("http:"))
}

/// The three `_redis_ssl_kwargs` values, in one struct.
struct RedisSsl {
    /// `ssl_cert_reqs`, lowercased, default `required`.
    cert_reqs: String,
    /// `ssl_check_hostname`: false exactly when `cert_reqs == "none"`.
    check_hostname: bool,
    /// `ssl_ca_certs`, omitted when the variable is unset or empty.
    ca_certs: Option<String>,
}

/// Port of `_redis_ssl_kwargs`: the URL query wins over `REDIS_SSL_CERT_REQS`.
fn redis_ssl(url: Option<&str>, source: &impl EnvSource) -> RedisSsl {
    let from_url = url
        .and_then(|url| url.split_once('?').map(|(_, rest)| rest))
        .and_then(|query| query.split('#').next())
        .and_then(|query| query_param(query, "ssl_cert_reqs"))
        .unwrap_or_default();
    let from_env = source.get("REDIS_SSL_CERT_REQS").unwrap_or_default();
    let picked = if from_url.is_empty() {
        from_env
    } else {
        from_url
    };
    let trimmed = picked.trim().to_lowercase();
    let cert_reqs = if trimmed.is_empty() {
        "required".to_string()
    } else {
        trimmed
    };
    let ca_certs = source
        .get("REDIS_SSL_CA_CERTS")
        .filter(|value| !value.is_empty());
    RedisSsl {
        check_hostname: cert_reqs != "none",
        cert_reqs,
        ca_certs,
    }
}

/// First value of `name` in an `application/x-www-form-urlencoded` query, the
/// way `parse_qs(...).get(name, [""])[0]` reads it: blank values are dropped.
fn query_param(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if unquote_plus(key) == name && !value.is_empty() {
            Some(unquote_plus(value))
        } else {
            None
        }
    })
}

/// `urllib.parse.unquote_plus` for UTF-8: `+` is a space, `%XX` is a byte.
fn unquote_plus(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
                match hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    None => {
                        out.push(bytes[index]);
                        index += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Port of `_retention_days`: unset, unparseable, or negative keeps the default.
fn retention_days(source: &impl EnvSource, name: &str, default: i64) -> i64 {
    match source.get(name) {
        None => default,
        Some(raw) => match py_int(&raw) {
            Some(days) if days >= 0 => days,
            _ => default,
        },
    }
}

/// Port of the `NUM_PROXIES` branch: decimal, at most three digits, else unset.
fn num_proxies(raw: Option<String>) -> Option<u32> {
    let raw = raw?.trim().to_string();
    if raw.is_empty() || raw.len() > 3 || !raw.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    raw.parse().ok()
}
/// Python's `int(str)`: surrounding whitespace, optional sign, and single
/// underscores between digits (so `0_1` is `1` and `1_0` is `10`).
#[must_use]
pub fn py_int(raw: &str) -> Option<i64> {
    let text = raw.trim();
    if text.is_empty() {
        return None;
    }
    let (sign, digits) = match text.as_bytes()[0] {
        b'+' => (1i64, &text[1..]),
        b'-' => (-1i64, &text[1..]),
        _ => (1i64, text),
    };
    if digits.is_empty() {
        return None;
    }

    let mut value: i64 = 0;
    let mut previous_digit = false;
    for (index, ch) in digits.char_indices() {
        if ch == '_' {
            let next = digits[index + 1..].chars().next();
            if !previous_digit || !next.is_some_and(|next| next.is_ascii_digit()) {
                return None;
            }
            previous_digit = false;
            continue;
        }
        if !ch.is_ascii_digit() {
            return None;
        }
        value = value
            .checked_mul(10)?
            .checked_add(i64::from(ch as u8 - b'0'))?;
        previous_digit = true;
    }
    if !previous_digit {
        return None;
    }
    value.checked_mul(sign)
}
#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    fn base() -> HashMap<String, String> {
        env(&[("SECRET_KEY", "unit-test-key")])
    }

    #[test]
    fn defaults_match_django() {
        let config = Config::from_source(&base()).expect("config");
        assert_eq!(config.settings_module, "plane.settings.production");
        assert!(!config.debug);
        assert_eq!(config.bind, "0.0.0.0:8000");
        assert_eq!(config.allowed_hosts.as_slice(), ["*"]);
        assert_eq!(config.time_zone, "UTC");
        assert_eq!(config.limits.file_size_limit, 5_242_880);
        assert_eq!(config.limits.data_upload_max_memory_size, 5_242_880);
        assert_eq!(config.limits.api_key_rate_limit, "60/minute");
        assert_eq!(config.limits.authentication_rate_limit, "10/minute");
        assert_eq!(config.limits.num_proxies, None);
        assert_eq!(config.cookies.session_cookie_name, "session-id");
        assert_eq!(config.cookies.session_cookie_age, 604_800);
        assert_eq!(config.cookies.admin_cookie_name, "admin-session-id");
        assert_eq!(config.cookies.admin_cookie_age, 3_600);
        assert_eq!(config.cookies.domain, None);
        assert!(!config.cookies.secure);
        assert_eq!(config.storage.bucket, "uploads");
        assert_eq!(config.storage.signed_url_expiration, 3_600);
        assert_eq!(config.database.port, "5432");
        assert_eq!(config.redis.queue_db, 1);
        assert_eq!(config.redis.cache_db, 2);
        assert_eq!(config.redis.prefix, "plane-rs:");
        assert_eq!(config.redis.cert_reqs, "required");
        assert!(!config.redis.tls);
        assert!(!config.storage.use_minio);
        assert_eq!(config.limits.hard_delete_after_days, 60);
        assert_eq!(config.limits.api_activity_log_retention_days, 14);
        assert_eq!(config.limits.webhook_log_retention_days, 14);
        assert_eq!(config.limits.email_log_retention_days, 7);
        assert!(config.cors.allow_all);
        assert!(!config.cors.allow_credentials);
        assert!(config.cors.allowed_origins.is_empty());
    }

    #[test]
    fn missing_secret_key_fails_with_its_name() {
        let error = Config::from_source(&HashMap::new()).expect_err("must fail");
        assert_eq!(error, ConfigError::Missing("SECRET_KEY"));
        assert_eq!(error.to_string(), "SECRET_KEY is required but unset");
    }

    #[test]
    fn blank_secret_key_is_missing() {
        let error = Config::from_source(&env(&[("SECRET_KEY", "   ")])).expect_err("blank");
        assert_eq!(error, ConfigError::Missing("SECRET_KEY"));
    }

    #[test]
    fn read_replica_is_refused() {
        let mut vars = base();
        vars.insert("ENABLE_READ_REPLICA".to_string(), "1".to_string());
        let error = Config::from_source(&vars).expect_err("replica");
        assert_eq!(
            error,
            ConfigError::Unsupported("ENABLE_READ_REPLICA=1 (read replica not implemented)")
        );
        assert!(error.to_string().contains("read replica not implemented"));
    }

    #[test]
    fn read_replica_off_keeps_one_pool() {
        for value in ["0", "", "true", "yes"] {
            let mut vars = base();
            vars.insert("ENABLE_READ_REPLICA".to_string(), value.to_string());
            assert!(Config::from_source(&vars).is_ok(), "value {value:?}");
        }
    }

    #[test]
    fn debug_follows_python_int_rules() {
        for (raw, expected) in [
            ("1", true),
            ("0", false),
            ("01", true),
            (" 1 ", true),
            ("0_1", true),
            ("1_0", false),
            ("-0", false),
            ("+1", true),
        ] {
            let mut vars = base();
            vars.insert("DEBUG".to_string(), raw.to_string());
            let config = Config::from_source(&vars).expect("config");
            assert_eq!(config.debug, expected, "DEBUG={raw:?}");
        }
    }

    #[test]
    fn debug_must_be_an_integer() {
        let mut vars = base();
        vars.insert("DEBUG".to_string(), "release".to_string());
        let error = Config::from_source(&vars).expect_err("debug");
        assert_eq!(
            error,
            ConfigError::Invalid("DEBUG", "an integer like 0 or 1")
        );
        assert_eq!(error.to_string(), "DEBUG must be an integer like 0 or 1");
    }

    #[test]
    fn booleans_only_accept_one_like_django() {
        let mut vars = base();
        vars.insert("SESSION_SAVE_EVERY_REQUEST".to_string(), "true".to_string());
        vars.insert("MINIO_ENDPOINT_SSL".to_string(), "yes".to_string());
        vars.insert("USE_MINIO".to_string(), "01".to_string());
        let config = Config::from_source(&vars).expect("config");
        assert!(!config.cookies.save_every_request);
        assert!(!config.storage.minio_ssl);
        assert!(config.storage.use_minio);
    }
    #[test]
    fn num_proxies_requires_decimal_of_at_most_three_digits() {
        assert_eq!(num_proxies(Some("2".to_string())), Some(2));
        assert_eq!(num_proxies(Some(" 2 ".to_string())), Some(2));
        assert_eq!(num_proxies(Some("000".to_string())), Some(0));
        assert_eq!(num_proxies(Some("1000".to_string())), None);
        assert_eq!(num_proxies(Some("-1".to_string())), None);
        assert_eq!(num_proxies(Some("1.5".to_string())), None);
        assert_eq!(num_proxies(Some(String::new())), None);
        assert_eq!(num_proxies(None), None);
    }

    #[test]
    fn retention_keeps_the_default_for_bad_values() {
        let mut vars = base();
        vars.insert("WEBHOOK_LOG_RETENTION_DAYS".to_string(), "-5".to_string());
        vars.insert("EMAIL_LOG_RETENTION_DAYS".to_string(), "nope".to_string());
        vars.insert(
            "API_ACTIVITY_LOG_RETENTION_DAYS".to_string(),
            "30".to_string(),
        );
        let config = Config::from_source(&vars).expect("config");
        assert_eq!(config.limits.webhook_log_retention_days, 14);
        assert_eq!(config.limits.email_log_retention_days, 7);
        assert_eq!(config.limits.api_activity_log_retention_days, 30);
    }

    #[test]
    fn cors_falls_back_to_the_instance_base_urls() {
        let mut vars = base();
        vars.insert("WEB_URL".to_string(), "https://app.example.com".to_string());
        let config = Config::from_source(&vars).expect("config");
        assert_eq!(config.cors.allowed_origins, ["https://app.example.com"]);
        assert!(!config.cors.allow_all);
        assert!(config.cors.allow_credentials);
        // WEB_URL itself is https, so `_cookies_secure` returns True before it
        // even looks at the origin list.
        assert!(config.cookies.secure);
    }

    #[test]
    fn configured_cors_origins_are_kept_verbatim() {
        // Django does not de-duplicate the configured list, and `_cookies_secure`
        // reads that list rather than the resolved one.
        let mut vars = base();
        vars.insert("WEB_URL".to_string(), "http://localhost:3000".to_string());
        vars.insert(
            "CORS_ALLOWED_ORIGINS".to_string(),
            "https://a.test, https://b.test ,https://a.test".to_string(),
        );
        let config = Config::from_source(&vars).expect("config");
        assert_eq!(
            config.cors.allowed_origins,
            ["https://a.test", "https://b.test", "https://a.test"]
        );
        assert!(config.cookies.secure);
    }

    #[test]
    fn base_url_fallback_does_not_make_cookies_secure() {
        // `_cookies_secure` reads the *configured* origin list, so an https
        // origin that only the ADMIN_BASE_URL fallback supplied leaves cookies
        // non-Secure. Using the resolved list would wrongly flip this to true.
        let mut vars = base();
        vars.insert(
            "ADMIN_BASE_URL".to_string(),
            "https://admin.example.com".to_string(),
        );
        let config = Config::from_source(&vars).expect("config");
        assert_eq!(config.cors.allowed_origins, ["https://admin.example.com"]);
        assert!(config.cors.allow_credentials);
        assert!(!config.cookies.secure);

        let mut vars = base();
        vars.insert(
            "CORS_ALLOWED_ORIGINS".to_string(),
            "https://admin.example.com".to_string(),
        );
        assert!(Config::from_source(&vars).expect("config").cookies.secure);
    }

    #[test]
    fn blank_web_url_falls_through_to_app_base_url() {
        // `(web_url or app_base_url or "")`: a blank WEB_URL loses to APP_BASE_URL.
        let mut vars = base();
        vars.insert("WEB_URL".to_string(), "  ".to_string());
        vars.insert(
            "APP_BASE_URL".to_string(),
            "https://app.example.com".to_string(),
        );
        vars.insert(
            "CORS_ALLOWED_ORIGINS".to_string(),
            "https://app.example.com".to_string(),
        );
        assert!(Config::from_source(&vars).expect("config").cookies.secure);

        // A non-empty http WEB_URL wins over an https APP_BASE_URL, so the
        // scheme check fails and the http origin keeps cookies non-Secure.
        let mut vars = base();
        vars.insert("WEB_URL".to_string(), "http://localhost:3000".to_string());
        vars.insert(
            "APP_BASE_URL".to_string(),
            "https://app.example.com".to_string(),
        );
        vars.insert(
            "CORS_ALLOWED_ORIGINS".to_string(),
            "http://localhost:8080".to_string(),
        );
        assert!(!Config::from_source(&vars).expect("config").cookies.secure);
    }

    #[test]
    fn plain_http_install_keeps_cookies_insecure() {
        let mut vars = base();
        vars.insert("WEB_URL".to_string(), "http://localhost:3000".to_string());
        vars.insert(
            "CORS_ALLOWED_ORIGINS".to_string(),
            "http://localhost:8080".to_string(),
        );
        let config = Config::from_source(&vars).expect("config");
        assert!(!config.cookies.secure);
    }

    #[test]
    fn invalid_base_urls_are_dropped_like_django() {
        let mut vars = base();
        vars.insert("APP_BASE_URL".to_string(), "not a url".to_string());
        vars.insert("WEB_URL".to_string(), "https://web.test".to_string());
        let config = Config::from_source(&vars).expect("config");
        assert_eq!(config.cors.allowed_origins, ["https://web.test"]);
    }

    #[test]
    fn minio_endpoint_is_the_storage_fallback() {
        let mut vars = base();
        vars.insert(
            "MINIO_ENDPOINT_URL".to_string(),
            "http://minio:9000".to_string(),
        );
        let config = Config::from_source(&vars).expect("config");
        assert_eq!(
            config.storage.endpoint_url.as_deref(),
            Some("http://minio:9000")
        );
        assert!(config.storage.endpoint_from_minio);

        let mut vars = base();
        vars.insert(
            "MINIO_ENDPOINT_URL".to_string(),
            "http://minio:9000".to_string(),
        );
        vars.insert(
            "AWS_S3_ENDPOINT_URL".to_string(),
            "http://s3:9000".to_string(),
        );
        let config = Config::from_source(&vars).expect("config");
        assert_eq!(
            config.storage.endpoint_url.as_deref(),
            Some("http://s3:9000")
        );
        assert!(!config.storage.endpoint_from_minio);
    }

    #[test]
    fn redis_tls_matches_the_substring_test() {
        let mut vars = base();
        vars.insert("REDIS_URL".to_string(), "rediss://redis:6379/0".to_string());
        assert!(Config::from_source(&vars).expect("config").redis.tls);

        let mut vars = base();
        vars.insert("REDIS_URL".to_string(), "redis://redis:6379/0".to_string());
        assert!(!Config::from_source(&vars).expect("config").redis.tls);
    }

    #[test]
    fn blank_redis_cert_reqs_defaults_to_required() {
        let mut vars = base();
        vars.insert("REDIS_SSL_CERT_REQS".to_string(), "  ".to_string());
        let config = Config::from_source(&vars).expect("config");
        assert_eq!(config.redis.cert_reqs, "required");

        let mut vars = base();
        vars.insert("REDIS_SSL_CERT_REQS".to_string(), "NONE".to_string());
        let config = Config::from_source(&vars).expect("config");
        assert_eq!(config.redis.cert_reqs, "none");
        assert!(!config.redis.check_hostname);
        assert_eq!(config.redis.ca_certs, None);
    }

    #[test]
    fn redis_url_cert_reqs_beats_the_environment() {
        // `_redis_ssl_kwargs`: `url_cert_reqs or os.environ.get(...)`.
        let mut vars = base();
        vars.insert(
            "REDIS_URL".to_string(),
            "rediss://redis:6379/0?ssl_cert_reqs=none".to_string(),
        );
        vars.insert("REDIS_SSL_CERT_REQS".to_string(), "required".to_string());
        let config = Config::from_source(&vars).expect("config");
        assert_eq!(config.redis.cert_reqs, "none");
        assert!(!config.redis.check_hostname);

        // A query value is percent-decoded like `parse_qs` does.
        let mut vars = base();
        vars.insert(
            "REDIS_URL".to_string(),
            "rediss://redis:6379/0?db=0&ssl_cert_reqs=%6Eone".to_string(),
        );
        let config = Config::from_source(&vars).expect("config");
        assert_eq!(config.redis.cert_reqs, "none");

        // `parse_qs` drops blank values, so an empty query param falls back.
        let mut vars = base();
        vars.insert(
            "REDIS_URL".to_string(),
            "rediss://redis:6379/0?ssl_cert_reqs=".to_string(),
        );
        vars.insert("REDIS_SSL_CERT_REQS".to_string(), "none".to_string());
        let config = Config::from_source(&vars).expect("config");
        assert_eq!(config.redis.cert_reqs, "none");
    }

    #[test]
    fn redis_ca_certs_is_kept_only_when_non_empty() {
        let mut vars = base();
        vars.insert(
            "REDIS_SSL_CA_CERTS".to_string(),
            "/etc/ssl/ca.pem".to_string(),
        );
        let config = Config::from_source(&vars).expect("config");
        assert_eq!(config.redis.ca_certs.as_deref(), Some("/etc/ssl/ca.pem"));

        let mut vars = base();
        vars.insert("REDIS_SSL_CA_CERTS".to_string(), String::new());
        assert_eq!(
            Config::from_source(&vars).expect("config").redis.ca_certs,
            None
        );
    }
    #[test]
    fn secrets_never_render() {
        let vars = env(&[
            ("SECRET_KEY", "unit-test-key"),
            ("POSTGRES_PASSWORD", "db-pass"),
            ("AWS_SECRET_ACCESS_KEY", "aws-pass"),
        ]);
        let config = Config::from_source(&vars).expect("config");
        let debug = format!("{config:?}");
        assert!(!debug.contains("unit-test-key"));
        assert_eq!(config.secret_key.to_string(), "***");
        assert_eq!(config.secret_key.expose(), "unit-test-key");

        let snapshot = config.redacted(&vars);
        assert_eq!(snapshot.get("SECRET_KEY").map(String::as_str), Some("***"));
        assert_eq!(
            snapshot.get("POSTGRES_PASSWORD").map(String::as_str),
            Some("***")
        );
        assert_eq!(
            snapshot.get("AWS_SECRET_ACCESS_KEY").map(String::as_str),
            Some("***")
        );
        assert!(!format!("{snapshot:?}").contains("db-pass"));
    }

    #[test]
    fn redacted_snapshot_uses_defaults_for_unset_names() {
        let config = Config::from_source(&base()).expect("config");
        let snapshot = config.redacted(&base());
        assert_eq!(
            snapshot.get("POSTGRES_PORT").map(String::as_str),
            Some("5432")
        );
        assert_eq!(
            snapshot.get("FILE_SIZE_LIMIT").map(String::as_str),
            Some("5242880")
        );
        assert_eq!(snapshot.get("DATABASE_URL"), None);
    }

    #[test]
    fn placeholder_secret_keys_are_flagged() {
        assert!(Secret::new("change-this-key-on-deployment").is_insecure());
        assert!(!Secret::new("unit-test-key").is_insecure());
    }

    #[test]
    fn empty_settings_module_falls_back() {
        let mut vars = base();
        vars.insert("DJANGO_SETTINGS_MODULE".to_string(), String::new());
        let config = Config::from_source(&vars).expect("config");
        assert_eq!(config.settings_module(), "plane.settings.production");
    }

    #[test]
    fn allowed_hosts_split_on_a_bare_comma() {
        let mut vars = base();
        vars.insert("ALLOWED_HOSTS".to_string(), "a.test,b.test".to_string());
        let config = Config::from_source(&vars).expect("config");
        assert_eq!(config.allowed_hosts.as_slice(), ["a.test", "b.test"]);

        let mut vars = base();
        vars.insert("ALLOWED_HOSTS".to_string(), "a.test,".to_string());
        let config = Config::from_source(&vars).expect("config");
        assert_eq!(config.allowed_hosts.as_slice(), ["a.test", ""]);
    }

    #[test]
    fn every_spec_name_is_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for spec in ENV_SPECS {
            assert!(seen.insert(spec.name), "duplicate name {}", spec.name);
        }
    }

    #[test]
    fn documented_names_are_all_in_the_table() {
        // 18.1 lists these explicitly; a rename must fail here, not at cutover.
        for name in [
            "DATABASE_URL",
            "ENABLE_READ_REPLICA",
            "DATABASE_READ_REPLICA_URL",
            "REDIS_URL",
            "REDIS_HOST",
            "REDIS_PORT",
            "AWS_S3_BUCKET_NAME",
            "USE_MINIO",
            "MINIO_ENDPOINT_URL",
            "MINIO_ENDPOINT_SSL",
            "SIGNED_URL_EXPIRATION",
            "FILE_SIZE_LIMIT",
            "SECRET_KEY",
            "COOKIE_DOMAIN",
            "CORS_ALLOWED_ORIGINS",
            "WEB_URL",
            "APP_BASE_URL",
            "ADMIN_BASE_URL",
            "SPACE_BASE_URL",
            "LIVE_BASE_URL",
            "NUM_PROXIES",
            "API_KEY_RATE_LIMIT",
            "AUTHENTICATION_RATE_LIMIT",
            "VITE_API_BASE_URL",
            "EMAIL_HOST",
            "EMAIL_PORT",
            "EMAIL_HOST_USER",
            "EMAIL_HOST_PASSWORD",
            "EMAIL_USE_TLS",
            "EMAIL_FROM",
            "ENABLE_DRF_SPECTACULAR",
            "METRICS_PUSH_INTERVAL_MINUTES",
            "HARD_DELETE_AFTER_DAYS",
            // settings/common.py and settings/production.py read these too; a
            // full `.env` must map 1:1, so the table must name them.
            "SKIP_ENV_VAR",
            "WEBHOOK_ALLOWED_IPS",
            "WEBHOOK_ALLOWED_HOSTS",
            "WEBHOOK_DISALLOWED_DOMAINS",
            "ANALYTICS_BASE_API",
            "ANALYTICS_SECRET_KEY",
            "GITHUB_ACCESS_TOKEN",
            "UNSPLASH_ACCESS_KEY",
            "INSTANCE_CHANGELOG_URL",
            "SCOUT_MONITOR",
            "SCOUT_KEY",
            "EMAIL_BACKEND",
            "POSTGRES_READ_REPLICA_DB",
            "POSTGRES_READ_REPLICA_USER",
            "POSTGRES_READ_REPLICA_PASSWORD",
            "POSTGRES_READ_REPLICA_HOST",
            "POSTGRES_READ_REPLICA_PORT",
        ] {
            assert!(
                ENV_SPECS.iter().any(|spec| spec.name == name),
                "missing {name}"
            );
        }
    }

    #[test]
    fn py_int_matches_cpython() {
        assert_eq!(py_int("1"), Some(1));
        assert_eq!(py_int(" -2 "), Some(-2));
        assert_eq!(py_int("0_1"), Some(1));
        assert_eq!(py_int("1_0"), Some(10));
        assert_eq!(py_int("_1"), None);
        assert_eq!(py_int("1_"), None);
        assert_eq!(py_int("1__0"), None);
        assert_eq!(py_int(""), None);
        assert_eq!(py_int("+"), None);
        assert_eq!(py_int("1.0"), None);
        assert_eq!(py_int("0x10"), None);
    }

    #[test]
    fn is_valid_url_matches_urlparse() {
        // Cases mirror `urlparse(url).scheme and .netloc` from
        // `plane/utils/url.py`; the odd ones are deliberate.
        assert!(is_valid_url("https://example.com"));
        assert!(is_valid_url("http://localhost:3000/path"));
        assert!(is_valid_url("https://user:pass@example.com/x"));
        assert!(is_valid_url("https://:8080"));
        assert!(is_valid_url("custom+v1.2-x://host"));
        assert!(!is_valid_url("mailto:x@y.test"));
        assert!(!is_valid_url("example.com"));
        assert!(!is_valid_url("https://"));
        assert!(!is_valid_url("https:///path"));
        assert!(!is_valid_url("https:x"));
        assert!(!is_valid_url(""));
        assert!(!is_valid_url("/relative"));
        assert!(!is_valid_url("1http://host"));
        assert!(!is_valid_url("ht tp://host"));
    }

    #[test]
    fn base_origin_lowercases_and_drops_the_path() {
        assert_eq!(
            base_origin("HTTPS://App.Example.COM/Path?q=1"),
            Some("https://app.example.com".to_string())
        );
        assert_eq!(
            base_origin("  https://app.example.com  "),
            Some("https://app.example.com".to_string())
        );
        assert_eq!(base_origin("https://"), None);
        assert_eq!(base_origin("app.example.com"), None);
        assert_eq!(base_origin(""), None);
    }
    #[test]
    fn every_recorded_helper_case_matches() {
        // The rows come from `contracts/tools/capture_config.py`, which executes
        // the real helper functions out of `plane/settings/common.py` with
        // `ast`, so this compares against Plane's own code, not a re-typed copy.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../contracts/golden/config-helpers.json");
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let golden: serde_json::Value = serde_json::from_str(&raw).expect("golden json");

        // `urlparse` moved between 3.10 and 3.12, and `apps/api/pyproject.toml`
        // pins `>=3.12,<3.13`, so a golden recorded with another interpreter is
        // invalid evidence rather than a diff to fix by hand.
        let python = golden["python"].as_str().expect("python field");
        assert!(
            python.starts_with("3.12."),
            "golden recorded with Python {python}; re-run capture_config.py with \
             apps/api/.venv (3.12) to match apps/api/pyproject.toml"
        );

        for (url, expected) in golden["is_valid_url"].as_object().expect("is_valid_url") {
            assert_eq!(
                is_valid_url(url),
                expected.as_bool().expect("bool"),
                "is_valid_url({url:?})"
            );
        }

        for row in golden["cors"].as_array().expect("cors") {
            let raw = row["raw"].as_str().expect("raw");
            let configured: Vec<String> = raw
                .split(',')
                .map(str::trim)
                .filter(|origin| !origin.is_empty())
                .map(str::to_string)
                .collect();
            let base_urls: Vec<String> = row["base_urls"]
                .as_array()
                .expect("base_urls")
                .iter()
                .map(|value| value.as_str().unwrap_or_default().to_string())
                .collect();
            let at = |index: usize| {
                base_urls
                    .get(index)
                    .map(String::as_str)
                    .filter(|value| !value.is_empty())
            };
            let computed = cors_settings(&configured, [at(0), at(1), at(2), at(3), at(4)]);
            let expected: Vec<String> = row["origins"]
                .as_array()
                .expect("origins")
                .iter()
                .map(|value| value.as_str().expect("origin").to_string())
                .collect();
            assert_eq!(computed.allowed_origins, expected, "cors {raw:?}");
            assert_eq!(
                computed.allow_all,
                row["allow_all"].as_bool().expect("allow_all"),
                "cors {raw:?}"
            );
            assert_eq!(
                computed.allow_credentials,
                row["allow_credentials"]
                    .as_bool()
                    .expect("allow_credentials"),
                "cors {raw:?}"
            );
        }

        for row in golden["cookies_secure"].as_array().expect("cookies_secure") {
            let origins: Vec<String> = row["origins"]
                .as_array()
                .expect("origins")
                .iter()
                .map(|value| value.as_str().expect("origin").to_string())
                .collect();
            let web_url = row["web_url"].as_str();
            let app_base_url = row["app_base_url"].as_str();
            assert_eq!(
                cookies_secure(web_url, app_base_url, &origins),
                row["secure"].as_bool().expect("secure"),
                "cookies_secure({web_url:?}, {app_base_url:?}, {origins:?})"
            );
        }

        for row in golden["retention_days"].as_array().expect("retention_days") {
            let default = row["default"].as_i64().expect("default");
            let vars: HashMap<String, String> = match row["raw"].as_str() {
                Some(raw) => HashMap::from([("RETENTION_UNDER_TEST".to_string(), raw.to_string())]),
                None => HashMap::new(),
            };
            assert_eq!(
                retention_days(&vars, "RETENTION_UNDER_TEST", default),
                row["days"].as_i64().expect("days"),
                "retention for {row:?}"
            );
        }

        for row in golden["redis_ssl"].as_array().expect("redis_ssl") {
            let vars: HashMap<String, String> = row["env"]
                .as_object()
                .expect("env")
                .iter()
                .map(|(key, value)| (key.clone(), value.as_str().expect("value").to_string()))
                .collect();
            let ssl = redis_ssl(row["url"].as_str(), &vars);
            let expected = row["kwargs"].as_object().expect("kwargs");
            assert_eq!(
                ssl.cert_reqs,
                expected["ssl_cert_reqs"].as_str().expect("cert_reqs"),
                "cert_reqs for {row:?}"
            );
            assert_eq!(
                ssl.check_hostname,
                expected["ssl_check_hostname"].as_bool().expect("hostname"),
                "ssl_check_hostname for {row:?}"
            );
            assert_eq!(
                ssl.ca_certs.as_deref(),
                expected
                    .get("ssl_ca_certs")
                    .and_then(|value| value.as_str()),
                "ssl_ca_certs for {row:?}"
            );
        }
    }
}

//! Parsing the operator's database URL into the exact driver options, and
//! the transport-security policy decided from those same options.
//!
//! The connection comes only from the one named variable (D115): user, host,
//! and database must be in the URL; every driver option is set explicitly;
//! Postgres environment variables that would change the connection make the
//! target unusable, and the password file is never read. Only a fixed list
//! of URL parameters is accepted, each at most once.

use std::fmt;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use percent_encoding::percent_decode_str;
use sqlx::mysql::{MySqlConnectOptions, MySqlSslMode};
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use url::Url;

use crate::redact::Secrets;
use crate::sqlite::SqliteTarget;
use crate::{ENV_PREFIX, SqlError};

/// The database engine behind the configured URL.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SqlEngine {
    Sqlite,
    Postgres,
    MySql,
}

impl SqlEngine {
    /// Operator-facing engine name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Sqlite => "sqlite",
            Self::Postgres => "postgres",
            Self::MySql => "mysql",
        }
    }

    /// Whether talking to this engine can open a network connection.
    pub const fn uses_network(self) -> bool {
        !matches!(self, Self::Sqlite)
    }
}

/// Whether `name` is an acceptable `--sql-env` variable name:
/// `COMPRESSO_SQL_` followed by at least one of `A-Z`, `0-9`, `_`.
pub fn is_valid_env_name(name: &str) -> bool {
    name.strip_prefix(ENV_PREFIX).is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    })
}

/// Postgres environment variables the driver would read to change the
/// connection (`libpq` names). Any of them set makes a Postgres target
/// unusable, so the connection comes only from the operator's variable.
pub const POSTGRES_ENVIRONMENT: &[&str] = &[
    "PGHOST",
    "PGHOSTADDR",
    "PGPORT",
    "PGUSER",
    "PGPASSWORD",
    "PGPASSFILE",
    "PGDATABASE",
    "PGOPTIONS",
    "PGSSLMODE",
    "PGSSLROOTCERT",
    "PGSSLCERT",
    "PGSSLKEY",
    "PGAPPNAME",
];

/// Transport security the driver is told to use.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Tls {
    /// Loopback or a local socket: the URL's own mode (default: try TLS,
    /// fall back to plain).
    Local(LocalTls),
    /// Any other host: TLS with certificate and host-name verification.
    VerifyFull,
}

/// The URL's own mode for a local connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LocalTls {
    Disable,
    Prefer,
    Require,
    VerifyCa,
    VerifyFull,
}

/// The driver options a connection uses, built once from the URL.
#[derive(Clone)]
pub(crate) enum DriverOptions {
    Postgres(PgConnectOptions),
    MySql(MySqlConnectOptions),
}

/// A parsed network target. Nothing in it prints through `Debug`.
pub(crate) struct NetworkTarget {
    pub(crate) engine: SqlEngine,
    pub(crate) options: DriverOptions,
    pub(crate) tls: Tls,
}

impl fmt::Debug for NetworkTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NetworkTarget")
            .field("engine", &self.engine)
            .field("tls", &self.tls)
            .finish_non_exhaustive()
    }
}

/// A usable target and the strings its error messages must never contain.
pub(crate) enum SqlTarget {
    Sqlite(SqliteTarget),
    Network(Box<NetworkTarget>, Secrets),
}

impl fmt::Debug for SqlTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.engine().name())
    }
}

/// One accepted URL parameter: its canonical name and its spellings.
struct Param {
    name: &'static str,
    spellings: &'static [&'static str],
}

const SSL_MODE: Param = Param {
    name: "sslmode",
    spellings: &["sslmode", "ssl-mode"],
};
const SSL_ROOT: Param = Param {
    name: "sslrootcert",
    spellings: &["sslrootcert", "ssl-root-cert", "sslca", "ssl-ca"],
};
const SSL_CERT: Param = Param {
    name: "sslcert",
    spellings: &["sslcert", "ssl-cert"],
};
const SSL_KEY: Param = Param {
    name: "sslkey",
    spellings: &["sslkey", "ssl-key"],
};
const SOCKET: Param = Param {
    name: "socket",
    spellings: &["socket"],
};

/// Parameters a Postgres URL may carry. Host, port, user, password, and
/// database come only from the URL's own parts; `options`, `host`,
/// `hostaddr`, `application_name`, and everything else are refused.
const POSTGRES_PARAMS: &[Param] = &[SSL_MODE, SSL_ROOT, SSL_CERT, SSL_KEY];
/// Parameters a MySQL/MariaDB URL may carry. `charset` is refused: the
/// connection is always utf8mb4 (a multibyte charset such as gbk can swallow
/// a backslash).
const MYSQL_PARAMS: &[Param] = &[SSL_MODE, SSL_ROOT, SSL_CERT, SSL_KEY, SOCKET];

/// The accepted parameters of a URL by canonical name, each at most once.
fn parameters(url: &Url, allowed: &[Param]) -> Result<Vec<(&'static str, String)>, String> {
    let mut found: Vec<(&'static str, String)> = Vec::new();
    for (key, value) in url.query_pairs() {
        let Some(param) = allowed
            .iter()
            .find(|param| param.spellings.contains(&key.as_ref()))
        else {
            return Err(format!(
                "the URL carries an unsupported parameter '{}' (accepted: {})",
                printable_key(&key),
                allowed
                    .iter()
                    .map(|param| param.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        };
        if found.iter().any(|(name, _)| *name == param.name) {
            return Err(format!(
                "the URL gives the parameter '{}' more than once",
                param.name
            ));
        }
        found.push((param.name, value.into_owned()));
    }
    Ok(found)
}

fn decoded(text: &str, part: &str) -> Result<String, String> {
    percent_decode_str(text)
        .decode_utf8()
        .map(|text| text.into_owned())
        .map_err(|_| format!("the URL's {part} is not valid UTF-8"))
}

/// The first Postgres environment variable that is set, if any.
pub(crate) fn postgres_environment_conflict(
    lookup: impl Fn(&str) -> Option<String>,
) -> Option<&'static str> {
    POSTGRES_ENVIRONMENT
        .iter()
        .copied()
        .find(|name| lookup(name).is_some())
}

impl SqlTarget {
    /// Parses one URL against the Postgres variables `environment` reports
    /// as set. The error never contains the URL or its parts.
    pub(crate) fn parse_with_environment(
        value: &str,
        protected: Vec<PathBuf>,
        environment: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, String> {
        let value = value.trim();
        if let Some(rest) = value.strip_prefix("sqlite:") {
            let path = rest.strip_prefix("//").unwrap_or(rest);
            if path.contains(['\0', '?', '#']) {
                return Err(
                    "a sqlite: URL holds only an absolute file path, with no parameters".to_owned(),
                );
            }
            let path = Path::new(path);
            if !path.is_absolute() {
                return Err("a sqlite: URL must hold an absolute file path".to_owned());
            }
            return SqliteTarget::new(path.to_path_buf(), protected).map(Self::Sqlite);
        }
        let url = Url::parse(value)
            .map_err(|_| "the value is not a sqlite:, postgres://, or mysql:// URL".to_owned())?;
        let engine = match url.scheme() {
            "postgres" | "postgresql" => SqlEngine::Postgres,
            "mysql" | "mariadb" => SqlEngine::MySql,
            _ => {
                return Err(
                    "the URL scheme must be sqlite:, postgres://, postgresql://, mysql://, or mariadb://"
                        .to_owned(),
                );
            }
        };
        if url.fragment().is_some() {
            return Err("the URL must not carry a fragment".to_owned());
        }
        let params = parameters(
            &url,
            if engine == SqlEngine::Postgres {
                POSTGRES_PARAMS
            } else {
                MYSQL_PARAMS
            },
        )?;
        let param = |name: &str| {
            params
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.clone())
        };
        let user = decoded(url.username(), "user")?;
        if user.is_empty() {
            return Err("the URL must name its user (user@host)".to_owned());
        }
        let password = url
            .password()
            .map(|password| decoded(password, "password"))
            .transpose()?;
        let host = decoded(url.host_str().unwrap_or_default(), "host")?;
        let socket = param("socket");
        if host.is_empty() && socket.is_none() {
            return Err("the URL must name its host".to_owned());
        }
        let database = decoded(url.path().trim_start_matches('/'), "database")?;
        if database.is_empty() || database.contains('/') {
            return Err("the URL must name exactly one database (…/database)".to_owned());
        }
        let mode = param("sslmode")
            .map(|mode| parse_mode(engine, &mode))
            .transpose()?;
        let mut secrets = Secrets::from_url(
            value,
            &url,
            &[
                "sslkey",
                "ssl-key",
                "sslcert",
                "ssl-cert",
                "sslrootcert",
                "ssl-root-cert",
                "sslca",
                "ssl-ca",
                "socket",
            ],
        );
        secrets.add(&user);
        secrets.add(&host);
        secrets.add(&database);
        if let Some(password) = &password {
            secrets.add(password);
        }
        let (options, local) = match engine {
            SqlEngine::Postgres => {
                if let Some(variable) = postgres_environment_conflict(&environment) {
                    return Err(format!(
                        "{variable} is set; the Postgres connection must come only from the --sql-env variable, so unset it in the server's environment"
                    ));
                }
                // `new_without_pgpass` never reads the password file; with no
                // PG* variable set, every field below is the URL's own.
                let mut options = PgConnectOptions::new_without_pgpass()
                    .port(url.port().unwrap_or(5432))
                    .username(&user)
                    .database(&database)
                    .application_name("compresso");
                options = if host.starts_with('/') {
                    options.socket(&host)
                } else {
                    options.host(&host)
                };
                if let Some(password) = &password {
                    options = options.password(password);
                }
                if let Some(path) = param("sslrootcert") {
                    options = options.ssl_root_cert(path);
                }
                if let Some(path) = param("sslcert") {
                    options = options.ssl_client_cert(path);
                }
                if let Some(path) = param("sslkey") {
                    options = options.ssl_client_key(path);
                }
                // The policy reads the very object the driver connects with.
                let local = options.get_socket().is_some() || is_loopback_text(options.get_host());
                (DriverOptions::Postgres(options), local)
            }
            _ => {
                let mut options = MySqlConnectOptions::new()
                    .host(if host.is_empty() { "localhost" } else { &host })
                    .port(url.port().unwrap_or(3306))
                    .username(&user)
                    .database(&database)
                    .charset("utf8mb4")
                    // Keep the server's own `||` and time zone, so results
                    // match what the operator's own client shows.
                    .pipes_as_concat(false)
                    .timezone(None);
                if let Some(password) = &password {
                    options = options.password(password);
                }
                if let Some(socket) = &socket {
                    options = options.socket(socket);
                }
                if let Some(path) = param("sslrootcert") {
                    options = options.ssl_ca(path);
                }
                if let Some(path) = param("sslcert") {
                    options = options.ssl_client_cert(path);
                }
                if let Some(path) = param("sslkey") {
                    options = options.ssl_client_key(path);
                }
                let local = options.get_socket().is_some() || is_loopback_text(options.get_host());
                (DriverOptions::MySql(options), local)
            }
        };
        let tls = tls_policy(engine, local, mode)?;
        let options = match options {
            DriverOptions::Postgres(options) => {
                DriverOptions::Postgres(options.ssl_mode(match tls {
                    Tls::VerifyFull | Tls::Local(LocalTls::VerifyFull) => PgSslMode::VerifyFull,
                    Tls::Local(LocalTls::VerifyCa) => PgSslMode::VerifyCa,
                    Tls::Local(LocalTls::Require) => PgSslMode::Require,
                    Tls::Local(LocalTls::Prefer) => PgSslMode::Prefer,
                    Tls::Local(LocalTls::Disable) => PgSslMode::Disable,
                }))
            }
            DriverOptions::MySql(options) => DriverOptions::MySql(options.ssl_mode(match tls {
                Tls::VerifyFull | Tls::Local(LocalTls::VerifyFull) => MySqlSslMode::VerifyIdentity,
                Tls::Local(LocalTls::VerifyCa) => MySqlSslMode::VerifyCa,
                Tls::Local(LocalTls::Require) => MySqlSslMode::Required,
                Tls::Local(LocalTls::Prefer) => MySqlSslMode::Preferred,
                Tls::Local(LocalTls::Disable) => MySqlSslMode::Disabled,
            })),
        };
        Ok(Self::Network(
            Box::new(NetworkTarget {
                engine,
                options,
                tls,
            }),
            secrets,
        ))
    }

    pub(crate) fn engine(&self) -> SqlEngine {
        match self {
            Self::Sqlite(_) => SqlEngine::Sqlite,
            Self::Network(target, _) => target.engine,
        }
    }

    /// Removes every part of the configured URL from an error's message.
    pub(crate) fn scrub(&self, error: SqlError) -> SqlError {
        match self {
            Self::Sqlite(target) => SqlError::new(error.code(), target.scrub(error.message())),
            Self::Network(_, secrets) => {
                SqlError::new(error.code(), secrets.scrub(error.message()))
            }
        }
    }
}

/// A parameter name is echoed only when it is short and plain.
fn printable_key(key: &str) -> String {
    if key.len() <= 32
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        key.to_owned()
    } else {
        "(unprintable)".to_owned()
    }
}

/// Plain text only to a loopback address or a local socket; any other host
/// gets TLS with full certificate verification, and a weaker mode for such a
/// host is an error, not a silent upgrade.
pub(crate) fn tls_policy(
    engine: SqlEngine,
    local: bool,
    mode: Option<LocalTls>,
) -> Result<Tls, String> {
    if local {
        return Ok(Tls::Local(mode.unwrap_or(LocalTls::Prefer)));
    }
    match mode {
        None | Some(LocalTls::VerifyFull) => Ok(Tls::VerifyFull),
        Some(_) => Err(match engine {
            SqlEngine::Postgres => {
                "a non-loopback host needs sslmode=verify-full (or no sslmode); weaker modes are refused"
            }
            _ => {
                "a non-loopback host needs ssl-mode=VERIFY_IDENTITY (or no ssl-mode); weaker modes are refused"
            }
        }
        .to_owned()),
    }
}

fn parse_mode(engine: SqlEngine, mode: &str) -> Result<LocalTls, String> {
    let normalized = mode.to_ascii_lowercase().replace('_', "-");
    let parsed = match (engine, normalized.as_str()) {
        (SqlEngine::Postgres, "disable" | "allow") => LocalTls::Disable,
        (SqlEngine::Postgres, "prefer") => LocalTls::Prefer,
        (SqlEngine::Postgres, "require") => LocalTls::Require,
        (SqlEngine::Postgres, "verify-ca") => LocalTls::VerifyCa,
        (SqlEngine::Postgres, "verify-full") => LocalTls::VerifyFull,
        (SqlEngine::MySql, "disabled") => LocalTls::Disable,
        (SqlEngine::MySql, "preferred") => LocalTls::Prefer,
        (SqlEngine::MySql, "required") => LocalTls::Require,
        (SqlEngine::MySql, "verify-ca") => LocalTls::VerifyCa,
        (SqlEngine::MySql, "verify-identity") => LocalTls::VerifyFull,
        _ => return Err("the URL's TLS mode is not recognized".to_owned()),
    };
    Ok(parsed)
}

fn is_loopback_text(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(value: &str) -> Result<SqlTarget, String> {
        SqlTarget::parse_with_environment(value, Vec::new(), |_| None)
    }

    fn tls(value: &str) -> Result<Tls, String> {
        match parse(value)? {
            SqlTarget::Network(target, _) => Ok(target.tls),
            SqlTarget::Sqlite(_) => panic!("network"),
        }
    }

    #[test]
    fn test_env_names_must_carry_the_prefix_and_plain_characters() {
        assert!(is_valid_env_name("COMPRESSO_SQL_MAIN"));
        assert!(is_valid_env_name("COMPRESSO_SQL_DB_2"));
        for invalid in [
            "COMPRESSO_SQL_",
            "COMPRESSO_SQL_main",
            "DATABASE_URL",
            "COMPRESSO_SQL_A-B",
            "COMPRESSO_SQL_A B",
            "compresso_sql_main",
            "",
        ] {
            assert!(!is_valid_env_name(invalid), "{invalid}");
        }
    }

    #[test]
    fn test_remote_hosts_get_verified_tls_and_refuse_weaker_modes() {
        assert_eq!(tls("postgres://u@db.example.com/app"), Ok(Tls::VerifyFull));
        assert_eq!(
            tls("postgres://u@db.example.com/app?sslmode=verify-full"),
            Ok(Tls::VerifyFull)
        );
        for weak in ["disable", "allow", "prefer", "require", "verify-ca"] {
            assert!(
                tls(&format!("postgres://u@db.example.com/app?sslmode={weak}")).is_err(),
                "{weak}"
            );
        }
        assert_eq!(tls("mysql://u@10.0.0.5/app"), Ok(Tls::VerifyFull));
        assert_eq!(
            tls("mysql://u@10.0.0.5/app?ssl-mode=VERIFY_IDENTITY"),
            Ok(Tls::VerifyFull)
        );
        for weak in ["DISABLED", "PREFERRED", "REQUIRED", "VERIFY_CA"] {
            assert!(
                tls(&format!("mysql://u@10.0.0.5/app?ssl-mode={weak}")).is_err(),
                "{weak}"
            );
        }
    }

    #[test]
    fn test_loopback_and_sockets_keep_their_own_mode() {
        assert_eq!(
            tls("postgres://u@localhost/app"),
            Ok(Tls::Local(LocalTls::Prefer))
        );
        assert_eq!(
            tls("postgres://u@127.0.0.1:5433/app?sslmode=disable"),
            Ok(Tls::Local(LocalTls::Disable))
        );
        assert_eq!(
            tls("postgres://u@[::1]/app?sslmode=require"),
            Ok(Tls::Local(LocalTls::Require))
        );
        assert_eq!(
            tls("postgres://u@%2Ftmp%2Fpg/app"),
            Ok(Tls::Local(LocalTls::Prefer))
        );
        assert_eq!(
            tls("mysql://u@localhost/app?ssl-mode=DISABLED"),
            Ok(Tls::Local(LocalTls::Disable))
        );
        assert_eq!(
            tls("mysql://u@db.example.com/app?socket=/tmp/my.sock"),
            Ok(Tls::Local(LocalTls::Prefer))
        );
        // A host that merely starts with a loopback-looking label is remote.
        assert_eq!(
            tls("postgres://u@localhost.example.com/app"),
            Ok(Tls::VerifyFull)
        );
        assert_eq!(
            tls("postgres://u@127.0.0.1.nip.io/app"),
            Ok(Tls::VerifyFull)
        );
    }

    #[test]
    fn test_duplicate_or_aliased_parameters_cannot_downgrade_tls() {
        for value in [
            "postgres://u@localhost/app?host=localhost&host=db.remote&sslmode=disable",
            "postgres://u@db.remote/app?sslmode=verify-full&sslmode=disable",
            "postgres://u@db.remote/app?sslmode=verify-full&ssl-mode=disable",
            "mysql://u@db.remote/app?ssl-mode=VERIFY_IDENTITY&sslmode=DISABLED",
            "mysql://u@db.remote/app?socket=/tmp/a&socket=/tmp/b",
        ] {
            assert!(parse(value).is_err(), "{value}");
        }
    }

    #[test]
    fn test_only_understood_parameters_are_accepted() {
        for value in [
            "postgres://u@localhost/app?host=/tmp",
            "postgres://u@localhost/app?hostaddr=10.1.2.3",
            "postgres://u@localhost/app?options=-c%20x%3D1",
            "postgres://u@localhost/app?application_name=x",
            "postgres://u@localhost/app?port=1",
            "postgres://u@localhost/app?user=v",
            "postgres://u@localhost/app?dbname=other",
            "postgres://u@localhost/app?password=x",
            "mysql://u@localhost/app?charset=gbk",
            "mysql://u@localhost/app?timezone=%2B01:00",
            "mysql://u@localhost/app?statement-cache-capacity=1",
            "sqlite:/tmp/x.db?mode=rwc",
        ] {
            assert!(parse(value).is_err(), "{value}");
        }
        assert!(parse("postgres://u@localhost/app?sslmode=require&sslrootcert=/ca.pem").is_ok());
        assert!(parse("mysql://u@localhost/app?ssl-ca=/ca.pem&ssl-mode=REQUIRED").is_ok());
    }

    #[test]
    fn test_user_host_and_database_must_be_explicit() {
        for value in [
            "postgres:///app",
            "postgres://localhost/app",
            "postgres://u@localhost",
            "postgres://u@localhost/",
            "postgres://u@localhost/a/b",
            "mysql://localhost/app",
            "mysql://u@localhost",
        ] {
            assert!(parse(value).is_err(), "{value}");
        }
    }

    #[test]
    fn test_postgres_environment_variables_make_the_target_unusable() {
        let error =
            SqlTarget::parse_with_environment("postgres://u@localhost/app", Vec::new(), |name| {
                (name == "PGPASSFILE").then(String::new)
            })
            .expect_err("environment");
        assert!(error.starts_with("PGPASSFILE is set"), "{error}");
        for name in POSTGRES_ENVIRONMENT {
            assert_eq!(
                postgres_environment_conflict(|candidate| (candidate == *name).then(String::new)),
                Some(*name)
            );
        }
        // MySQL reads no environment, so the same variables do not matter.
        assert!(
            SqlTarget::parse_with_environment("mysql://u@localhost/app", Vec::new(), |_| Some(
                String::new()
            ))
            .is_ok()
        );
    }

    #[test]
    fn test_the_policy_reads_the_options_the_driver_uses() {
        let SqlTarget::Network(target, _) = parse("postgres://u:pw@localhost:6000/app").unwrap()
        else {
            panic!("network");
        };
        let DriverOptions::Postgres(options) = &target.options else {
            panic!("postgres");
        };
        assert_eq!(options.get_host(), "localhost");
        assert_eq!(options.get_port(), 6000);
        assert_eq!(options.get_username(), "u");
        assert_eq!(options.get_database(), Some("app"));
        assert_eq!(options.get_application_name(), Some("compresso"));
        assert_eq!(options.get_options(), None);
        let SqlTarget::Network(target, _) = parse("mysql://u@localhost/app").unwrap() else {
            panic!("network");
        };
        let DriverOptions::MySql(options) = &target.options else {
            panic!("mysql");
        };
        assert_eq!(options.get_charset(), "utf8mb4");
    }

    #[test]
    fn test_parse_refuses_unknown_parameters_and_schemes_without_echoing_values() {
        let canary = concat!("canary", "-", "pw-", "7731");
        for value in [
            format!("postgres://u:{canary}@localhost/app?options=-c%20x%3D1"),
            format!("redis://u:{canary}@localhost/0"),
            format!("postgres://u:{canary}@localhost/app#frag"),
            format!("not a url {canary}"),
            format!("postgres://u:{canary}@db.example.com/app?sslmode=disable"),
            format!("sqlite:relative/{canary}.db"),
        ] {
            let error = parse(&value).expect_err(&value);
            assert!(!error.contains(canary), "{error}");
        }
    }
}

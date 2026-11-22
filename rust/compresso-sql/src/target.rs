//! Parsing the operator's database URL and the transport-security policy.

use std::fmt;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use percent_encoding::percent_decode_str;
use url::{Host, Url};

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

/// A parsed network target. The URL itself stays private to the crate.
pub(crate) struct NetworkTarget {
    pub(crate) engine: SqlEngine,
    pub(crate) url: Url,
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
    Network(NetworkTarget, Secrets),
}

impl fmt::Debug for SqlTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.engine().name())
    }
}

/// Query parameters a Postgres URL may carry. Anything else (notably
/// `options`, which sets server parameters) is refused.
const POSTGRES_PARAMS: &[&str] = &[
    "sslmode",
    "ssl-mode",
    "sslrootcert",
    "ssl-root-cert",
    "sslcert",
    "ssl-cert",
    "sslkey",
    "ssl-key",
    "host",
    "hostaddr",
    "port",
    "dbname",
    "user",
    "password",
    "application_name",
];

/// Query parameters a MySQL/MariaDB URL may carry.
const MYSQL_PARAMS: &[&str] = &[
    "sslmode", "ssl-mode", "sslca", "ssl-ca", "sslcert", "ssl-cert", "sslkey", "ssl-key", "socket",
    "charset",
];

/// Query parameters whose values are secret or locate secrets.
const SECRET_PARAMS: &[&str] = &[
    "password",
    "user",
    "host",
    "hostaddr",
    "dbname",
    "socket",
    "sslkey",
    "ssl-key",
    "sslcert",
    "ssl-cert",
    "sslrootcert",
    "ssl-root-cert",
    "sslca",
    "ssl-ca",
];

impl SqlTarget {
    /// Parses one URL. The error never contains the URL or its parts.
    pub(crate) fn parse(value: &str, protected: Vec<PathBuf>) -> Result<Self, String> {
        let value = value.trim();
        if let Some(rest) = value.strip_prefix("sqlite:") {
            let path = rest.strip_prefix("//").unwrap_or(rest);
            if path.contains('\0') {
                return Err("the SQLite path contains a NUL byte".to_owned());
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
        let allowed = match engine {
            SqlEngine::Postgres => POSTGRES_PARAMS,
            _ => MYSQL_PARAMS,
        };
        for (key, _) in url.query_pairs() {
            if !allowed.contains(&key.as_ref()) {
                return Err(format!(
                    "the URL carries an unsupported parameter '{}'",
                    printable_key(&key)
                ));
            }
        }
        if url.fragment().is_some() {
            return Err("the URL must not carry a fragment".to_owned());
        }
        let tls = tls_policy(engine, &url)?;
        let secrets = Secrets::from_url(value, &url, SECRET_PARAMS);
        Ok(Self::Network(NetworkTarget { engine, url, tls }, secrets))
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

/// Decides the transport security of a network URL: plain text is allowed
/// only to a loopback address or a local socket, and any other host gets TLS
/// with full certificate verification; a weaker mode for such a host is an
/// error, not a silent upgrade.
pub(crate) fn tls_policy(engine: SqlEngine, url: &Url) -> Result<Tls, String> {
    let param = |name: &str| {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    };
    let mode = param("sslmode").or_else(|| param("ssl-mode"));
    let local = match engine {
        SqlEngine::Postgres => postgres_is_local(url, param("host"), param("hostaddr"))?,
        _ => mysql_is_local(url, param("socket"))?,
    };
    let parsed = match mode.as_deref() {
        None => None,
        Some(mode) => Some(parse_mode(engine, mode)?),
    };
    if local {
        return Ok(Tls::Local(parsed.unwrap_or(LocalTls::Prefer)));
    }
    match parsed {
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
        (_, "disabled") if engine != SqlEngine::Postgres => LocalTls::Disable,
        (_, "preferred") if engine != SqlEngine::Postgres => LocalTls::Prefer,
        (_, "required") if engine != SqlEngine::Postgres => LocalTls::Require,
        (_, "verify-ca") if engine != SqlEngine::Postgres => LocalTls::VerifyCa,
        (_, "verify-identity") if engine != SqlEngine::Postgres => LocalTls::VerifyFull,
        _ => return Err("the URL's TLS mode is not recognized".to_owned()),
    };
    Ok(parsed)
}

fn is_loopback_host(host: Option<Host<&str>>) -> bool {
    match host {
        // Non-special schemes keep an IPv4 host as text.
        Some(Host::Domain(domain)) => is_loopback_text(domain),
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

fn is_loopback_text(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

/// A Postgres URL is local when it names a socket directory (a host or
/// `host` parameter starting with `/`) or a loopback host, and any
/// `hostaddr` (from the URL or `PGHOSTADDR`, which the driver also reads) is
/// loopback too. A URL without a host is refused: the driver would fall back
/// to `PGHOST`, which this policy cannot see.
fn postgres_is_local(
    url: &Url,
    host_param: Option<String>,
    hostaddr_param: Option<String>,
) -> Result<bool, String> {
    let host = url
        .host_str()
        .map(|host| percent_decode_str(host).decode_utf8_lossy().into_owned());
    let named = host_param
        .clone()
        .or(host.clone())
        .filter(|host| !host.is_empty());
    let Some(named) = named else {
        return Err(
            "a postgres URL must name its host (or a host=/socket/dir parameter)".to_owned(),
        );
    };
    let hostaddr = hostaddr_param.or_else(|| std::env::var("PGHOSTADDR").ok());
    if let Some(hostaddr) = hostaddr
        && !is_loopback_text(&hostaddr)
    {
        return Ok(false);
    }
    if named.starts_with('/') {
        return Ok(true);
    }
    if host_param.is_some() {
        return Ok(is_loopback_text(&named));
    }
    Ok(is_loopback_host(url.host()))
}

/// A MySQL URL is local when it names a socket or a loopback host.
fn mysql_is_local(url: &Url, socket: Option<String>) -> Result<bool, String> {
    if socket.is_some_and(|socket| !socket.is_empty()) {
        return Ok(true);
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err("a mysql URL must name its host (or a socket parameter)".to_owned());
    }
    Ok(is_loopback_host(url.host()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tls(value: &str) -> Result<Tls, String> {
        let url = Url::parse(value).expect("url");
        let engine = if value.starts_with("mysql") {
            SqlEngine::MySql
        } else {
            SqlEngine::Postgres
        };
        tls_policy(engine, &url)
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
            tls("postgres://u@db.example.com/app?host=/tmp/pg&sslmode=disable"),
            Ok(Tls::Local(LocalTls::Disable))
        );
        assert_eq!(
            tls("mysql://u@localhost/app?ssl-mode=DISABLED"),
            Ok(Tls::Local(LocalTls::Disable))
        );
        assert_eq!(
            tls("mysql://u@db.example.com/app?socket=/tmp/my.sock"),
            Ok(Tls::Local(LocalTls::Prefer))
        );
    }

    #[test]
    fn test_a_remote_hostaddr_or_missing_host_is_not_local() {
        assert_eq!(
            tls("postgres://u@localhost/app?hostaddr=10.1.2.3"),
            Ok(Tls::VerifyFull)
        );
        assert!(tls("postgres://u@localhost/app?hostaddr=10.1.2.3&sslmode=disable").is_err());
        assert!(tls("postgres:///app").is_err());
        assert!(tls("mysql:///app").is_err());
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
            let error = SqlTarget::parse(&value, Vec::new()).expect_err(&value);
            assert!(!error.contains(canary), "{error}");
        }
        let error = SqlTarget::parse(
            &format!("postgres://u@localhost/app?{canary}=1"),
            Vec::new(),
        )
        .expect_err("unknown key");
        assert!(error.contains("unsupported parameter"), "{error}");
    }

    #[test]
    fn test_parse_recognizes_each_engine() {
        for (value, engine) in [
            ("postgres://u@localhost/app", SqlEngine::Postgres),
            ("postgresql://u@localhost/app", SqlEngine::Postgres),
            ("mysql://u@localhost/app", SqlEngine::MySql),
            ("mariadb://u@localhost/app", SqlEngine::MySql),
        ] {
            assert_eq!(
                SqlTarget::parse(value, Vec::new()).expect(value).engine(),
                engine
            );
        }
    }
}

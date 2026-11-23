//! The one MongoDB server a `compresso_mongo` process may reach (E17-52,
//! D117).
//!
//! The URL is parsed here and never handed to the driver's own parser: the
//! driver gets options built field by field. Accepted: `mongodb://`, exactly
//! one host, a user and a password, one database, and the URL options in
//! [`URL_OPTIONS`] (case-insensitive, each at most once). Refused:
//! `mongodb+srv://` (the driver would read hosts from SRV records and options
//! from a TXT record), several hosts (replica-set discovery would contact
//! hosts the URL does not name), any other option, a fragment, and the
//! `admin`, `local`, and `config` databases. The driver always gets a direct
//! connection to the one host. A host other than loopback always gets TLS
//! with certificate and host-name verification against the driver's bundled
//! root certificates, or only the `tlsCAFile` the URL names; there is no
//! option that weakens it.

use std::fmt;
use std::path::PathBuf;

use url::Url;

use crate::redact::Secrets;
use crate::target::{checked_pem, decoded, is_loopback_text};

/// Prefix every operator-chosen variable name carries.
pub const MONGO_ENV_PREFIX: &str = "COMPRESSO_MONGO_";

/// URL options accepted (compared without case), each at most once.
pub const URL_OPTIONS: &[&str] = &[
    "authMechanism",
    "authSource",
    "directConnection",
    "tls",
    "ssl",
    "tlsCAFile",
];

/// Variables that make the driver describe a function-as-a-service host in
/// its handshake, sending values from this process's environment (region,
/// memory, runtime) to the server. Any of them set (as the driver tests
/// them) makes the target unusable.
pub const MONGO_HANDSHAKE_ENVIRONMENT: &[&str] = &[
    "AWS_EXECUTION_ENV",
    "AWS_LAMBDA_RUNTIME_API",
    "VERCEL",
    "FUNCTIONS_WORKER_RUNTIME",
    "K_SERVICE",
    "FUNCTION_NAME",
];

/// Whether `name` is an acceptable `--mongo-env` variable name:
/// `COMPRESSO_MONGO_` followed by at least one of `A-Z`, `0-9`, `_`.
pub fn is_valid_mongo_env_name(name: &str) -> bool {
    name.strip_prefix(MONGO_ENV_PREFIX).is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    })
}

/// Whether the driver would treat `name` (holding `value`) as marking a
/// function-as-a-service host.
pub(crate) fn marks_handshake(name: &str, value: &str) -> bool {
    match name {
        "AWS_EXECUTION_ENV" => value.starts_with("AWS_Lambda_"),
        _ => !value.is_empty(),
    }
}

/// The SCRAM variants accepted for `authMechanism`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Mechanism {
    ScramSha256,
    ScramSha1,
}

/// Transport security for the one host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum MongoTls {
    /// Plain text: only to a loopback host whose URL does not ask for TLS.
    Off,
    /// TLS with certificate and host-name verification, against the bundled
    /// roots or only the named root certificate file.
    Verified { ca_file: Option<PathBuf> },
}

/// A parsed, checked target. `Debug` never prints a URL part.
#[derive(Clone)]
pub(crate) struct MongoTarget {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) username: String,
    pub(crate) password: String,
    pub(crate) database: String,
    pub(crate) auth_source: Option<String>,
    pub(crate) mechanism: Option<Mechanism>,
    pub(crate) tls: MongoTls,
    pub(crate) secrets: Secrets,
}

impl fmt::Debug for MongoTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MongoTarget")
            .field("tls", &matches!(self.tls, MongoTls::Verified { .. }))
            .finish_non_exhaustive()
    }
}

/// A database name MongoDB accepts and the tool targets: 1 to 63 bytes, none
/// of `/\. "$*<>:|?` or NUL.
fn valid_database(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && !name.chars().any(|character| {
            matches!(
                character,
                '/' | '\\' | '.' | ' ' | '"' | '$' | '*' | '<' | '>' | ':' | '|' | '?' | '\0'
            ) || character.is_control()
        })
}

impl MongoTarget {
    /// Parses `value`; the error never contains it or any part of it.
    /// `environment` names the [`MONGO_HANDSHAKE_ENVIRONMENT`] variables set.
    pub(crate) fn parse(value: &str, environment: &[String]) -> Result<Self, String> {
        if let Some(name) = environment.first() {
            return Err(format!(
                "{name} is set, so the driver would send values from this environment to the server in its handshake; start the server without it"
            ));
        }
        let lower = value.get(..14).map(str::to_ascii_lowercase);
        if lower.as_deref() == Some("mongodb+srv://") {
            return Err("mongodb+srv:// URLs are refused (the driver would read hosts from SRV records and options from a TXT record); give one host with mongodb://".to_owned());
        }
        if !value.starts_with("mongodb://") {
            return Err("the value must be a mongodb:// URL".to_owned());
        }
        let after_scheme = &value["mongodb://".len()..];
        let authority = after_scheme
            .split(['/', '?', '#'])
            .next()
            .unwrap_or_default();
        let hosts = authority
            .rsplit_once('@')
            .map_or(authority, |(_, hosts)| hosts);
        if hosts.contains(',') {
            return Err("the URL names several hosts; give exactly one host (the tool always connects directly to it, never to a replica-set member it discovers)".to_owned());
        }
        let url = Url::parse(value).map_err(|_| "the value is not a valid URL".to_owned())?;
        if url.fragment().is_some() {
            return Err("the URL may not have a fragment".to_owned());
        }
        let host = url
            .host_str()
            .filter(|host| !host.is_empty())
            .ok_or_else(|| "the URL must name a host".to_owned())?;
        let host = decoded(host, "host")?;
        if host.contains(',') {
            return Err("the URL names several hosts; give exactly one host".to_owned());
        }
        if host.contains('/') || host.ends_with(".sock") {
            return Err("local sockets are refused; give a host name or address".to_owned());
        }
        let host = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned();
        let port = url.port().unwrap_or(27017);
        let username = decoded(url.username(), "user name")?;
        if username.is_empty() {
            return Err(
                "the URL must name a user (the account check needs one authenticated user)"
                    .to_owned(),
            );
        }
        let password = decoded(url.password().unwrap_or_default(), "password")?;
        if password.is_empty() {
            return Err("the URL must carry the user's password".to_owned());
        }
        let path = url.path().strip_prefix('/').unwrap_or_default();
        let database = decoded(path, "database")?;
        if !valid_database(&database) {
            return Err("the URL must name one database after the host (1 to 63 characters, none of /\\. \"$*<>:|?)".to_owned());
        }
        if ["admin", "local", "config"].contains(&database.as_str()) {
            return Err(
                "the admin, local, and config databases are refused; name the data database"
                    .to_owned(),
            );
        }

        let mut seen: Vec<String> = Vec::new();
        let mut tls: Option<bool> = None;
        let mut ca_file = None;
        let mut auth_source = None;
        let mut mechanism = None;
        let mut secret_params = Vec::new();
        for (key, option) in url.query_pairs() {
            let lower = key.to_ascii_lowercase();
            let Some(canonical) = URL_OPTIONS
                .iter()
                .find(|known| known.eq_ignore_ascii_case(&lower))
            else {
                return Err(format!(
                    "the URL option {} is not accepted; accepted options: {}",
                    printable(&key),
                    URL_OPTIONS.join(", ")
                ));
            };
            // tls and ssl are one option.
            let slot = if lower == "ssl" {
                "tls".to_owned()
            } else {
                lower
            };
            if seen.contains(&slot) {
                return Err(format!(
                    "the URL option {canonical} is given more than once"
                ));
            }
            seen.push(slot);
            match *canonical {
                "tls" | "ssl" => {
                    tls = Some(match option.as_ref() {
                        "true" => true,
                        "false" => false,
                        _ => return Err("tls must be true or false".to_owned()),
                    });
                }
                "tlsCAFile" => {
                    secret_params.push(option.clone().into_owned());
                    ca_file = Some(checked_pem(&option, "tlsCAFile", "CERTIFICATE")?);
                }
                "authSource" => {
                    if !valid_database(&option) {
                        return Err("authSource must name a database".to_owned());
                    }
                    secret_params.push(option.clone().into_owned());
                    auth_source = Some(option.into_owned());
                }
                "authMechanism" => {
                    mechanism = Some(match option.as_ref() {
                        "SCRAM-SHA-256" => Mechanism::ScramSha256,
                        "SCRAM-SHA-1" => Mechanism::ScramSha1,
                        _ => {
                            return Err(
                                "authMechanism must be SCRAM-SHA-256 or SCRAM-SHA-1".to_owned()
                            );
                        }
                    });
                }
                "directConnection" => {
                    if option != "true" {
                        return Err("directConnection may only be true (the tool always connects directly to the one host)".to_owned());
                    }
                }
                _ => unreachable!("every accepted option is handled"),
            }
        }
        let tls = if is_loopback_text(&host) {
            match (tls, &ca_file) {
                (Some(false), Some(_)) => {
                    return Err("tlsCAFile needs TLS; drop tls=false".to_owned());
                }
                (Some(true), _) | (None, Some(_)) => MongoTls::Verified { ca_file },
                _ => MongoTls::Off,
            }
        } else {
            if tls == Some(false) {
                return Err("TLS cannot be turned off for a host other than loopback".to_owned());
            }
            MongoTls::Verified { ca_file }
        };
        let mut secrets = Secrets::from_url(value, &url, &[]);
        secrets.add(&username);
        secrets.add(&password);
        secrets.add(&host);
        secrets.add(&database);
        for param in &secret_params {
            secrets.add(param);
        }
        Ok(Self {
            host,
            port,
            username,
            password,
            database,
            auth_source,
            mechanism,
            tls,
            secrets,
        })
    }
}

/// An option name as shown in an error: printable ASCII up to 40 characters,
/// otherwise a placeholder.
fn printable(key: &str) -> String {
    if !key.is_empty()
        && key.len() <= 40
        && key
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        key.to_owned()
    } else {
        "(unprintable)".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWORD: &str = concat!("s3cr", "et-pw");

    fn url(rest: &str) -> String {
        format!("mongodb://reader:{PASSWORD}@{rest}")
    }

    fn parse(value: &str) -> Result<MongoTarget, String> {
        MongoTarget::parse(value, &[])
    }

    fn refused(value: &str) -> String {
        let error = parse(value).expect_err("refused");
        assert!(!error.contains(PASSWORD), "{error}");
        assert!(!error.contains("db.example"), "{error}");
        error
    }

    #[test]
    fn test_env_names_must_carry_the_prefix_and_plain_characters() {
        assert!(is_valid_mongo_env_name("COMPRESSO_MONGO_MAIN"));
        assert!(is_valid_mongo_env_name("COMPRESSO_MONGO_DB_2"));
        for bad in [
            "COMPRESSO_MONGO_",
            "COMPRESSO_SQL_MAIN",
            "MONGO_URL",
            "COMPRESSO_MONGO_a",
            "mongodb://x",
        ] {
            assert!(!is_valid_mongo_env_name(bad), "{bad}");
        }
    }

    #[test]
    fn test_one_loopback_host_parses_with_plain_text_and_remote_hosts_get_tls() {
        let target = parse(&url("127.0.0.1:27117/probe")).expect("valid");
        assert_eq!(target.host, "127.0.0.1");
        assert_eq!(target.port, 27117);
        assert_eq!(target.database, "probe");
        assert_eq!(target.username, "reader");
        assert_eq!(target.tls, MongoTls::Off);
        let target = parse(&url("[::1]/probe")).expect("valid");
        assert_eq!(target.host, "::1");
        assert_eq!(target.port, 27017);
        let target = parse(&url("localhost/probe?tls=true")).expect("valid");
        assert_eq!(target.tls, MongoTls::Verified { ca_file: None });

        let target = parse(&url("db.example.com/probe")).expect("valid");
        assert_eq!(target.tls, MongoTls::Verified { ca_file: None });
        let target = parse(&url("db.example.com/probe?tls=true&authSource=admin&authMechanism=SCRAM-SHA-256&directConnection=true")).expect("valid");
        assert_eq!(target.auth_source.as_deref(), Some("admin"));
        assert_eq!(target.mechanism, Some(Mechanism::ScramSha256));
        let error = refused(&url("db.example.com/probe?tls=false"));
        assert!(error.contains("TLS cannot be turned off"), "{error}");
        let error = refused(&url("db.example.com/probe?ssl=false"));
        assert!(error.contains("TLS cannot be turned off"), "{error}");
        assert!(format!("{target:?}").find(PASSWORD).is_none());
    }

    #[test]
    fn test_srv_and_several_hosts_are_refused() {
        let error = refused(&format!(
            "mongodb+srv://reader:{PASSWORD}@cluster.db.example.com/probe"
        ));
        assert!(error.contains("mongodb+srv://"), "{error}");
        let error = refused(&format!(
            "MONGODB+SRV://reader:{PASSWORD}@cluster.db.example.com/probe"
        ));
        assert!(error.contains("mongodb+srv://"), "{error}");
        let error = refused(&url("a.db.example.com,b.db.example.com/probe"));
        assert!(error.contains("several hosts"), "{error}");
        let error = refused(&url(
            "a.db.example.com:1,b.db.example.com:2/probe?replicaSet=rs0",
        ));
        assert!(error.contains("several hosts"), "{error}");
        assert!(
            refused(&url("a.db.example.com%2Cb.db.example.com/probe")).contains("several hosts")
        );
        assert!(refused("postgres://u:p@h/db").contains("mongodb://"));
    }

    #[test]
    fn test_only_listed_options_once_each() {
        for option in [
            "tlsInsecure=true",
            "tlsAllowInvalidCertificates=true",
            "tlsAllowInvalidHostnames=true",
            "tlsCertificateKeyFile=/tmp/x.pem",
            "replicaSet=rs0",
            "readPreference=secondary",
            "compressors=zlib",
            "loadBalanced=true",
            "srvServiceName=x",
            "proxyHost=evil",
            "authMechanismProperties=x:y",
            "w=0",
            "appName=x",
            "TLSINSECURE=true",
        ] {
            let error = refused(&url(&format!("db.example.com/probe?{option}")));
            assert!(error.contains("not accepted"), "{option}: {error}");
        }
        for duplicated in [
            "tls=true&tls=true",
            "tls=true&ssl=true",
            "tls=true&TLS=true",
            "authSource=admin&authsource=probe",
            "t%6Cs=true&tls=true",
        ] {
            let error = refused(&url(&format!("db.example.com/probe?{duplicated}")));
            assert!(error.contains("more than once"), "{duplicated}: {error}");
        }
        assert!(
            refused(&url("db.example.com/probe?directConnection=false"))
                .contains("directConnection")
        );
        assert!(
            refused(&url("db.example.com/probe?authMechanism=PLAIN")).contains("authMechanism")
        );
        assert!(
            refused(&url("db.example.com/probe?authMechanism=MONGODB-X509"))
                .contains("authMechanism")
        );
        assert!(refused(&url("db.example.com/probe?tls=yes")).contains("tls must be"));
        assert!(refused(&url("db.example.com/probe?tlsCAFile=relative.pem")).contains("tlsCAFile"));
        assert!(refused(&url("db.example.com/probe#x")).contains("fragment"));
    }

    #[test]
    fn test_a_private_root_certificate_is_read_only_through_a_checked_option() {
        let directory = tempfile::tempdir().expect("temp dir");
        let pem = directory.path().join("ca.pem");
        std::fs::write(
            &pem,
            "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
        )
        .unwrap();
        let target = parse(&url(&format!(
            "db.example.com/probe?tlsCAFile={}",
            pem.display()
        )))
        .expect("valid");
        assert_eq!(
            target.tls,
            MongoTls::Verified {
                ca_file: Some(pem.clone())
            }
        );
        let target = parse(&url(&format!(
            "localhost/probe?tlsCAFile={}",
            pem.display()
        )))
        .expect("valid");
        assert!(matches!(
            target.tls,
            MongoTls::Verified { ca_file: Some(_) }
        ));
        assert!(
            refused(&url(&format!(
                "localhost/probe?tls=false&tlsCAFile={}",
                pem.display()
            )))
            .contains("needs TLS")
        );
        let key = directory.path().join("key.pem");
        std::fs::write(
            &key,
            "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n",
        )
        .unwrap();
        assert!(
            refused(&url(&format!(
                "db.example.com/probe?tlsCAFile={}",
                key.display()
            )))
            .contains("tlsCAFile")
        );
    }

    #[test]
    fn test_user_password_and_database_must_be_explicit() {
        assert!(refused("mongodb://db.example.com/probe").contains("user"));
        assert!(refused("mongodb://reader@db.example.com/probe").contains("password"));
        assert!(refused(&url("db.example.com")).contains("database"));
        assert!(refused(&url("db.example.com/")).contains("database"));
        assert!(refused(&url("db.example.com/a.b")).contains("database"));
        for system in ["admin", "local", "config"] {
            assert!(refused(&url(&format!("db.example.com/{system}"))).contains("refused"));
        }
        assert!(refused(&url("%2Ftmp%2Fmongodb-27017.sock/probe")).contains("socket"));
        assert!(refused(&url("mongodb-27017.sock/probe")).contains("socket"));
    }

    #[test]
    fn test_function_host_variables_make_the_target_unusable() {
        let error = MongoTarget::parse(&url("localhost/probe"), &["VERCEL".to_owned()])
            .expect_err("refused");
        assert!(error.contains("VERCEL"), "{error}");
        assert!(marks_handshake("AWS_EXECUTION_ENV", "AWS_Lambda_java17"));
        assert!(!marks_handshake("AWS_EXECUTION_ENV", "AWS_ECS_FARGATE"));
        assert!(marks_handshake("K_SERVICE", "svc"));
        assert!(!marks_handshake("K_SERVICE", ""));
    }

    #[test]
    fn test_errors_scrub_every_part() {
        let target = parse(&url("db.example.com:27018/probe_db?authSource=authdb")).expect("valid");
        let message = format!(
            "connect to db.example.com:27018 as reader with {PASSWORD} on probe_db via authdb failed: mongodb://reader:{PASSWORD}@db.example.com"
        );
        let scrubbed = target.secrets.scrub(&message);
        for part in [PASSWORD, "db.example.com", "probe_db", "authdb", "reader"] {
            assert!(!scrubbed.contains(part), "{part}: {scrubbed}");
        }
        // A secret where the length cut falls is scrubbed before the cut, so
        // no prefix of it survives.
        for padding in [195, 595, 598] {
            let message = format!("{}{PASSWORD} tail", "x".repeat(padding));
            let scrubbed = target.secrets.scrub(&message);
            assert!(!scrubbed.contains(&PASSWORD[..4]), "{padding}: {scrubbed}");
        }
    }
}

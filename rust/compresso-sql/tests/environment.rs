//! Environment variables that would configure a connection, read from the
//! real process environment. One test in its own binary, because it sets
//! and removes variables of this process.

use compresso_sql::{
    POSTGRES_ENVIRONMENT, POSTGRES_UNOVERRIDABLE, SqlEngine, SqlService, SqlSettings,
    TRUST_ANCHOR_ENVIRONMENT,
};

const ENV: &str = "COMPRESSO_SQL_TEST";
const POSTGRES: &str = "postgres://u:p@127.0.0.1:5432/app?sslmode=disable";
const MYSQL: &str = "mysql://u:p@127.0.0.1:3306/app?ssl-mode=DISABLED";

fn set(name: &str, value: &str) {
    // SAFETY: this binary runs this single test; no other thread reads or
    // writes the environment while it runs.
    unsafe { std::env::set_var(name, value) };
}

fn unset(name: &str) {
    // SAFETY: as in `set`.
    unsafe { std::env::remove_var(name) };
}

fn problem(settings: &SqlSettings) -> Option<String> {
    SqlService::new(settings, Vec::new())
        .configuration_problem()
        .map(str::to_owned)
}

#[test]
fn test_connection_settings_come_only_from_the_named_variable() {
    // Start from a known environment whatever the machine sets (GitHub's
    // Windows image sets PGUSER, PGPASSWORD, PGROOT, PGDATA, PGBIN, ...).
    for name in POSTGRES_ENVIRONMENT
        .iter()
        .chain(POSTGRES_UNOVERRIDABLE)
        .chain(TRUST_ANCHOR_ENVIRONMENT)
    {
        unset(name);
    }
    // Server-side and tooling variables the driver never reads change
    // nothing.
    for name in ["PGDATA", "PGBIN", "PGROOT", "PGLOCALEDIR", "PGPASSFILE"] {
        set(name, "/somewhere");
    }
    set(ENV, POSTGRES);
    assert_eq!(problem(&SqlSettings::from_env(ENV)), None);
    // from_env records every PG* variable that is set, by name.
    set("PGAPPNAME", "probe");
    set(ENV, POSTGRES);
    let settings = SqlSettings::from_env(ENV);
    assert!(format!("{settings:?}").contains("PGAPPNAME"));
    assert!(problem(&settings).is_some_and(|reason| reason.starts_with("PGAPPNAME is set")));
    unset("PGAPPNAME");
    assert_eq!(problem(&SqlSettings::from_env(ENV)), None);

    // Variables the driver reads and no option can override refuse even
    // settings built with an explicit value.
    set("PGOPTIONS", "-c default_transaction_read_only=off");
    let explicit = SqlSettings::with_value(ENV, Some(POSTGRES));
    assert!(problem(&explicit).is_some_and(|reason| reason.starts_with("PGOPTIONS is set")));
    unset("PGOPTIONS");
    assert_eq!(problem(&explicit), None);

    // The native certificate loader reads SSL_CERT_FILE and SSL_CERT_DIR;
    // they refuse only a connection that verifies certificates.
    for name in ["SSL_CERT_FILE", "SSL_CERT_DIR"] {
        set(name, "/tmp/elsewhere");
        for url in [
            "postgres://u:p@db.example.com:5432/app",
            "mysql://u:p@db.example.com:3306/app",
        ] {
            set(ENV, url);
            let reason = problem(&SqlSettings::from_env(ENV));
            assert!(
                reason
                    .as_deref()
                    .is_some_and(|reason| reason.starts_with(&format!("{name} is set"))),
                "{url}: {reason:?}"
            );
        }
        for url in [POSTGRES, MYSQL] {
            set(ENV, url);
            assert_eq!(problem(&SqlSettings::from_env(ENV)), None, "{url}");
        }
        unset(name);
    }
    // An empty SSL_CERT_DIR names no directory: the certificate loader
    // ignores it, and so does the rule.
    set("SSL_CERT_DIR", "");
    set(ENV, "postgres://u:p@db.example.com:5432/app");
    assert_eq!(problem(&SqlSettings::from_env(ENV)), None);
    unset("SSL_CERT_DIR");
    let service = SqlService::new(&SqlSettings::with_value(ENV, Some(MYSQL)), Vec::new());
    assert_eq!(service.engine(), Some(SqlEngine::MySql));
    unset(ENV);
}

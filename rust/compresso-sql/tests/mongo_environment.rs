//! E17-52 (D117): the variables that make the MongoDB driver describe a
//! function-as-a-service host in its handshake, read from the real process
//! environment as the driver tests them. One test in its own binary, because
//! it sets and removes variables of this process.

use compresso_sql::mongo::{MONGO_HANDSHAKE_ENVIRONMENT, MongoService, MongoSettings};

const ENV: &str = "COMPRESSO_MONGO_TEST";

fn set(name: &str, value: &str) {
    // SAFETY: this binary runs this single test; no other thread reads or
    // writes the environment while it runs.
    unsafe { std::env::set_var(name, value) };
}

fn unset(name: &str) {
    // SAFETY: as in `set`.
    unsafe { std::env::remove_var(name) };
}

fn problem() -> Option<String> {
    set(ENV, "mongodb://reader:pw@127.0.0.1:27017/app");
    MongoService::new(&MongoSettings::from_env(ENV))
        .configuration_problem()
        .map(str::to_owned)
}

#[test]
fn test_function_host_variables_are_read_as_the_driver_reads_them() {
    for name in MONGO_HANDSHAKE_ENVIRONMENT {
        unset(name);
    }
    assert_eq!(problem(), None);
    // Set but empty, or an AWS environment that is not Lambda: the driver
    // ignores them, and so does the tool.
    set("K_SERVICE", "");
    set("AWS_EXECUTION_ENV", "AWS_ECS_FARGATE");
    assert_eq!(problem(), None);
    for (name, value) in [
        ("AWS_EXECUTION_ENV", "AWS_Lambda_java17"),
        ("AWS_LAMBDA_RUNTIME_API", "127.0.0.1:9001"),
        ("VERCEL", "1"),
        ("FUNCTIONS_WORKER_RUNTIME", "node"),
        ("K_SERVICE", "svc"),
        ("FUNCTION_NAME", "f"),
    ] {
        for other in MONGO_HANDSHAKE_ENVIRONMENT {
            unset(other);
        }
        set(name, value);
        let problem = problem().unwrap_or_else(|| panic!("{name} accepted"));
        assert!(problem.contains(name), "{problem}");
        assert!(value.len() < 4 || !problem.contains(value), "{problem}");
    }
    for name in MONGO_HANDSHAKE_ENVIRONMENT {
        unset(name);
    }
    unset(ENV);
}

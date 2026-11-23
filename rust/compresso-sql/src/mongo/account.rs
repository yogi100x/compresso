//! The account check every `compresso_mongo` call runs first (E17-52, D117).
//!
//! MongoDB has no read-only session: what the account may do is the control.
//! Before any other command, the call runs `connectionStatus` with
//! `showPrivileges` and refuses, failing closed, unless:
//!
//! 1. exactly one user is authenticated (a server with authentication off
//!    reports none, and would otherwise pass every privilege rule);
//! 2. every role the server reports that carries a built-in role's name is
//!    `read` on the configured database (custom roles are judged by their
//!    privileges below);
//! 3. every privilege's resource is a collection resource of the configured
//!    database (`{db: <configured>, collection: <any>}`), never the cluster,
//!    any resource, any database, or another database;
//! 4. every action of every privilege is one the built-in `read` role grants
//!    on MongoDB 8.0 ([`READ_ACTIONS`], taken from a live server);
//! 5. the server enforces those privileges: `hostInfo` (a read-only command
//!    that needs a cluster action no accepted account holds) must fail with
//!    `Unauthorized`. A server running without access control still lets a
//!    client authenticate and reports the user's privileges, but enforces
//!    none of them; this probe is what catches it.
//!
//! Anything the check cannot read is a refusal. A refusal names the rule,
//! never a user, role, database, or action value.

use bson::{Bson, Document};

/// The actions the built-in `read` role grants on one database, as MongoDB
/// 8.0.38 reports them through `connectionStatus` for a user holding only
/// `read` on that database.
pub const READ_ACTIONS: &[&str] = &[
    "changeStream",
    "collStats",
    "dbHash",
    "dbStats",
    "find",
    "killCursors",
    "listCollections",
    "listIndexes",
    "listSearchIndexes",
    "planCacheRead",
];

/// Built-in role names. Only `read` on the configured database is accepted;
/// any other, on any database, refuses the account.
pub const BUILT_IN_ROLES: &[&str] = &[
    "__queryableBackup",
    "__system",
    "backup",
    "clusterAdmin",
    "clusterManager",
    "clusterMonitor",
    "dbAdmin",
    "dbAdminAnyDatabase",
    "dbOwner",
    "directShardOperations",
    "enableSharding",
    "hostManager",
    "read",
    "readAnyDatabase",
    "readWrite",
    "readWriteAnyDatabase",
    "restore",
    "root",
    "searchCoordinator",
    "userAdmin",
    "userAdminAnyDatabase",
];

/// The rule an account broke, as shown to the caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AccountRule {
    Unreadable,
    NotOneUser,
    BuiltInRole,
    ResourceOutsideDatabase,
    ActionBeyondRead,
    AuthorizationNotEnforced,
}

/// The probe for rule 5: a read-only command that needs the cluster action
/// `hostInfo`, run on `admin`.
pub(crate) const ENFORCEMENT_PROBE: &str = "hostInfo";

/// Judges the probe's outcome: only `Unauthorized` (code 13) passes.
pub(crate) fn check_enforcement(outcome: Result<(), Option<i32>>) -> Result<(), AccountRule> {
    match outcome {
        Err(Some(13)) => Ok(()),
        Ok(()) => Err(AccountRule::AuthorizationNotEnforced),
        Err(_) => Err(AccountRule::Unreadable),
    }
}

impl AccountRule {
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::Unreadable => {
                "the server's connectionStatus answer could not be read, so the account is refused (fail closed)."
            }
            Self::NotOneUser => {
                "the server reports no single authenticated user (is authentication off?); the tool needs a user with only the read role on the configured database."
            }
            Self::BuiltInRole => {
                "the account holds a built-in role other than read on the configured database; give the tool a user with only that role."
            }
            Self::ResourceOutsideDatabase => {
                "the account holds a privilege outside the configured database's collections (cluster, any resource, or another database); give the tool a user with only read on the configured database."
            }
            Self::ActionBeyondRead => {
                "the account holds an action the read role does not grant (a write, admin, or cluster action); give the tool a user with only read on the configured database."
            }
            Self::AuthorizationNotEnforced => {
                "the server ran a command the account's privileges do not allow, so it does not enforce access control; the tool needs a server started with authorization on."
            }
        }
    }
}

/// Checks one `connectionStatus` answer for the configured `database`.
pub(crate) fn check(answer: &Document, database: &str) -> Result<(), AccountRule> {
    let info = answer
        .get_document("authInfo")
        .map_err(|_| AccountRule::Unreadable)?;
    let users = info
        .get_array("authenticatedUsers")
        .map_err(|_| AccountRule::Unreadable)?;
    if users.len() != 1 {
        return Err(AccountRule::NotOneUser);
    }
    let Some(Bson::Document(user)) = users.first() else {
        return Err(AccountRule::Unreadable);
    };
    if user.get_str("user").is_err() || user.get_str("db").is_err() {
        return Err(AccountRule::Unreadable);
    }
    let roles = info
        .get_array("authenticatedUserRoles")
        .map_err(|_| AccountRule::Unreadable)?;
    for role in roles {
        let Bson::Document(role) = role else {
            return Err(AccountRule::Unreadable);
        };
        let (Ok(name), Ok(db)) = (role.get_str("role"), role.get_str("db")) else {
            return Err(AccountRule::Unreadable);
        };
        if BUILT_IN_ROLES.contains(&name) && !(name == "read" && db == database) {
            return Err(AccountRule::BuiltInRole);
        }
    }
    let privileges = info
        .get_array("authenticatedUserPrivileges")
        .map_err(|_| AccountRule::Unreadable)?;
    for privilege in privileges {
        let Bson::Document(privilege) = privilege else {
            return Err(AccountRule::Unreadable);
        };
        let resource = privilege
            .get_document("resource")
            .map_err(|_| AccountRule::Unreadable)?;
        let in_database = resource.len() == 2
            && resource.get_str("db").ok() == Some(database)
            && resource.get_str("collection").is_ok();
        if !in_database {
            return Err(AccountRule::ResourceOutsideDatabase);
        }
        let actions = privilege
            .get_array("actions")
            .map_err(|_| AccountRule::Unreadable)?;
        for action in actions {
            let Bson::String(action) = action else {
                return Err(AccountRule::Unreadable);
            };
            if !READ_ACTIONS.contains(&action.as_str()) {
                return Err(AccountRule::ActionBeyondRead);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    /// The answer MongoDB 8.0.38 gave for a user with only `read` on `probe`.
    fn read_user() -> Document {
        doc! {
            "authInfo": {
                "authenticatedUsers": [{"user": "reader", "db": "probe"}],
                "authenticatedUserRoles": [{"role": "read", "db": "probe"}],
                "authenticatedUserPrivileges": [
                    {"resource": {"db": "probe", "collection": "system.js"}, "actions": READ_ACTIONS.to_vec()},
                    {"resource": {"db": "probe", "collection": ""}, "actions": READ_ACTIONS.to_vec()},
                ],
            },
            "ok": 1,
        }
    }

    fn with_info(edit: impl FnOnce(&mut Document)) -> Document {
        let mut answer = read_user();
        let info = answer.get_document_mut("authInfo").unwrap();
        edit(info);
        answer
    }

    #[test]
    fn test_a_read_user_on_the_configured_database_passes() {
        assert_eq!(check(&read_user(), "probe"), Ok(()));
        // A read user defined in admin, with read on the configured database.
        let answer = with_info(|info| {
            info.insert(
                "authenticatedUsers",
                vec![Bson::Document(doc! {"user": "r", "db": "admin"})],
            );
        });
        assert_eq!(check(&answer, "probe"), Ok(()));
    }

    #[test]
    fn test_no_user_or_several_users_is_refused() {
        // Authentication off: no users, no roles, no privileges.
        let off = doc! {"authInfo": {"authenticatedUsers": [], "authenticatedUserRoles": [], "authenticatedUserPrivileges": []}, "ok": 1};
        assert_eq!(check(&off, "probe"), Err(AccountRule::NotOneUser));
        let two = with_info(|info| {
            info.insert(
                "authenticatedUsers",
                vec![
                    Bson::Document(doc! {"user": "a", "db": "probe"}),
                    Bson::Document(doc! {"user": "b", "db": "probe"}),
                ],
            );
        });
        assert_eq!(check(&two, "probe"), Err(AccountRule::NotOneUser));
    }

    #[test]
    fn test_built_in_roles_beyond_read_on_the_database_are_refused() {
        for (role, db) in [
            ("readWrite", "probe"),
            ("dbOwner", "probe"),
            ("dbAdmin", "probe"),
            ("userAdmin", "probe"),
            ("read", "other"),
            ("readAnyDatabase", "admin"),
            ("root", "admin"),
            ("clusterMonitor", "admin"),
            ("backup", "admin"),
        ] {
            let answer = with_info(|info| {
                info.insert(
                    "authenticatedUserRoles",
                    vec![
                        Bson::Document(doc! {"role": "read", "db": "probe"}),
                        Bson::Document(doc! {"role": role, "db": db}),
                    ],
                );
            });
            assert_eq!(
                check(&answer, "probe"),
                Err(AccountRule::BuiltInRole),
                "{role}@{db}"
            );
        }
        // A custom role is judged by its privileges.
        let custom = with_info(|info| {
            info.insert(
                "authenticatedUserRoles",
                vec![Bson::Document(doc! {"role": "reporting", "db": "probe"})],
            );
        });
        assert_eq!(check(&custom, "probe"), Ok(()));
    }

    #[test]
    fn test_privileges_outside_the_database_or_beyond_read_are_refused() {
        for resource in [
            doc! {"cluster": true},
            doc! {"anyResource": true},
            doc! {"db": "", "collection": ""},
            doc! {"db": "other", "collection": ""},
            doc! {"db": "probe"},
            doc! {"db": "probe", "collection": "c", "extra": 1},
            doc! {"db": "probe", "system_buckets": ""},
        ] {
            let answer = with_info(|info| {
                info.insert(
                    "authenticatedUserPrivileges",
                    vec![Bson::Document(
                        doc! {"resource": resource.clone(), "actions": ["find"]},
                    )],
                );
            });
            assert_eq!(
                check(&answer, "probe"),
                Err(AccountRule::ResourceOutsideDatabase),
                "{resource}"
            );
        }
        for action in [
            "insert",
            "update",
            "remove",
            "createCollection",
            "dropCollection",
            "createIndex",
            "killop",
            "inprog",
            "anyAction",
            "bypassDocumentValidation",
        ] {
            let answer = with_info(|info| {
                info.insert(
                    "authenticatedUserPrivileges",
                    vec![Bson::Document(doc! {"resource": {"db": "probe", "collection": ""}, "actions": ["find", action]})],
                );
            });
            assert_eq!(
                check(&answer, "probe"),
                Err(AccountRule::ActionBeyondRead),
                "{action}"
            );
        }
    }

    #[test]
    fn test_unreadable_answers_fail_closed() {
        for answer in [
            doc! {"ok": 1},
            doc! {"authInfo": {"authenticatedUsers": [{"user": "r", "db": "probe"}]}},
            doc! {"authInfo": {"authenticatedUsers": [{"user": "r", "db": "probe"}], "authenticatedUserRoles": []}},
            doc! {"authInfo": {"authenticatedUsers": ["r"], "authenticatedUserRoles": [], "authenticatedUserPrivileges": []}},
            doc! {"authInfo": {"authenticatedUsers": [{"user": "r", "db": "probe"}], "authenticatedUserRoles": ["read"], "authenticatedUserPrivileges": []}},
            doc! {"authInfo": {"authenticatedUsers": [{"user": "r", "db": "probe"}], "authenticatedUserRoles": [], "authenticatedUserPrivileges": [{"actions": ["find"]}]}},
            doc! {"authInfo": {"authenticatedUsers": [{"user": "r", "db": "probe"}], "authenticatedUserRoles": [], "authenticatedUserPrivileges": [{"resource": {"db": "probe", "collection": ""}, "actions": [1]}]}},
        ] {
            assert_eq!(
                check(&answer, "probe"),
                Err(AccountRule::Unreadable),
                "{answer}"
            );
        }
        for rule in [
            AccountRule::Unreadable,
            AccountRule::NotOneUser,
            AccountRule::BuiltInRole,
            AccountRule::ResourceOutsideDatabase,
            AccountRule::ActionBeyondRead,
            AccountRule::AuthorizationNotEnforced,
        ] {
            assert!(!rule.message().contains("probe"));
        }
    }

    #[test]
    fn test_only_an_unauthorized_probe_shows_enforcement() {
        assert_eq!(check_enforcement(Err(Some(13))), Ok(()));
        assert_eq!(
            check_enforcement(Ok(())),
            Err(AccountRule::AuthorizationNotEnforced)
        );
        assert_eq!(
            check_enforcement(Err(Some(59))),
            Err(AccountRule::Unreadable)
        );
        assert_eq!(check_enforcement(Err(None)), Err(AccountRule::Unreadable));
    }
}

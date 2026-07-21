use rusqlite::params;

use crate::files::validate_nonempty;
use crate::{Store, StoreError};

/// SQLite identifier for a persisted integration receipt.
pub type ReceiptId = i64;

/// One integration configuration mutation receipt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReceiptRecord {
    /// `None` creates a row; records loaded from Store carry `Some(id)` and may be updated.
    pub id: Option<ReceiptId>,
    /// Integration client identifier: Claude Code, Codex, or Cursor.
    pub client: String,
    /// Exact configuration path mutated by the integration.
    pub config_path: String,
    /// Ordered key paths owned by Compresso.
    pub keys_owned: Vec<String>,
    /// Backup path or the documented no-prior-file sentinel.
    pub backup_path: String,
    /// Unix installation timestamp in seconds.
    pub installed_at: i64,
    /// Version of Compresso that performed the install.
    pub compresso_version: String,
    /// Unix uninstall timestamp, when the owned keys were removed.
    pub uninstalled_at: Option<i64>,
}

impl Store {
    /// Inserts a new receipt or updates the existing row identified by `receipt.id`.
    pub fn save_receipt(&self, receipt: &ReceiptRecord) -> Result<(), StoreError> {
        validate_receipt(receipt)?;
        let keys_owned = serde_json::to_string(&receipt.keys_owned).map_err(|source| {
            StoreError::JsonEncoding {
                field: "integration_receipts.keys_owned",
                source,
            }
        })?;
        let connection = self.connection()?;
        if let Some(id) = receipt.id {
            let changed = connection.execute(
                "UPDATE integration_receipts SET client = ?1, config_path = ?2, keys_owned = ?3, backup_path = ?4, installed_at = ?5, compresso_version = ?6, uninstalled_at = ?7 WHERE id = ?8",
                params![
                    &receipt.client,
                    &receipt.config_path,
                    keys_owned,
                    &receipt.backup_path,
                    receipt.installed_at,
                    &receipt.compresso_version,
                    receipt.uninstalled_at,
                    id,
                ],
            )?;
            if changed == 0 {
                return Err(StoreError::ReceiptNotFound(id));
            }
        } else {
            connection.execute(
                "INSERT INTO integration_receipts (client, config_path, keys_owned, backup_path, installed_at, compresso_version, uninstalled_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    &receipt.client,
                    &receipt.config_path,
                    keys_owned,
                    &receipt.backup_path,
                    receipt.installed_at,
                    &receipt.compresso_version,
                    receipt.uninstalled_at,
                ],
            )?;
        }
        Ok(())
    }

    /// Returns receipts in deterministic insertion order, optionally filtered by client.
    pub fn receipts(&self, client: Option<&str>) -> Result<Vec<ReceiptRecord>, StoreError> {
        if let Some(client) = client {
            validate_receipt_client(client)?;
        }
        let connection = self.connection()?;
        let sql = "SELECT id, client, config_path, keys_owned, backup_path, installed_at, compresso_version, uninstalled_at FROM integration_receipts WHERE (?1 IS NULL OR client = ?1) ORDER BY id";
        let mut statement = connection.prepare(sql)?;
        let rows = statement
            .query_map([client], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter().map(decode_receipt).collect()
    }
}

fn validate_receipt(receipt: &ReceiptRecord) -> Result<(), StoreError> {
    validate_receipt_client(&receipt.client)?;
    validate_nonempty("receipt config_path", &receipt.config_path)?;
    validate_keys_owned(&receipt.keys_owned).map_err(|reason| StoreError::InvalidField {
        field: "receipt keys_owned",
        reason,
    })?;
    validate_nonempty("receipt backup_path", &receipt.backup_path)?;
    validate_nonempty("receipt compresso_version", &receipt.compresso_version)?;
    if receipt
        .uninstalled_at
        .is_some_and(|uninstalled_at| uninstalled_at < receipt.installed_at)
    {
        return Err(StoreError::InvalidField {
            field: "receipt uninstalled_at",
            reason: "must not precede installed_at",
        });
    }
    Ok(())
}

fn validate_receipt_client(client: &str) -> Result<(), StoreError> {
    if matches!(client, "claude-code" | "codex" | "cursor") {
        Ok(())
    } else {
        Err(StoreError::InvalidField {
            field: "receipt client",
            reason: "must be claude-code, codex, or cursor",
        })
    }
}

fn validate_keys_owned(keys: &[String]) -> Result<(), &'static str> {
    if keys.is_empty() {
        return Err("must contain at least one key path");
    }
    if keys.iter().any(|key| key.is_empty()) {
        return Err("key paths must not be empty");
    }
    if keys.iter().any(|key| key.contains('\0')) {
        return Err("key paths must not contain a NUL byte");
    }
    Ok(())
}

type RawReceipt = (
    i64,
    String,
    String,
    String,
    String,
    i64,
    String,
    Option<i64>,
);

fn decode_receipt(raw: RawReceipt) -> Result<ReceiptRecord, StoreError> {
    let (id, client, config_path, keys_owned, backup_path, installed_at, version, uninstalled_at) =
        raw;
    let keys_owned: Vec<String> =
        serde_json::from_str(&keys_owned).map_err(|error| StoreError::MalformedStoredJson {
            field: "integration_receipts.keys_owned",
            reason: error.to_string(),
        })?;
    validate_keys_owned(&keys_owned).map_err(|reason| StoreError::MalformedStoredJson {
        field: "integration_receipts.keys_owned",
        reason: reason.to_owned(),
    })?;
    Ok(ReceiptRecord {
        id: Some(id),
        client,
        config_path,
        keys_owned,
        backup_path,
        installed_at,
        compresso_version: version,
        uninstalled_at,
    })
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use crate::{ReceiptRecord, Store, StoreError};

    fn setup() -> (tempfile::TempDir, Store) {
        let directory = tempdir().expect("temp directory should be created");
        let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
        (directory, store)
    }

    fn receipt(client: &str, config_path: &str) -> ReceiptRecord {
        ReceiptRecord {
            id: None,
            client: client.to_owned(),
            config_path: config_path.to_owned(),
            keys_owned: vec!["mcp_servers.compresso".to_owned()],
            backup_path: format!("{config_path}.backup"),
            installed_at: 100,
            compresso_version: "0.1.0".to_owned(),
            uninstalled_at: None,
        }
    }

    #[test]
    fn test_receipt_roundtrip_and_filter() {
        let (_directory, store) = setup();
        store
            .save_receipt(&receipt("claude-code", "/tmp/claude.json"))
            .expect("Claude receipt should save");
        store
            .save_receipt(&receipt("codex", "/tmp/config.toml"))
            .expect("Codex receipt should save");

        let codex = store
            .receipts(Some("codex"))
            .expect("Codex receipts should load");
        assert_eq!(codex.len(), 1);
        assert!(codex[0].id.is_some());
        assert_eq!(codex[0].client, "codex");
        assert_eq!(codex[0].config_path, "/tmp/config.toml");
        assert_eq!(codex[0].keys_owned, ["mcp_servers.compresso"]);
        assert_eq!(codex[0].backup_path, "/tmp/config.toml.backup");
        assert_eq!(codex[0].installed_at, 100);
        assert_eq!(codex[0].compresso_version, "0.1.0");
        assert_eq!(codex[0].uninstalled_at, None);
        assert_eq!(
            store
                .receipts(None)
                .expect("all receipts should load")
                .len(),
            2
        );
    }

    #[test]
    fn test_receipt_update_by_id_roundtrips_uninstalled_at() {
        let (_directory, store) = setup();
        store
            .save_receipt(&receipt("cursor", "/tmp/cursor.json"))
            .expect("receipt should save");
        let mut stored = store.receipts(None).expect("receipt should load").remove(0);
        stored.uninstalled_at = Some(200);
        store
            .save_receipt(&stored)
            .expect("stored receipt should update");
        assert_eq!(
            store.receipts(None).expect("receipt should reload"),
            [stored]
        );
    }

    #[test]
    fn test_receipts_reject_malformed_stored_keys_owned() {
        let (_directory, store) = setup();
        store
            .connection()
            .expect("connection should lock")
            .execute(
                "INSERT INTO integration_receipts (client, config_path, keys_owned, backup_path, installed_at, compresso_version) VALUES ('codex', '/tmp/config.toml', '[1]', '/tmp/backup', 100, '0.1.0')",
                [],
            )
            .expect("malformed fixture row should insert");
        assert!(matches!(
            store.receipts(None),
            Err(StoreError::MalformedStoredJson {
                field: "integration_receipts.keys_owned",
                ..
            })
        ));
    }
}

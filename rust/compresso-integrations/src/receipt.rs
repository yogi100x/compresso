use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::plan::{atomic_replace, publish_completed_temp_no_replace, sync_parent};

/// Receipt schema version emitted by the first local integration slice.
pub const RECEIPT_SCHEMA_VERSION: u32 = 1;

/// Sentinel recorded when installation created a new configuration file.
pub const CREATED_FILE_BACKUP_SENTINEL: &str = "(none: file created)";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptHead {
    schema_version: u32,
    client: String,
    config_path: PathBuf,
    plan_hash: String,
    mac: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptEnvelope {
    schema_version: u32,
    receipt: InstallReceipt,
    mac: String,
}

/// Durable proof of the exact integration-owned state Compresso installed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallReceipt {
    /// Boundary schema version.
    pub schema_version: u32,
    /// Discriminator used by `schemas/integrations/v1.json`.
    pub document_type: String,
    /// Stable plan identity; also makes persistence idempotent.
    pub plan_hash: String,
    /// Client identifier.
    pub client: String,
    /// Absolute path changed by the install.
    pub config_path: PathBuf,
    /// Exact owned key paths.
    pub keys_owned: Vec<String>,
    /// Hash of each canonical owned value written by Compresso.
    pub owned_value_hashes: BTreeMap<String, String>,
    /// Timestamped backup path or the created-file sentinel.
    pub backup_path: String,
    /// Unix timestamp in seconds.
    pub installed_at: i64,
    /// Installing Compresso version.
    pub compresso_version: String,
    /// Unix timestamp in seconds after a successful uninstall.
    pub uninstalled_at: Option<i64>,
}

impl InstallReceipt {
    pub(crate) fn validate(&self) -> Result<(), ReceiptError> {
        if self.schema_version != RECEIPT_SCHEMA_VERSION {
            return Err(ReceiptError::Invalid {
                reason: "unsupported schema_version".to_owned(),
            });
        }
        if self.document_type != "install_receipt" {
            return Err(ReceiptError::Invalid {
                reason: "document_type must be install_receipt".to_owned(),
            });
        }
        if !is_lower_hex_digest(&self.plan_hash) {
            return Err(ReceiptError::Invalid {
                reason: "plan_hash must be a 32-byte hexadecimal digest".to_owned(),
            });
        }
        if !matches!(self.client.as_str(), "claude-code" | "codex" | "cursor") {
            return Err(ReceiptError::Invalid {
                reason: "unknown integration client".to_owned(),
            });
        }
        if !self.config_path.is_absolute() {
            return Err(ReceiptError::Invalid {
                reason: "config_path must be absolute".to_owned(),
            });
        }
        if self.keys_owned.is_empty()
            || self.keys_owned.iter().any(|key| key.is_empty())
            || self
                .keys_owned
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != self.keys_owned.len()
            || self
                .keys_owned
                .iter()
                .any(|key| !self.owned_value_hashes.contains_key(key))
            || self
                .owned_value_hashes
                .keys()
                .any(|key| !self.keys_owned.contains(key))
        {
            return Err(ReceiptError::Invalid {
                reason: "keys_owned and owned_value_hashes must be non-empty and identical"
                    .to_owned(),
            });
        }
        if self.backup_path.is_empty() || self.compresso_version.trim().is_empty() {
            return Err(ReceiptError::Invalid {
                reason: "backup_path and compresso_version must not be empty".to_owned(),
            });
        }
        if self
            .owned_value_hashes
            .values()
            .any(|hash| !is_lower_hex_digest(hash))
        {
            return Err(ReceiptError::Invalid {
                reason: "owned value hashes must be lowercase 32-byte hexadecimal digests"
                    .to_owned(),
            });
        }
        if self.uninstalled_at.is_some_and(|at| at < self.installed_at) {
            return Err(ReceiptError::Invalid {
                reason: "uninstalled_at precedes installed_at".to_owned(),
            });
        }
        Ok(())
    }
}

fn is_lower_hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Receipt persistence errors are kept separate from config mutation errors.
#[derive(Debug, Error)]
pub enum ReceiptError {
    /// Receipt failed semantic validation.
    #[error("invalid integration receipt: {reason}")]
    Invalid { reason: String },
    /// Receipt store I/O failed.
    #[error("receipt store I/O failed for {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Receipt store contained malformed JSON.
    #[error("receipt store contains malformed JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// Receipt store's process-local lock was poisoned.
    #[error("receipt store lock was poisoned")]
    LockPoisoned,
    /// Operating-system randomness was unavailable for a private approval key.
    #[error("approval-key entropy unavailable: {reason}")]
    Entropy { reason: String },
    /// Durable approval key is missing, malformed, permissive, or not user-owned.
    #[error("invalid durable approval key at {path}: {reason}")]
    ApprovalKey { path: PathBuf, reason: String },
    /// Journal authenticator did not verify under the durable approval key.
    #[error("integration recovery journal authentication failed")]
    InvalidJournalMac,
    /// Canonical per-plan receipt authenticator did not verify.
    #[error("canonical integration receipt authentication failed")]
    InvalidReceiptMac,
}

/// Persistence boundary used by the integration framework.
///
/// Safety-authoritative production implementations must durably authenticate
/// journals, per-plan receipt records, and active selection. The fixed SQLite
/// `integration_receipts` table is only a repairable metadata projection and
/// must not directly replace [`FileReceiptStore`] at this boundary.
pub trait ReceiptStore: Send + Sync {
    /// Finds a receipt created by the same plan.
    fn by_plan_hash(&self, plan_hash: &str) -> Result<Option<InstallReceipt>, ReceiptError>;

    /// Finds the active receipt for one client configuration.
    fn active_for(
        &self,
        client: &str,
        config_path: &Path,
    ) -> Result<Option<InstallReceipt>, ReceiptError>;

    /// Atomically creates or replaces a receipt.
    fn save(&self, receipt: &InstallReceipt) -> Result<(), ReceiptError>;

    /// Authenticates a journal before its first durable reservation.
    fn seal_journal(&self, payload: &[u8]) -> Result<String, ReceiptError>;

    /// Verifies a persisted journal before any recovery side effect.
    fn verify_journal(&self, payload: &[u8], mac: &str) -> Result<(), ReceiptError>;
}

/// Process-local receipt store useful to embedded callers and tests.
pub struct MemoryReceiptStore {
    receipts: Mutex<Vec<InstallReceipt>>,
    approval_key: [u8; 32],
}

impl Default for MemoryReceiptStore {
    fn default() -> Self {
        let mut approval_key = [0_u8; 32];
        getrandom::fill(&mut approval_key)
            .expect("operating-system randomness is required for journal authentication");
        Self {
            receipts: Mutex::new(Vec::new()),
            approval_key,
        }
    }
}

impl MemoryReceiptStore {
    /// Returns a stable snapshot in insertion order.
    pub fn all(&self) -> Result<Vec<InstallReceipt>, ReceiptError> {
        Ok(self
            .receipts
            .lock()
            .map_err(|_| ReceiptError::LockPoisoned)?
            .clone())
    }
}

impl ReceiptStore for MemoryReceiptStore {
    fn by_plan_hash(&self, plan_hash: &str) -> Result<Option<InstallReceipt>, ReceiptError> {
        Ok(self
            .receipts
            .lock()
            .map_err(|_| ReceiptError::LockPoisoned)?
            .iter()
            .find(|receipt| receipt.plan_hash == plan_hash)
            .cloned())
    }

    fn active_for(
        &self,
        client: &str,
        config_path: &Path,
    ) -> Result<Option<InstallReceipt>, ReceiptError> {
        Ok(self
            .receipts
            .lock()
            .map_err(|_| ReceiptError::LockPoisoned)?
            .iter()
            .rev()
            .find(|receipt| receipt.client == client && receipt.config_path == config_path)
            .filter(|receipt| receipt.uninstalled_at.is_none())
            .cloned())
    }

    fn save(&self, receipt: &InstallReceipt) -> Result<(), ReceiptError> {
        receipt.validate()?;
        let mut receipts = self
            .receipts
            .lock()
            .map_err(|_| ReceiptError::LockPoisoned)?;
        if let Some(existing) = receipts
            .iter_mut()
            .find(|existing| existing.plan_hash == receipt.plan_hash)
        {
            *existing = receipt.clone();
        } else {
            receipts.push(receipt.clone());
        }
        Ok(())
    }

    fn seal_journal(&self, payload: &[u8]) -> Result<String, ReceiptError> {
        Ok(journal_mac(&self.approval_key, payload))
    }

    fn verify_journal(&self, payload: &[u8], mac: &str) -> Result<(), ReceiptError> {
        verify_journal_mac(&self.approval_key, payload, mac)
    }
}

/// Canonical authenticated integration receipt store.
///
/// SQLite may mirror its content for queries, but that projection is never an
/// uninstall authorization or replacement for this safety boundary.
pub struct FileReceiptStore {
    records_path: PathBuf,
    approval_key_path: PathBuf,
    lock: Mutex<()>,
}

impl FileReceiptStore {
    /// Opens a receipt collection at `path`; the file is created lazily.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let approval_key_path = path.with_file_name(format!("{name}.approval-key"));
        let records_path = path.with_file_name(format!("{name}.d"));
        Self {
            records_path,
            approval_key_path,
            lock: Mutex::new(()),
        }
    }

    /// Returns an authenticated snapshot of every canonical receipt.
    ///
    /// The returned records are ordered by installation time and plan hash.
    /// SQLite projections must reconcile from this snapshot, never authorize
    /// restoration in its place.
    pub fn all(&self) -> Result<Vec<InstallReceipt>, ReceiptError> {
        let _guard = self.lock.lock().map_err(|_| ReceiptError::LockPoisoned)?;
        self.load_unlocked()
    }

    pub(crate) fn load_unlocked(&self) -> Result<Vec<InstallReceipt>, ReceiptError> {
        let entries = match fs::read_dir(&self.records_path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(ReceiptError::Io {
                    path: self.records_path.clone(),
                    source,
                });
            }
        };
        let mut paths = entries
            .map(|entry| {
                entry
                    .map(|entry| entry.path())
                    .map_err(|source| ReceiptError::Io {
                        path: self.records_path.clone(),
                        source,
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        paths.retain(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
                && !path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("active-"))
        });
        paths.sort();
        let key = self.load_approval_key_unlocked(false)?;
        let mut receipts = Vec::with_capacity(paths.len());
        for path in paths {
            let bytes = fs::read(&path).map_err(|source| ReceiptError::Io {
                path: path.clone(),
                source,
            })?;
            let receipt = decode_receipt_envelope(&key, &bytes)?;
            receipts.push(receipt);
        }
        receipts.sort_by(|left, right| {
            (left.installed_at, &left.plan_hash).cmp(&(right.installed_at, &right.plan_hash))
        });
        Ok(receipts)
    }

    fn load_approval_key_unlocked(&self, create: bool) -> Result<[u8; 32], ReceiptError> {
        let parent = self
            .approval_key_path
            .parent()
            .unwrap_or_else(|| Path::new("."));
        if create {
            fs::create_dir_all(parent).map_err(|source| ReceiptError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        if !self.approval_key_path.exists() && create {
            let mut key = [0_u8; 32];
            getrandom::fill(&mut key).map_err(|error| ReceiptError::Entropy {
                reason: error.to_string(),
            })?;
            publish_private_file_no_replace(&self.approval_key_path, &key).map_err(|source| {
                ReceiptError::Io {
                    path: self.approval_key_path.clone(),
                    source,
                }
            })?;
        }

        let symlink_metadata = fs::symlink_metadata(&self.approval_key_path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                ReceiptError::ApprovalKey {
                    path: self.approval_key_path.clone(),
                    reason: "key is missing".to_owned(),
                }
            } else {
                ReceiptError::Io {
                    path: self.approval_key_path.clone(),
                    source,
                }
            }
        })?;
        if symlink_metadata.file_type().is_symlink() || !symlink_metadata.is_file() {
            return Err(ReceiptError::ApprovalKey {
                path: self.approval_key_path.clone(),
                reason: "key must be a regular non-symlink file".to_owned(),
            });
        }
        let mut file = File::open(&self.approval_key_path).map_err(|source| ReceiptError::Io {
            path: self.approval_key_path.clone(),
            source,
        })?;
        let metadata = file.metadata().map_err(|source| ReceiptError::Io {
            path: self.approval_key_path.clone(),
            source,
        })?;
        validate_private_key_metadata(&self.approval_key_path, parent, &metadata)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|source| ReceiptError::Io {
                path: self.approval_key_path.clone(),
                source,
            })?;
        bytes
            .try_into()
            .map_err(|_: Vec<u8>| ReceiptError::ApprovalKey {
                path: self.approval_key_path.clone(),
                reason: "key must contain exactly 32 bytes".to_owned(),
            })
    }

    fn head_path(&self, client: &str, config_path: &Path) -> Result<PathBuf, ReceiptError> {
        let identity = serde_json::to_vec(&(client, config_path))?;
        Ok(self
            .records_path
            .join(format!("active-{}.json", blake3::hash(&identity).to_hex())))
    }

    fn save_head_unlocked(&self, receipt: &InstallReceipt) -> Result<(), ReceiptError> {
        let key = self.load_approval_key_unlocked(true)?;
        let path = self.head_path(&receipt.client, &receipt.config_path)?;
        let mut head = ReceiptHead {
            schema_version: 1,
            client: receipt.client.clone(),
            config_path: receipt.config_path.clone(),
            plan_hash: receipt.plan_hash.clone(),
            mac: String::new(),
        };
        head.mac = receipt_head_mac(&key, &head)?;
        let bytes = serde_json::to_vec_pretty(&head)?;
        atomic_replace(&path, &bytes, "active-receipt").map_err(|source| ReceiptError::Io {
            path: path.clone(),
            source,
        })?;
        sync_parent(&path).map_err(|source| ReceiptError::Io {
            path: self.records_path.clone(),
            source,
        })
    }

    fn load_head_unlocked(
        &self,
        client: &str,
        config_path: &Path,
    ) -> Result<Option<InstallReceipt>, ReceiptError> {
        let path = self.head_path(client, config_path)?;
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(ReceiptError::Io {
                    path: path.clone(),
                    source,
                });
            }
        };
        let head: ReceiptHead = serde_json::from_slice(&bytes)?;
        let key = self.load_approval_key_unlocked(false)?;
        if head.schema_version != 1
            || head.client != client
            || head.config_path != config_path
            || receipt_head_mac(&key, &head)? != head.mac
        {
            return Err(ReceiptError::Invalid {
                reason: "active receipt head failed authentication or identity validation"
                    .to_owned(),
            });
        }
        let receipt_path = self.records_path.join(format!("{}.json", head.plan_hash));
        let receipt = decode_receipt_envelope(
            &key,
            &fs::read(&receipt_path).map_err(|source| ReceiptError::Io {
                path: receipt_path.clone(),
                source,
            })?,
        )?;
        if receipt.plan_hash != head.plan_hash
            || receipt.client != client
            || receipt.config_path != config_path
        {
            return Err(ReceiptError::Invalid {
                reason: "active receipt head points to a mismatched receipt".to_owned(),
            });
        }
        Ok(Some(receipt))
    }
}

impl ReceiptStore for FileReceiptStore {
    fn by_plan_hash(&self, plan_hash: &str) -> Result<Option<InstallReceipt>, ReceiptError> {
        let _guard = self.lock.lock().map_err(|_| ReceiptError::LockPoisoned)?;
        Ok(self
            .load_unlocked()?
            .into_iter()
            .find(|receipt| receipt.plan_hash == plan_hash))
    }

    fn active_for(
        &self,
        client: &str,
        config_path: &Path,
    ) -> Result<Option<InstallReceipt>, ReceiptError> {
        let _guard = self.lock.lock().map_err(|_| ReceiptError::LockPoisoned)?;
        Ok(self
            .load_head_unlocked(client, config_path)?
            .filter(|receipt| receipt.uninstalled_at.is_none()))
    }

    fn save(&self, receipt: &InstallReceipt) -> Result<(), ReceiptError> {
        receipt.validate()?;
        let _guard = self.lock.lock().map_err(|_| ReceiptError::LockPoisoned)?;
        ensure_private_state_dir(&self.records_path)?;
        let key = self.load_approval_key_unlocked(true)?;
        let path = self
            .records_path
            .join(format!("{}.json", receipt.plan_hash));
        let mut envelope = ReceiptEnvelope {
            schema_version: 1,
            receipt: receipt.clone(),
            mac: String::new(),
        };
        envelope.mac = receipt_record_mac(&key, receipt)?;
        let bytes = serde_json::to_vec_pretty(&envelope)?;
        atomic_replace(&path, &bytes, "receipt").map_err(|source| ReceiptError::Io {
            path: path.clone(),
            source,
        })?;
        sync_parent(&path).map_err(|source| ReceiptError::Io {
            path: self.records_path.clone(),
            source,
        })?;
        // This one-file replacement is the durable monotonic ordering source.
        // Replaying the authenticated config journal repairs a crash between
        // receipt creation and head replacement.
        self.save_head_unlocked(receipt)
    }

    fn seal_journal(&self, payload: &[u8]) -> Result<String, ReceiptError> {
        let _guard = self.lock.lock().map_err(|_| ReceiptError::LockPoisoned)?;
        let key = self.load_approval_key_unlocked(true)?;
        Ok(journal_mac(&key, payload))
    }

    fn verify_journal(&self, payload: &[u8], mac: &str) -> Result<(), ReceiptError> {
        let _guard = self.lock.lock().map_err(|_| ReceiptError::LockPoisoned)?;
        let key = self.load_approval_key_unlocked(false)?;
        verify_journal_mac(&key, payload, mac)
    }
}

fn ensure_private_state_dir(path: &Path) -> Result<(), ReceiptError> {
    #[cfg(unix)]
    let existed = path.exists();
    fs::create_dir_all(path).map_err(|source| ReceiptError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    #[cfg(unix)]
    if !existed {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
            ReceiptError::Io {
                path: path.to_path_buf(),
                source,
            }
        })?;
    }
    Ok(())
}

fn journal_mac(key: &[u8; 32], payload: &[u8]) -> String {
    let mut authenticated = b"compresso-integration-journal-v1\0".to_vec();
    authenticated.extend_from_slice(payload);
    blake3::keyed_hash(key, &authenticated).to_hex().to_string()
}

fn receipt_head_mac(key: &[u8; 32], head: &ReceiptHead) -> Result<String, ReceiptError> {
    let mut unsigned = head.clone();
    unsigned.mac.clear();
    let mut authenticated = b"compresso-integration-receipt-head-v1\0".to_vec();
    authenticated.extend_from_slice(&serde_json::to_vec(&unsigned)?);
    Ok(blake3::keyed_hash(key, &authenticated).to_hex().to_string())
}

fn receipt_record_mac(key: &[u8; 32], receipt: &InstallReceipt) -> Result<String, ReceiptError> {
    let mut authenticated = b"compresso-integration-receipt-v1\0".to_vec();
    authenticated.extend_from_slice(&serde_json::to_vec(receipt)?);
    Ok(blake3::keyed_hash(key, &authenticated).to_hex().to_string())
}

fn decode_receipt_envelope(key: &[u8; 32], bytes: &[u8]) -> Result<InstallReceipt, ReceiptError> {
    let envelope: ReceiptEnvelope = serde_json::from_slice(bytes)?;
    if envelope.schema_version != 1 || receipt_record_mac(key, &envelope.receipt)? != envelope.mac {
        return Err(ReceiptError::InvalidReceiptMac);
    }
    envelope.receipt.validate()?;
    Ok(envelope.receipt)
}

fn verify_journal_mac(key: &[u8; 32], payload: &[u8], mac: &str) -> Result<(), ReceiptError> {
    let actual = blake3::Hash::from_hex(mac).map_err(|_| ReceiptError::InvalidJournalMac)?;
    let expected = blake3::Hash::from_hex(journal_mac(key, payload))
        .expect("internally generated BLAKE3 hash is valid");
    if actual == expected {
        Ok(())
    } else {
        Err(ReceiptError::InvalidJournalMac)
    }
}

#[cfg(unix)]
fn validate_private_key_metadata(
    path: &Path,
    parent: &Path,
    metadata: &fs::Metadata,
) -> Result<(), ReceiptError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let parent_metadata = fs::metadata(parent).map_err(|source| ReceiptError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(ReceiptError::ApprovalKey {
            path: path.to_path_buf(),
            reason: "key permissions must be user-only (0600)".to_owned(),
        });
    }
    if metadata.uid() != parent_metadata.uid() {
        return Err(ReceiptError::ApprovalKey {
            path: path.to_path_buf(),
            reason: "key owner must match its private state directory owner".to_owned(),
        });
    }
    if parent_metadata.permissions().mode() & 0o022 != 0 {
        return Err(ReceiptError::ApprovalKey {
            path: path.to_path_buf(),
            reason: "approval-key directory must not be group/other writable".to_owned(),
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_key_metadata(
    _path: &Path,
    _parent: &Path,
    metadata: &fs::Metadata,
) -> Result<(), ReceiptError> {
    if metadata.is_file() {
        Ok(())
    } else {
        Err(ReceiptError::ApprovalKey {
            path: _path.to_path_buf(),
            reason: "key must be a regular file".to_owned(),
        })
    }
}

fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file: File = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn publish_private_file_no_replace(path: &Path, bytes: &[u8]) -> std::io::Result<bool> {
    let mut random = [0_u8; 12];
    getrandom::fill(&mut random).map_err(|error| std::io::Error::other(error.to_string()))?;
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temp = path.with_file_name(format!(".{name}.compresso-tmp-approval-key-{suffix}"));

    if let Err(write_error) = write_private_file(&temp, bytes) {
        return match remove_if_present(&temp) {
            Ok(()) => Err(write_error),
            Err(cleanup_error) => Err(cleanup_error),
        };
    }
    let publish_result = publish_completed_temp_no_replace(&temp, path);
    let destination_exists = match &publish_result {
        Ok(()) => true,
        Err(error) => error.kind() == std::io::ErrorKind::AlreadyExists,
    };
    let pre_cleanup_sync_result = if publish_result.is_ok() {
        sync_parent(path)
    } else {
        Ok(())
    };
    let cleanup_result = remove_if_present(&temp);
    let post_cleanup_sync_result = if destination_exists && cleanup_result.is_ok() {
        sync_parent(path)
    } else {
        Ok(())
    };
    pre_cleanup_sync_result?;
    cleanup_result?;
    post_cleanup_sync_result?;

    match publish_result {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    }
}

fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Barrier};

    use tempfile::tempdir;

    use super::*;

    fn receipt(path: PathBuf) -> InstallReceipt {
        InstallReceipt {
            schema_version: 1,
            document_type: "install_receipt".to_owned(),
            plan_hash: "a".repeat(64),
            client: "codex".to_owned(),
            config_path: path,
            keys_owned: vec!["mcp_servers.compresso".to_owned()],
            owned_value_hashes: BTreeMap::from([(
                "mcp_servers.compresso".to_owned(),
                "b".repeat(64),
            )]),
            backup_path: CREATED_FILE_BACKUP_SENTINEL.to_owned(),
            installed_at: 100,
            compresso_version: "0.1.0".to_owned(),
            uninstalled_at: None,
        }
    }

    #[test]
    fn test_file_receipt_store_is_atomic_and_idempotent() {
        let directory = tempdir().expect("temp directory");
        let store = FileReceiptStore::new(directory.path().join("receipts.json"));
        let mut value = receipt(directory.path().join("config.toml"));
        store.save(&value).expect("receipt saves");
        store.save(&value).expect("repeat save is idempotent");
        assert_eq!(store.all().expect("receipts load"), [value.clone()]);

        value.uninstalled_at = Some(200);
        store.save(&value).expect("receipt updates atomically");
        assert_eq!(
            store
                .by_plan_hash(&value.plan_hash)
                .expect("receipt lookup")
                .expect("receipt exists")
                .uninstalled_at,
            Some(200)
        );
    }

    #[test]
    fn test_receipt_rejects_unknown_owned_hash() {
        let directory = tempdir().expect("temp directory");
        let store = MemoryReceiptStore::default();
        let mut value = receipt(directory.path().join("config.toml"));
        value
            .owned_value_hashes
            .insert("unknown.key".to_owned(), "c".repeat(64));
        assert!(matches!(
            store.save(&value),
            Err(ReceiptError::Invalid { .. })
        ));
    }

    #[test]
    fn test_separate_file_store_instances_do_not_lose_concurrent_receipts() {
        let directory = tempdir().expect("temp directory");
        let state = directory.path().join("receipts.json");
        const STORE_COUNT: usize = 16;
        let barrier = Arc::new(Barrier::new(STORE_COUNT));
        let threads = (0..STORE_COUNT)
            .map(|index| {
                let state = state.clone();
                let barrier = Arc::clone(&barrier);
                let mut value = receipt(directory.path().join(format!("config-{index}.toml")));
                value.plan_hash = format!("{index:064x}");
                std::thread::spawn(move || {
                    let store = FileReceiptStore::new(state);
                    barrier.wait();
                    store.save(&value).expect("concurrent receipt saves");
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().expect("receipt thread joins");
        }

        let stored = FileReceiptStore::new(state)
            .load_unlocked()
            .expect("receipts load");
        assert_eq!(stored.len(), STORE_COUNT);
        assert_eq!(
            stored
                .iter()
                .map(|receipt| receipt.plan_hash.as_str())
                .collect::<Vec<_>>(),
            (0..STORE_COUNT)
                .map(|index| format!("{index:064x}"))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            fs::read(directory.path().join("receipts.json.approval-key"))
                .expect("approval key reads")
                .len(),
            32
        );
    }

    #[test]
    fn test_private_file_publication_never_clobbers_and_cleans_temps() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("receipts.json.approval-key");
        assert!(publish_private_file_no_replace(&path, &[1_u8; 32]).expect("first key publishes"));
        assert!(
            !publish_private_file_no_replace(&path, &[2_u8; 32])
                .expect("second key observes winner")
        );
        assert_eq!(fs::read(&path).expect("published key reads"), [1_u8; 32]);
        assert!(
            fs::read_dir(directory.path())
                .expect("directory reads")
                .all(|entry| !entry
                    .expect("directory entry reads")
                    .file_name()
                    .to_string_lossy()
                    .contains("compresso-tmp-approval-key"))
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path)
                    .expect("key metadata reads")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn test_malformed_existing_approval_key_is_not_repaired() {
        let directory = tempdir().expect("temp directory");
        let state = directory.path().join("receipts.json");
        let store = FileReceiptStore::new(state);
        write_private_file(&store.approval_key_path, &[7_u8; 31])
            .expect("malformed key fixture writes");

        assert!(matches!(
            store.seal_journal(b"payload"),
            Err(ReceiptError::ApprovalKey { reason, .. })
                if reason == "key must contain exactly 32 bytes"
        ));
        assert_eq!(
            fs::read(&store.approval_key_path).expect("malformed key remains readable"),
            [7_u8; 31]
        );
    }

    #[test]
    fn test_receipt_rejects_duplicate_keys_and_empty_metadata() {
        let directory = tempdir().expect("temp directory");
        let store = MemoryReceiptStore::default();
        let mut duplicate = receipt(directory.path().join("config.toml"));
        duplicate
            .keys_owned
            .push("mcp_servers.compresso".to_owned());
        assert!(matches!(
            store.save(&duplicate),
            Err(ReceiptError::Invalid { .. })
        ));

        let mut empty = receipt(directory.path().join("config.toml"));
        empty.backup_path.clear();
        empty.compresso_version.clear();
        assert!(matches!(
            store.save(&empty),
            Err(ReceiptError::Invalid { .. })
        ));
    }

    #[test]
    fn test_docs_lock_canonical_file_receipt_and_projection_authority() {
        let integration_contract = include_str!("../../../docs/plan/04-mcp-and-integrations.md");
        let schema_contract = include_str!("../../../docs/plan/02-schemas.md");
        for contract in [integration_contract, schema_contract] {
            let normalized = contract.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(normalized.contains("FileReceiptStore"));
            assert!(normalized.contains("owned_value_hashes"));
            assert!(normalized.contains("SQLite MUST NOT authorize uninstall"));
            assert!(normalized.contains("not claimed to be one atomic transaction"));
        }
        assert!(integration_contract.contains("active-head"));
        assert!(integration_contract.contains("final recheck/replace interval"));
        assert!(integration_contract.contains("Windows"));
    }
}

use std::collections::BTreeMap;

/// Why the engine turned a submission away before queueing it.
#[derive(Debug, PartialEq, Eq)]
pub enum AdmissionError {
    UnknownTenant { tenant: String },
    TenantSuspended { tenant: String, since: u64 },
}

/// One line per job a paused tenant still has waiting, for the operator report.
pub fn paused_tenant_summary(jobs: &BTreeMap<u64, String>, outcome: &AdmissionError) -> Vec<String> {
    let AdmissionError::TenantSuspended { tenant, since } = outcome else {
        return Vec::new();
    };
    jobs.iter()
        .filter(|(_, owner)| *owner == tenant)
        .map(|(job, owner)| format!("job {job} of tenant {owner} is held: suspended since {since}"))
        .collect()
}

/// Decides whether a submission may be queued for its owner.
pub fn gate_submission(paused: &BTreeMap<String, u64>, known: &[String], owner: &str) -> Result<(), AdmissionError> {
    if !known.iter().any(|name| name == owner) {
        return Err(AdmissionError::UnknownTenant { tenant: owner.to_owned() });
    }
    if let Some(since) = paused.get(owner) {
        return Err(AdmissionError::TenantSuspended { tenant: owner.to_owned(), since: *since });
    }
    Ok(())
}

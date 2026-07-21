# 0024 - High-impact tool boundaries

## Status

Accepted

## Context

Editing, SQL mutation, browser automation, and terminal control can change source, data, accounts, or machines. Combining read and write behavior in one broad tool makes client permissions ambiguous and creates prompt-injection escalation paths.

## Decision

Expose separate plan and apply tools. Planning is read-only and returns a content-hashed plan, bounded preview, permission class, and expiry. Applying requires the current plan hash, explicit approval, unchanged base versions, and policy authorization. Edit writes are transactional with rollback records. SQL defaults to a read-only role and transaction; mutations require elevated approval. Browser and terminal capabilities run in isolated profiles and cannot inherit credentials or filesystem access by default. `assist` is the default integration mode; Compresso never disables native client tools automatically.

## Consequences

- Clients can present accurate read/write/destructive annotations.
- Stale plans and time-of-check/time-of-use races fail closed.
- Prompt content cannot grant itself additional permissions.
- Operations remain unavailable where rollback or isolation cannot be guaranteed.

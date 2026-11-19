"""Removes blobs that outlived their retention policy."""

import os


class RetentionPolicy:
    def __init__(self, keep_days):
        self.keep_days = keep_days

    def is_expired(self, age_days):
        return age_days > self.keep_days


def purge_expired_blobs(store, policy, ages_by_digest):
    """Deletes every expired blob and returns the digests that were removed."""
    removed = []
    for digest, age_days in sorted(ages_by_digest.items()):
        if not policy.is_expired(age_days):
            continue
        location = store.blob_location(digest)
        if os.path.exists(location):
            os.remove(location)
        removed.append(digest)
    return removed

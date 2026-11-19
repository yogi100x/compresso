"""Content-addressed blob storage on the local disk."""

import hashlib
import os

from storage.errors import IntegrityError, QuotaExceeded


class BlobStore:
    def __init__(self, root, quota_bytes):
        self.root = root
        self.quota_bytes = quota_bytes
        self.used_bytes = 0

    def open_blob(self, name):
        """Starts a staged write and returns the path of the staging area."""
        staging = os.path.join(self.root, "staging", name)
        os.makedirs(os.path.dirname(staging), exist_ok=True)
        return staging

    def finalize(self, staging, expected_digest):
        with open(staging, "rb") as handle:
            data = handle.read()
        # Verify the checksum of the upload before it is committed: a
        # mismatch means the bytes were damaged in transit.
        actual_digest = hashlib.sha256(data).hexdigest()
        if actual_digest != expected_digest:
            os.remove(staging)
            raise IntegrityError(expected_digest, actual_digest)
        if self.used_bytes + len(data) > self.quota_bytes:
            raise QuotaExceeded(self.quota_bytes)
        target = os.path.join(self.root, "blobs", actual_digest)
        os.makedirs(os.path.dirname(target), exist_ok=True)
        os.replace(staging, target)
        self.used_bytes += len(data)
        return actual_digest

    def blob_location(self, digest):
        return os.path.join(self.root, "blobs", digest)

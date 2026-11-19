"""Storage settings loaded from a config file."""

import json

from storage.errors import FileNotReadable


class StorageConfig:
    def __init__(self, root, quota_bytes, keep_days):
        self.root = root
        self.quota_bytes = quota_bytes
        self.keep_days = keep_days


def read_config_file(location):
    """Reads the JSON config file; a missing file is an error, not a default."""
    try:
        with open(location, encoding="utf-8") as handle:
            raw = json.load(handle)
    except OSError as error:
        raise FileNotReadable(location) from error
    return StorageConfig(
        root=raw["root"],
        quota_bytes=int(raw.get("quota_bytes", 1 << 30)),
        keep_days=int(raw.get("keep_days", 14)),
    )

"""Failures raised by the storage layer."""


class StorageError(Exception):
    """Base class for every storage failure."""


class FileNotReadable(StorageError):
    def __init__(self, location):
        super().__init__(f"cannot read {location}")
        self.location = location


class QuotaExceeded(StorageError):
    def __init__(self, limit_bytes):
        super().__init__(f"storage limit of {limit_bytes} bytes reached")
        self.limit_bytes = limit_bytes


class IntegrityError(StorageError):
    def __init__(self, expected, actual):
        super().__init__(f"expected {expected}, got {actual}")
        self.expected = expected
        self.actual = actual

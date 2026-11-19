import hashlib

import pytest

from storage.blobs import BlobStore
from storage.errors import IntegrityError, QuotaExceeded


def write_staged(store, name, data):
    staging = store.open_blob(name)
    with open(staging, "wb") as handle:
        handle.write(data)
    return staging


def test_finalize_moves_file_into_place(tmp_path):
    store = BlobStore(str(tmp_path), quota_bytes=1024)
    staging = write_staged(store, "report", b"hello")
    digest = store.finalize(staging, hashlib.sha256(b"hello").hexdigest())
    assert store.blob_location(digest).endswith(digest)


def test_finalize_rejects_damaged_file(tmp_path):
    store = BlobStore(str(tmp_path), quota_bytes=1024)
    staging = write_staged(store, "report", b"hello")
    with pytest.raises(IntegrityError):
        store.finalize(staging, "0" * 64)


def test_storage_limit_raises_error(tmp_path):
    store = BlobStore(str(tmp_path), quota_bytes=2)
    staging = write_staged(store, "report", b"hello")
    with pytest.raises(QuotaExceeded):
        store.finalize(staging, hashlib.sha256(b"hello").hexdigest())

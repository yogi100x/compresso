from storage.blobs import BlobStore
from storage.retention import RetentionPolicy, purge_expired_blobs


def test_purge_expired_blobs_removes_only_old_entries(tmp_path):
    store = BlobStore(str(tmp_path), quota_bytes=1024)
    policy = RetentionPolicy(keep_days=7)
    removed = purge_expired_blobs(store, policy, {"aa": 30, "bb": 1})
    assert removed == ["aa"]


def test_retention_policy_keeps_blob_on_last_day():
    policy = RetentionPolicy(keep_days=7)
    assert not policy.is_expired(7)
    assert policy.is_expired(8)

"""Maps a job id to the blobs its result is made of."""


class ManifestIndex:
    def __init__(self):
        self._by_job = {}

    def record_manifest(self, job_id, digests):
        self._by_job[job_id] = list(digests)

    def lookup_manifest(self, job_id):
        """Returns the digests recorded for a job, oldest first."""
        return list(self._by_job.get(job_id, []))

    def forget_job(self, job_id):
        self._by_job.pop(job_id, None)

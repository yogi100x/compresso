class JobQueue:
    def push(self, job):
        return job

    async def drain(self):
        return []

def make_queue():
    return JobQueue()

MAX_RETRIES = 3

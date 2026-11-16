"""Streaming reads that never hold a whole file in memory."""

DEFAULT_CHUNK_BYTES = 64 * 1024


class FileReader:
    def __init__(self, location, chunk_bytes=DEFAULT_CHUNK_BYTES):
        self.location = location
        self.chunk_bytes = chunk_bytes

    def read_file_chunked(self):
        """Yields the file one chunk at a time until it is exhausted."""
        with open(self.location, "rb") as handle:
            while True:
                chunk = handle.read(self.chunk_bytes)
                if not chunk:
                    return
                yield chunk


def count_chunks(location, chunk_bytes=DEFAULT_CHUNK_BYTES):
    reader = FileReader(location, chunk_bytes)
    return sum(1 for _ in reader.read_file_chunked())

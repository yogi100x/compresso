"""Multipart uploads: a large blob arrives as numbered parts."""

from storage.errors import StorageError
from storage.limits import SEGMENT_CEILING


class PartRefused(StorageError):
    def __init__(self, number, length):
        super().__init__(f"part {number} is {length} bytes")
        self.number = number
        self.length = length


class MultipartUpload:
    def __init__(self, blob_id):
        self.blob_id = blob_id
        self.parts = {}

    def accept_part(self, number, data):
        """Stores one part; an oversized part is refused before it is written."""
        if len(data) > SEGMENT_CEILING:
            raise PartRefused(number, len(data))
        self.parts[number] = data

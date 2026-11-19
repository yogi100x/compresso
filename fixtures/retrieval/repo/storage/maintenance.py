"""Housekeeping for the metadata database."""


def nightly(cursor):
    """Reclaims the space freed by deleted rows."""
    cursor.execute("VACUUM")
    cursor.execute("ANALYZE")

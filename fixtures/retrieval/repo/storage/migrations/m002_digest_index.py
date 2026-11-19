def apply_m002(cursor):
    cursor.execute("CREATE INDEX entries_digest ON entries (digest)")


def revert_m002(cursor):
    cursor.execute("DROP INDEX entries_digest")

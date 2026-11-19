def apply_m005(cursor):
    cursor.execute("CREATE TABLE sizes (digest TEXT PRIMARY KEY, bytes INTEGER)")


def revert_m005(cursor):
    cursor.execute("DROP TABLE sizes")

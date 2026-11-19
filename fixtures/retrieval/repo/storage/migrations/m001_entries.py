def apply_m001(cursor):
    cursor.execute("CREATE TABLE entries (id INTEGER PRIMARY KEY, digest TEXT)")


def revert_m001(cursor):
    cursor.execute("DROP TABLE entries")

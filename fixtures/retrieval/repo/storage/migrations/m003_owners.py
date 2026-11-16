def apply_m003(cursor):
    cursor.execute("CREATE TABLE owners (id INTEGER PRIMARY KEY, label TEXT)")


def revert_m003(cursor):
    cursor.execute("DROP TABLE owners")

def apply_m004(cursor):
    cursor.execute("CREATE VIEW owned AS SELECT digest, label FROM entries JOIN owners USING (id)")


def revert_m004(cursor):
    cursor.execute("DROP VIEW owned")

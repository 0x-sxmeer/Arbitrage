import sqlite3
conn = sqlite3.connect('base_pools.db')
c = conn.cursor()
c.execute("SELECT name FROM sqlite_master WHERE type='table'")
tables = c.fetchall()
print("Tables:", tables)

for table in tables:
    tname = table[0]
    try:
        c.execute(f"DELETE FROM {tname} WHERE name LIKE '%UNK%' OR name LIKE '%VIRTUAL%'")
        print(f"Deleted {c.rowcount} rows from {tname}")
    except Exception as e:
        print(e)
conn.commit()
conn.close()

import psycopg2
import os
import sys

db_url = os.environ.get('DATABASE_URL', 'postgresql://arb_user:arb@localhost:5432/arb_engine')

try:
    conn = psycopg2.connect(db_url)
    c = conn.cursor()
    c.execute("SELECT pool_id, dex, token_a_sym, token_b_sym FROM pool_registry WHERE token_a_sym ILIKE '%UNK%' OR token_b_sym ILIKE '%UNK%' OR token_a_sym ILIKE '%VIRTUAL%' OR token_b_sym ILIKE '%VIRTUAL%';")
    rows = c.fetchall()
    print(f"Found {len(rows)} toxic pools in postgres:")
    for row in rows:
        print(row)
        
    conn.close()
except Exception as e:
    print("Error:", e)

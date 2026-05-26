import pg from 'pg';
const { Client } = pg;

const client = new Client({
  connectionString: 'postgresql://arb_user:arb_password@localhost:5432/arb_engine',
});

async function run() {
  try {
    await client.connect();
    const res = await client.query(`
      INSERT INTO pools (
        id, chain, dex, token_a, token_b, fee_bps, pool_type, reserve_usd
      ) VALUES (
        'fake_arb_pool_1', 'Base', 'UniswapV2', '0x4200000000000000000000000000000000000006', '0x833589fcd6edb6e08f4c7c32d4f71b54bda02913', 30, 'ConstantProduct', 1000000
      ) ON CONFLICT (id) DO NOTHING;
    `);
    console.log('Fake pool inserted into PostgreSQL:', res.rowCount);
  } catch (err) {
    console.error(err);
  } finally {
    await client.end();
  }
}
run();

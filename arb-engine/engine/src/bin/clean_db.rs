use sqlx::postgres::PgPoolOptions;
use std::env;
use dotenvy::dotenv;

#[tokio::main]
async fn main() -> Result<(), sqlx::Error> {
    dotenv().ok();
    let db_url = env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = PgPoolOptions::new().max_connections(2).connect(&db_url).await?;

    let res = sqlx::query("DELETE FROM pool_registry WHERE token_a_symbol LIKE '%UNK%' OR token_b_symbol LIKE '%UNK%' OR token_a_symbol LIKE '%VIRTUAL%' OR token_b_symbol LIKE '%VIRTUAL%'")
        .execute(&pool)
        .await?;

    println!("Deleted {} tax token pools.", res.rows_affected());
    Ok(())
}

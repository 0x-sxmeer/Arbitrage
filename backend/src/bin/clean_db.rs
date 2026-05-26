use dotenvy::dotenv;
use sqlx::postgres::PgPoolOptions;
use std::env;

#[tokio::main]
async fn main() -> Result<(), sqlx::Error> {
    dotenv().ok();
    let db_url = env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&db_url)
        .await?;

    let res = sqlx::query("DELETE FROM pool_registry WHERE token_a_sym LIKE '%UNK%' OR token_b_sym LIKE '%UNK%' OR token_a_sym LIKE '%VIRTUAL%' OR token_b_sym LIKE '%VIRTUAL%'")
        .execute(&pool)
        .await?;

    println!("Deleted {} tax token pools.", res.rows_affected());
    Ok(())
}

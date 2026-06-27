use duckdb::Connection;
use escurel_index::Migrator;
#[test]
#[ignore]
fn live() {
    let p = std::env::var("DB").unwrap();
    let conn = Connection::open(&p).expect("open");
    Migrator::load_extensions(&conn).ok();
    let c = |s: &str| -> i64 { conn.query_row(s, [], |r| r.get(0)).unwrap_or(-1) };
    println!(
        "pages={} chat_messages={} crdt_ops={}",
        c("SELECT count(*) FROM pages"),
        c("SELECT count(*) FROM chat_messages"),
        c("SELECT count(*) FROM crdt_ops")
    );
    if let Ok(mut s) = conn.prepare(
        "SELECT chat_group_id,count(*) FROM chat_messages GROUP BY 1 ORDER BY 2 DESC LIMIT 10",
    ) && let Ok(rs) = s.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
    {
        for x in rs {
            let (g, n) = x.unwrap();
            println!("  {g} = {n}");
        }
    }
}

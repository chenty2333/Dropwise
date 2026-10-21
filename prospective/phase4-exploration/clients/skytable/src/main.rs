use skytable::{Config, Query};

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is required"))
}

fn q(s: String) -> Query { Query::new(&s) }

#[tokio::main(flavor = "multi_thread", worker_threads = 1)]
async fn main() {
    let addr = env("DW_SKY_ADDR");
    let (host, port) = addr.rsplit_once(':').unwrap();
    let user = env("DW_SKY_USER");
    let pass = env("DW_SKY_PASS");
    let space = std::env::var("DW_SKY_SPACE").unwrap_or_else(|_| "dw_r3_probe".into());
    let cfg = Config::new(host, port.parse().unwrap(), &user, &pass);
    let mut c = cfg.connect_async().await.expect("connect");
    let mut qs: Vec<(String, Query)> = Vec::new();
    qs.push(("create space".into(), q(format!("create space {space}"))));
    qs.push(("create model".into(), q(format!("create model {space}.kv(id: string, v: uint64)"))));
    let mut iq = q(format!("insert into {space}.kv(?, ?)"));
    iq.push_param("k"); iq.push_param(0u64);
    qs.push(("insert".into(), iq));
    let mut sq = q(format!("select * from {space}.kv where id = ?"));
    sq.push_param("k");
    qs.push(("select".into(), sq));
    let mut uq = q(format!("update {space}.kv set v = ? where id = ?"));
    uq.push_param(7u64); uq.push_param("k");
    qs.push(("update".into(), uq));
    let mut sq2 = q(format!("select * from {space}.kv where id = ?"));
    sq2.push_param("k");
    qs.push(("select2".into(), sq2));
    let mut dq = q(format!("delete from {space}.kv where id = ?"));
    dq.push_param("k");
    qs.push(("delete".into(), dq));
    qs.push(("drop model".into(), q(format!("drop model {space}.kv"))));
    qs.push(("drop space".into(), q(format!("drop space {space}"))));
    qs.push(("sysctl".into(), q("sysctl report status".into())));
    for (name, query) in qs {
        match c.query(&query).await {
            Ok(resp) => println!("PROBE {name}: Ok({:?})", resp),
            Err(err) => println!("PROBE {name}: Err({err:?})"),
        }
    }
}

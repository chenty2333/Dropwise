//! Short Gel 7.1 probe (not a ladder): connection, max_concurrency=1,
//! cancelled first transaction round trip, same-client survivor and a second
//! independent client as observer.

use std::time::Duration;

use dropwise::net::{Proxy, ProxyOptions};
use gel_tokio::{Builder, Client, TlsSecurity};

const QUERY: &str = "with u := (update DwGel filter .k = <str>$0 set { v := <int64>$1, marker := <str>$2 }) select (u.v, u.marker)";
const READ: &str = "select (DwGel.v, DwGel.marker) filter DwGel.k = <str>$0";

fn client_for(host: std::net::IpAddr, port: u16) -> Result<Client, String> {
    let cfg = Builder::new()
        .host(host)
        .port(port)
        .user("admin")
        .branch("main")
        .tls_security(TlsSecurity::Insecure)
        .max_concurrency(1usize)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(Client::new(&cfg))
}

async fn update(client: &Client, k: &str, v: i64, marker: &str) -> Result<(i64, String), String> {
    let rows: Vec<(i64, String)> = client
        .query(QUERY, &(k, v, marker))
        .await
        .map_err(|e| e.to_string())?;
    rows.into_iter().next().ok_or_else(|| "no row".into())
}

async fn update_tx(client: &Client, k: &str, v: i64, marker: &str) -> Result<(i64, String), String> {
    let rows: Vec<(i64, String)> = client
        .transaction(|mut tx| {
            let k = k.to_string();
            let marker = marker.to_string();
            async move { tx.query(QUERY, &(k.as_str(), v, marker.as_str())).await }
        })
        .await
        .map_err(|e| e.to_string())?;
    rows.into_iter().next().ok_or_else(|| "no row".into())
}

#[tokio::test]
async fn gel_probe() -> Result<(), String> {
    let port: u16 = std::env::var("DW_GEL_PORT").map_err(|e| e.to_string())?.parse().map_err(|e| format!("{e}"))?;
    let upstream: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().map_err(|e| format!("{e}"))?;
    let observer = client_for(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)), port)?;
    observer.ensure_connected().await.map_err(|e| e.to_string())?;
    let _ = observer
        .execute("create type default::DwGel { create required property k -> str; create required property v -> int64; create required property marker -> str; };", &())
        .await;
    for rep in 0..3 {
        let record = format!("r3-gel-{}-{rep}", std::process::id());
        observer
            .execute(
                "insert DwGel { k := <str>$0, v := 0, marker := 'seed' }",
                &(record.as_str(),),
            )
            .await
            .map_err(|e| e.to_string())?;
        let proxy = Proxy::start_with(upstream, ProxyOptions { reply_delay: Duration::from_millis(50) })
            .await
            .map_err(|e| e.to_string())?;
        let tested = client_for(proxy.addr().ip(), proxy.addr().port())?;
        tested.ensure_connected().await.map_err(|e| e.to_string())?;
        let cancelled = tokio::time::timeout(
            Duration::from_millis(10),
            update_tx(&tested, &record, 1, "target"),
        )
        .await
        .is_err();
        let after_cancel: Result<Vec<(i64, String)>, String> = observer
            .query(READ, &(record.as_str(),))
            .await
            .map_err(|e| e.to_string());
        let stale = update_tx(&tested, &record, 7, "survivor").await;
        let own: Result<Vec<(i64, String)>, String> = tested
            .query(READ, &(record.as_str(),))
            .await
            .map_err(|e| e.to_string());
        let visible: Result<Vec<(i64, String)>, String> = observer
            .query(READ, &(record.as_str(),))
            .await
            .map_err(|e| e.to_string());
        println!(
            "GEL-PROBE rep={rep} cancelled={cancelled} after_cancel={after_cancel:?} \
             stale_survivor={stale:?} own={own:?} visible={visible:?} \
             proxy_connections={}",
            proxy.connections().len()
        );
    }
    Ok(())
}

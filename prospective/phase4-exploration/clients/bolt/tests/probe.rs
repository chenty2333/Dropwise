use std::env;
use tokio::io::BufStream;

use bolt_client::bolt_proto::message::Success;
use bolt_client::bolt_proto::version::*;
use bolt_client::bolt_proto::{Message, Value};
use bolt_client::{Client, Metadata, Params, Stream};
use tokio_util::compat::TokioAsyncReadCompatExt;

fn meta() -> Metadata {
    let user = env::var("DW_NEO4J_USER").unwrap_or_else(|_| "neo4j".into());
    let password = env::var("DW_NEO4J_PASSWORD").unwrap_or_else(|_| "dropwise".into());
    Metadata::from_iter(vec![
        ("user_agent", "dropwise-p4e/0.1"),
        ("scheme", "basic"),
        ("principal", user.as_str()),
        ("credentials", password.as_str()),
    ])
}

#[tokio::test]
async fn probe() -> Result<(), Box<dyn std::error::Error>> {
    let addr = env::var("DW_NEO4J_URL")?;
    let addr = addr.strip_prefix("bolt://").unwrap_or(&addr).to_string();
    let stream = Stream::connect(&addr, None::<&str>).await?;
    let stream = BufStream::new(stream).compat();
    let mut client = Client::new(stream, &[V4_4, V4_3, 0, 0]).await?;
    let hello: Message = client.hello(meta()).await?;
    eprintln!("hello: {hello:?}");
    assert!(Success::try_from(hello).is_ok());
    let msg: Message = client.run("RETURN 1 AS n", None, None).await?;
    eprintln!("run: {msg:?}");
    assert!(Success::try_from(msg).is_ok());
    let (records, summary) = client.pull(Some(Metadata::from_iter(vec![("n", 1)]))).await?;
    eprintln!("pull records={records:?} summary={summary:?}");
    assert!(Success::try_from(summary).is_ok());
    let msg = client.run("CREATE (k:DwBolt {id: $id}) SET k.v = $v RETURN k.v AS v",
        Some(Params::from_iter(vec![("id", Value::from("probe")), ("v", Value::from(3))])), None).await?;
    eprintln!("write run: {msg:?}");
    assert!(Success::try_from(msg).is_ok());
    let (records, summary) = client.pull(Some(Metadata::from_iter(vec![("n", -1)]))).await?;
    eprintln!("write pull records={records:?} summary={summary:?}");
    assert!(Success::try_from(summary).is_ok());
    let msg = client.run("MATCH (k:DwBolt {id: 'probe'}) DETACH DELETE k", None, None).await?;
    let _ = Success::try_from(msg)?;
    let (_records, summary) = client.pull(Some(Metadata::from_iter(vec![("n", -1)]))).await?;
    let _ = Success::try_from(summary)?;
    Ok(())
}

use std::time::{Duration, Instant};
use std::io::{Read, Write};
use axum::{routing::get, Router};

struct Guard(Instant);
impl Drop for Guard {
    fn drop(&mut self) {
        eprintln!("handler future dropped after {:?}", self.0.elapsed());
    }
}

async fn handler() -> &'static str {
    let g = Guard(Instant::now());
    tokio::time::sleep(Duration::from_secs(2)).await;
    eprintln!("handler finished normally after {:?}", g.0.elapsed());
    std::mem::forget(g);
    "ok"
}

fn client(port: u16, mode: &str) {
    let s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    let mut s2 = s.try_clone().unwrap();
    s2.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    std::thread::sleep(Duration::from_millis(200));
    match mode {
        "rst" => { socket2::SockRef::from(&s).set_linger(Some(Duration::ZERO)).unwrap(); drop(s); drop(s2); }
        "fin" => { drop(s); drop(s2); }
        "halfclose" => { s.shutdown(std::net::Shutdown::Write).unwrap(); let mut b = Vec::new(); let _ = s2.read_to_end(&mut b); eprintln!("halfclose client got {} bytes", b.len()); }
        _ => unreachable!(),
    }
}

#[tokio::main]
async fn main() {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(l, Router::new().route("/", get(handler))).await.unwrap() });
    for mode in ["fin", "rst", "halfclose"] {
        eprintln!("== {mode}");
        let t = Instant::now();
        tokio::task::spawn_blocking(move || client(port, mode)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(2500).saturating_sub(t.elapsed())).await;
    }
}

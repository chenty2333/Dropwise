//! Protocol-level checks independent of GreenMail and Dropwise. Run against
//! published code normally, or set SMTP_POISON_PATCH=1 for the experimental
//! poison patch. That mode deliberately characterizes its rejection regression.
use async_smtp::{Envelope, SendableEmail, SmtpClient, SmtpTransport};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufStream, DuplexStream};
use tokio::sync::{oneshot, Notify};

type Transport = SmtpTransport<BufStream<DuplexStream>>;
fn patched() -> bool {
    std::env::var_os("SMTP_POISON_PATCH").is_some()
}
fn mail() -> SendableEmail {
    SendableEmail::new(
        Envelope::new(
            Some("a@example.com".parse().unwrap()),
            vec!["b@example.com".parse().unwrap()],
        )
        .unwrap(),
        "Subject: test\r\n\r\nbody",
    )
}

struct Server {
    task: tokio::task::JoinHandle<()>,
    commands: Arc<Mutex<Vec<String>>>,
    at_data: oneshot::Receiver<()>,
    release: Arc<Notify>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn connect(
    reject: Option<(&'static str, u16)>,
    pipeline: bool,
    hold_data: bool,
) -> (Transport, Server) {
    let (client, server) = tokio::io::duplex(8192);
    let commands = Arc::new(Mutex::new(Vec::new()));
    let seen = commands.clone();
    let (tx, at_data) = oneshot::channel();
    let release = Arc::new(Notify::new());
    let gate = release.clone();
    let task = tokio::spawn(async move {
        let mut stream = BufReader::new(server);
        stream
            .get_mut()
            .write_all(b"220 localhost test\r\n")
            .await
            .unwrap();
        let mut rejected = false;
        let mut in_body = false;
        let mut tx = Some(tx);
        loop {
            let mut line = String::new();
            if stream.read_line(&mut line).await.unwrap_or(0) == 0 {
                break;
            }
            seen.lock().unwrap().push(line.clone());
            let stage = if in_body {
                if line != ".\r\n" {
                    continue;
                }
                in_body = false;
                "FINAL"
            } else if line.starts_with("EHLO") {
                "EHLO"
            } else if line.starts_with("MAIL") {
                "MAIL"
            } else if line.starts_with("RCPT") {
                "RCPT"
            } else if line == "DATA\r\n" {
                "DATA"
            } else {
                panic!("unexpected command: {line:?}")
            };
            let response = if stage == "EHLO" {
                if pipeline {
                    "250-localhost\r\n250 PIPELINING\r\n".to_string()
                } else {
                    "250 localhost\r\n".to_string()
                }
            } else if !rejected && reject.is_some_and(|(s, _)| s == stage) {
                rejected = true;
                format!("{} test response\r\n", reject.unwrap().1)
            } else if stage == "DATA" {
                in_body = true;
                if hold_data {
                    if let Some(tx) = tx.take() {
                        let _ = tx.send(());
                        gate.notified().await;
                    }
                }
                "354 send body\r\n".to_string()
            } else {
                "250 OK\r\n".to_string()
            };
            if stream
                .get_mut()
                .write_all(response.as_bytes())
                .await
                .is_err()
            {
                break;
            }
        }
    });
    let transport = SmtpTransport::new(
        SmtpClient::new().pipelining(pipeline),
        BufStream::new(client),
    )
    .await
    .unwrap();
    (
        transport,
        Server {
            task,
            commands,
            at_data,
            release,
        },
    )
}

async fn send(t: &mut Transport) -> async_smtp::error::SmtpResult {
    tokio::time::timeout(Duration::from_secs(2), t.send(mail()))
        .await
        .expect("send must finish")
}

#[tokio::test]
async fn successful_sends_remain_reusable_with_and_without_pipelining() {
    for pipeline in [false, true] {
        let (mut t, _server) = connect(None, pipeline, false).await;
        for _ in 0..3 {
            assert_eq!(send(&mut t).await.unwrap().code.to_string(), "250");
        }
    }
}

#[tokio::test]
async fn ordinary_rejections_preserve_original_error_and_expose_poison_policy() {
    for stage in ["MAIL", "RCPT", "DATA", "FINAL"] {
        for code in [450, 550] {
            let (mut t, server) = connect(Some((stage, code)), false, false).await;
            let error = send(&mut t).await.unwrap_err();
            match error {
                async_smtp::error::Error::Transient(r) | async_smtp::error::Error::Permanent(r) => {
                    assert_eq!(r.code.to_string(), code.to_string())
                }
                e => panic!("wrong rejection: {e}"),
            }
            let before = server.commands.lock().unwrap().len();
            let retry = send(&mut t).await;
            if patched() {
                assert!(retry
                    .unwrap_err()
                    .to_string()
                    .contains("previous send did not complete"));
                assert_eq!(server.commands.lock().unwrap().len(), before);
            } else {
                assert_eq!(retry.unwrap().code.to_string(), "250");
            }
        }
    }
}

#[tokio::test]
async fn final_rejection_after_pipelining_also_exposes_poison_policy() {
    for code in [450, 550] {
        let (mut t, _server) = connect(Some(("FINAL", code)), true, false).await;
        assert!(send(&mut t).await.is_err());
        let retry = send(&mut t).await;
        if patched() {
            assert!(retry
                .unwrap_err()
                .to_string()
                .contains("previous send did not complete"));
        } else {
            assert_eq!(retry.unwrap().code.to_string(), "250");
        }
    }
}

#[tokio::test]
async fn a_nonfinal_message_reply_is_not_a_successful_completion() {
    let (mut t, _server) = connect(Some(("FINAL", 354)), false, false).await;
    let result = send(&mut t).await;
    if patched() {
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("non-final SMTP response"));
    } else {
        assert_eq!(result.unwrap().code.to_string(), "354");
    }
}

#[tokio::test]
async fn cancellation_at_data_requires_reconnect() {
    let (mut t, mut server) = connect(None, false, true).await;
    {
        let request = t.send(mail());
        tokio::pin!(request);
        tokio::select! {
            outcome = &mut request => panic!("send finished before DATA gate: {outcome:?}"),
            _ = &mut server.at_data => {}
        }
    } // drop the in-flight send after the server read DATA
    server.release.notify_one();
    let retry = tokio::time::timeout(Duration::from_millis(100), t.send(mail())).await;
    if patched() {
        assert!(retry
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("previous send did not complete"));
    } else {
        assert!(
            retry.is_err(),
            "commands after cancellation should be trapped in body mode"
        );
    }
    let (mut fresh, _fresh_server) = connect(None, false, false).await;
    assert_eq!(send(&mut fresh).await.unwrap().code.to_string(), "250");
}

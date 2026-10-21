
    // =====================================================================
    // Round-2 regression tests for the APPEND continuation read.
    // Injected before the closing brace of client::tests by
    // apply-round2-patch.sh. With --no-fix these tests run on the published
    // 0.12.0 code and must fail on the stale-response cases; with the patch
    // they must pass.
    // =====================================================================

    #[cfg(feature = "runtime-tokio")]
    async fn round2_append(session: &mut Session<MockStream>, content: &str) -> Result<()> {
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            session.append("INBOX", None, None, content),
        )
        .await
        .expect("APPEND must not wait forever")
    }

    #[cfg(feature = "runtime-tokio")]
    #[tokio::test]
    async fn round2_normal_append_still_works() {
        let response = b"+ OK\r\nA0001 OK [APPENDUID 1 2] APPEND completed\r\n".to_vec();
        let mut session = mock_session!(MockStream::new(response));
        round2_append(&mut session, "foobarbaz").await.unwrap();
        assert_eq!(
            session.stream.inner.written_buf,
            b"A0001 APPEND \"INBOX\" {9}\r\nfoobarbaz\r\n".to_vec()
        );
    }

    #[cfg(feature = "runtime-tokio")]
    #[tokio::test]
    async fn round2_append_rejected_before_continuation_returns_error() {
        let response = b"A0001 NO [TRYCREATE] Mailbox doesn't exist\r\n".to_vec();
        let mut session = mock_session!(MockStream::new(response));
        let err = round2_append(&mut session, "foobarbaz").await.unwrap_err();
        assert!(matches!(err, Error::Append), "{err:?}");
    }

    #[cfg(feature = "runtime-tokio")]
    #[tokio::test]
    async fn round2_append_bad_before_continuation_returns_error() {
        let response = b"A0001 BAD Invalid arguments\r\n".to_vec();
        let mut session = mock_session!(MockStream::new(response));
        let err = round2_append(&mut session, "foobarbaz").await.unwrap_err();
        assert!(matches!(err, Error::Append), "{err:?}");
    }

    #[cfg(feature = "runtime-tokio")]
    #[tokio::test]
    async fn round2_append_skips_stale_select_responses() {
        // The first eight responses are replies to an abandoned SELECT. The
        // published code treats the first untagged FLAGS as the APPEND
        // continuation and fails. The fix must skip them and use "+ OK".
        let response = b"* FLAGS (\\Answered \\Deleted \\Draft \\Flagged \\Seen)\r\n\
                         * 7 EXISTS\r\n\
                         * 2 RECENT\r\n\
                         * OK [UIDVALIDITY 1]\r\n\
                         * OK [UIDNEXT 8]\r\n\
                         * OK [UNSEEN 1] Message 1 is the first unseen\r\n\
                         * OK [PERMANENTFLAGS (\\Answered \\Deleted \\Draft \\Flagged \\Seen \\*)]\r\n\
                         A9999 OK [READ-WRITE] SELECT completed\r\n\
                         + OK\r\n\
                         A0001 OK [APPENDUID 1 8] APPEND completed\r\n"
            .to_vec();
        let mut session = mock_session!(MockStream::new(response));
        round2_append(&mut session, "foobarbaz").await.unwrap();
        assert_eq!(
            session.stream.inner.written_buf,
            b"A0001 APPEND \"INBOX\" {9}\r\nfoobarbaz\r\n".to_vec()
        );
    }

    #[cfg(feature = "runtime-tokio")]
    #[tokio::test]
    async fn round2_append_skips_stale_no_for_other_command() {
        // Mirrors async-imap PR #142: a completion for another request (here
        // NO) must not fail this command; only the tag may decide.
        let response = b"A9999 NO previous command failed\r\n\
                         + OK\r\n\
                         A0001 OK APPEND completed\r\n"
            .to_vec();
        let mut session = mock_session!(MockStream::new(response));
        round2_append(&mut session, "foobarbaz").await.unwrap();
        assert_eq!(
            session.stream.inner.written_buf,
            b"A0001 APPEND \"INBOX\" {9}\r\nfoobarbaz\r\n".to_vec()
        );
    }

    #[cfg(feature = "runtime-tokio")]
    #[tokio::test]
    async fn round2_append_skips_stale_bad_for_other_command() {
        let response = b"A9999 BAD previous command misparsed\r\n\
                         + OK\r\n\
                         A0001 OK APPEND completed\r\n"
            .to_vec();
        let mut session = mock_session!(MockStream::new(response));
        round2_append(&mut session, "foobarbaz").await.unwrap();
        assert_eq!(
            session.stream.inner.written_buf,
            b"A0001 APPEND \"INBOX\" {9}\r\nfoobarbaz\r\n".to_vec()
        );
    }

    #[cfg(feature = "runtime-tokio")]
    #[tokio::test]
    async fn round2_append_eof_before_continuation_returns_error() {
        let mut session = mock_session!(MockStream::default().with_eof());
        let err = round2_append(&mut session, "foobarbaz").await.unwrap_err();
        assert!(matches!(err, Error::Append), "{err:?}");
    }

    #[cfg(feature = "runtime-tokio")]
    #[tokio::test]
    async fn round2_append_eof_after_stale_responses_returns_error() {
        // MockStream returns UnexpectedEof once its buffer is exhausted; the
        // point is that the loop must return promptly instead of waiting for
        // a continuation that will never arrive.
        let response = b"A9999 OK stale completion\r\n".to_vec();
        let mut session = mock_session!(MockStream::new(response));
        let err = round2_append(&mut session, "foobarbaz").await.unwrap_err();
        assert!(
            matches!(err, Error::Io(_) | Error::Append | Error::ConnectionLost),
            "{err:?}"
        );
    }

    #[cfg(feature = "runtime-tokio")]
    #[tokio::test]
    async fn round2_append_no_after_continuation_returns_error() {
        let response = b"+ OK\r\nA0001 NO [TRYCREATE] Mailbox doesn't exist\r\n".to_vec();
        let mut session = mock_session!(MockStream::new(response));
        let err = round2_append(&mut session, "foobarbaz").await.unwrap_err();
        assert!(matches!(err, Error::No(_)), "{err:?}");
    }

    #[cfg(feature = "runtime-tokio")]
    #[tokio::test]
    async fn round2_append_bad_after_continuation_returns_error() {
        let response = b"+ OK\r\nA0001 BAD Invalid arguments\r\n".to_vec();
        let mut session = mock_session!(MockStream::new(response));
        let err = round2_append(&mut session, "foobarbaz").await.unwrap_err();
        assert!(matches!(err, Error::Bad(_)), "{err:?}");
    }

    #[cfg(feature = "runtime-tokio")]
    #[tokio::test]
    async fn round2_append_own_tagged_ok_without_continuation_returns_error() {
        // A tagged completion without the synchronizing continuation must not
        // be treated as a continuation (and must not be swallowed).
        let response = b"A0001 OK APPEND completed\r\n".to_vec();
        let mut session = mock_session!(MockStream::new(response));
        let err = round2_append(&mut session, "foobarbaz").await.unwrap_err();
        assert!(matches!(err, Error::Append), "{err:?}");
        assert_eq!(
            session.stream.inner.written_buf,
            b"A0001 APPEND \"INBOX\" {9}\r\n".to_vec()
        );
    }

    #[cfg(feature = "runtime-tokio")]
    #[tokio::test]
    async fn round2_append_stale_untagged_after_continuation_still_completes() {
        let response = b"+ OK\r\n\
                         * 8 EXISTS\r\n\
                         * 3 RECENT\r\n\
                         A0001 OK APPEND completed\r\n"
            .to_vec();
        let mut session = mock_session!(MockStream::new(response));
        round2_append(&mut session, "foobarbaz").await.unwrap();
        assert_eq!(
            session.stream.inner.written_buf,
            b"A0001 APPEND \"INBOX\" {9}\r\nfoobarbaz\r\n".to_vec()
        );
    }

    #[cfg(feature = "runtime-tokio")]
    #[tokio::test]
    async fn round2_append_stale_tagged_ok_after_continuation_is_not_swallowed() {
        // A stale OK for another tag after "+" must not be mistaken for this
        // APPEND's completion; the matching tagged OK still completes it.
        let response = b"+ OK\r\n\
                         A9999 OK completion for another command\r\n\
                         A0001 OK APPEND completed\r\n"
            .to_vec();
        let mut session = mock_session!(MockStream::new(response));
        round2_append(&mut session, "foobarbaz").await.unwrap();
        assert_eq!(
            session.stream.inner.written_buf,
            b"A0001 APPEND \"INBOX\" {9}\r\nfoobarbaz\r\n".to_vec()
        );
    }

    #[cfg(feature = "runtime-tokio")]
    #[tokio::test]
    async fn round2_append_preserves_unsolicited_before_continuation() {
        let response =
            b"* 8 EXISTS\r\n* 3 RECENT\r\n+ OK\r\nA0001 OK APPEND completed\r\n".to_vec();
        let mut session = mock_session!(MockStream::new(response));
        round2_append(&mut session, "foobarbaz").await.unwrap();
        assert_eq!(
            session.unsolicited_responses.try_recv().unwrap(),
            UnsolicitedResponse::Exists(8)
        );
        assert_eq!(
            session.unsolicited_responses.try_recv().unwrap(),
            UnsolicitedResponse::Recent(3)
        );
    }

    #[cfg(feature = "runtime-tokio")]
    #[tokio::test]
    async fn round2_append_bye_must_not_send_literal() {
        let response = b"* BYE server closing\r\n+ OK\r\nA0001 OK APPEND completed\r\n".to_vec();
        let mut session = mock_session!(MockStream::new(response));
        assert!(round2_append(&mut session, "foobarbaz").await.is_err());
        assert_eq!(
            session.stream.inner.written_buf,
            b"A0001 APPEND \"INBOX\" {9}\r\n".to_vec()
        );
    }

use super::*;

fn comment(author: &str, body: &str) -> IssueComment {
    serde_json::from_value(json!({"id":5805056057u64,"user":{"login":author},"body":body,"html_url":"https://github.com/AstrBotDevs/AstrBot/pull/10194#issuecomment-5805056057"})).unwrap()
}
fn target() -> Target {
    Target::parse("https://github.com/AstrBotDevs/AstrBot/pull/10194#issuecomment-5805056057")
        .unwrap()
}
#[test]
fn permissions_urls_and_original_request() {
    let config = Config::default();
    for (repo, author, body, reason) in [
        (
            "AstrBotDevs/AstrBot",
            "whatevertogo",
            "@whatevertogo 告诉我为啥单元测试失败了喵",
            None,
        ),
        (
            "AstrBotDevs/AstrBot",
            "stranger",
            "@whatevertogo review",
            Some("author_not_allowed"),
        ),
        (
            "VitaDynamics/Vvbot",
            "stranger",
            "@whatevertogo review",
            None,
        ),
        (
            "whatevertogo/astrcodey",
            "stranger",
            "@WhateverToGo review",
            None,
        ),
        (
            "VitaDynamics/Vvbot",
            "stranger",
            "@whatevertogo-extra review",
            Some("mention_missing"),
        ),
        (
            "VitaDynamics/Vvbot",
            "stranger",
            "mail@whatevertogo review",
            Some("mention_missing"),
        ),
        (
            "VitaDynamics/Vvbot",
            "whatevertogo",
            "<!-- astrcode-review-summary:v2 --> @whatevertogo",
            Some("agent_comment"),
        ),
    ] {
        assert_eq!(
            rejection(&config, repo, &comment(author, body)),
            reason,
            "{repo} {author} {body}"
        );
    }
    let mut restricted = config.clone();
    restricted.mention_repos = Some(vec![]);
    assert_eq!(
        rejection(
            &restricted,
            "VitaDynamics/Vvbot",
            &comment("stranger", "@whatevertogo")
        ),
        Some("author_not_allowed")
    );
    assert!(!asks_for_review("@whatevertogo 告诉我为啥单元测试失败了喵"));
    for url in [
        "https://github.com/a/b/issues/1#issuecomment-2",
        "https://github.com/a/b/pull/1#discussion_r2",
        "https://evil.test/a/b/pull/1#issuecomment-2",
        "https://github.com/a/b/pull/0#issuecomment-2",
        "https://github.com/a/b/pull/1#issuecomment-2?extra",
    ] {
        assert!(Target::parse(url).is_err(), "{url}");
    }
}
fn reply(status: u16, body: Value) -> github::Reply {
    github::Reply {
        status,
        body,
        headers: BTreeMap::new(),
    }
}
#[test]
fn canonical_validation_rejects_deleted_closed_edited_and_forged_comments() {
    let config = Config::default();
    let t = target();
    for (body, author, url, open, expected) in [
        (
            "@whatevertogo why tests fail",
            "whatevertogo",
            None,
            true,
            "accepted",
        ),
        ("edited away", "whatevertogo", None, true, "mention_missing"),
        (
            "@whatevertogo",
            "stranger",
            None,
            true,
            "author_not_allowed",
        ),
        ("@whatevertogo", "whatevertogo", None, false, "pr_closed"),
        (
            "@whatevertogo",
            "whatevertogo",
            Some("https://github.com/AstrBotDevs/AstrBot/issues/10194#issuecomment-5805056057"),
            true,
            "not_pr_discussion_comment",
        ),
    ] {
        let c = comment(author, body);
        let mut calls = 0;
        let result=validate_with(&config,&t,|_,_| {
            calls+=1;
            Ok(if calls==1 {reply(200,json!({"id":c.id,"body":c.body,"user":{"login":author},"html_url":url.or(c.html_url.as_deref())}))}
            else {reply(200,json!({"state":if open {"open"}else{"closed"},"title":"tests","html_url":"https://github.com/AstrBotDevs/AstrBot/pull/10194","head":{"sha":"fixed"},"base":{"ref":"master"}}))})
        }).unwrap();
        match result {
            Validation::Accepted(v) => {
                assert_eq!(expected, "accepted");
                assert_eq!(v.trigger.comment().unwrap().body.as_deref(), Some(body));
            }
            Validation::Rejected(reason) => assert_eq!(reason, expected),
        }
    }
    for status in [401, 403, 404, 410, 429] {
        let result = validate_with(&config, &t, |_, _| {
            Err(github::ApiFailure {
                status,
                retry_at: now_epoch() + 900,
                message: "fixture".into(),
            }
            .into())
        });
        if matches!(status, 404 | 410) {
            assert!(matches!(result.unwrap(), Validation::Rejected(_)));
        } else {
            let e = result.err().unwrap();
            assert!(github::retry_at(&e, 1) >= now_epoch() + 899);
        }
    }
    let wrong = validate_with(&config, &t, |_, _| {
        Ok(reply(
            200,
            json!({"html_url":"https://github.com/AstrBotDevs/AstrBot/pull/10195#issuecomment-5805056057"}),
        ))
    });
    assert!(wrong.is_err());
    let cached = github::parse_response(
        "HTTP/2.0 304 Not Modified\r\nETag: abc\r\nX-Poll-Interval: 120\r\n\r\n",
    )
    .unwrap();
    assert_eq!(cached.status, 304);
    assert_eq!(cached.headers["etag"], "abc");
}

#[tokio::test]
async fn durable_inbox_survives_busy_worker_duplicates_and_restart_boundaries() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("concurrent.json");
    let handles = (0..16)
        .map(|id| {
            let path = path.clone();
            std::thread::spawn(move || inbox::atomic_create(&path, &json!({"writer":id})).unwrap())
        })
        .collect::<Vec<_>>();
    assert_eq!(
        handles
            .into_iter()
            .map(|h| usize::from(h.join().unwrap()))
            .sum::<usize>(),
        1
    );
    assert!(
        serde_json::from_slice::<Value>(&fs::read(path).unwrap()).unwrap()["writer"].is_number()
    );
    RUN_DATA_DIR
        .scope(temp.path().to_owned(), async {
            let t = target();
            let mut state = State::default();
            save_state(&state).unwrap();
            let lock = OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(temp.path().join("run.lock"))
                .unwrap();
            lock.lock_exclusive().unwrap();
            // Holding the model executor lock does not prevent receipt.
            for source in ["activity", "repo", "search", "webhook", "cli"] {
                assert_eq!(inbox::persist(&t, source).unwrap(), source == "activity");
            }
            assert!(inbox::contains(&t).unwrap());
            assert_eq!(import_mentions(&mut state).unwrap(), 1);
            assert!(!inbox::contains(&t).unwrap());
            assert_eq!(read_state().unwrap().processed_comments.len(), 1);
            // Crash after state commit but before unlink: replaying the ingress file is harmless.
            inbox::persist(&t, "replayed-after-crash").unwrap();
            state = read_state().unwrap();
            assert_eq!(import_mentions(&mut state).unwrap(), 0);
            assert_eq!(state.processed_comments.len(), 1);
            let pending = inbox::next_pending_with(&mut state, |_| {
                Err(github::ApiFailure {
                    status: 403,
                    retry_at: now_epoch() + 60,
                    message: "rate limited".into(),
                }
                .into())
            })
            .unwrap();
            assert!(pending.is_none());
            assert_eq!(existing(&state, &t).unwrap().status, STATUS_PENDING);
            inbox::next_pending_with(&mut state, |_| {
                panic!("backoff must avoid another API call")
            })
            .unwrap();
            // A processed comment stays processed after editing / resubmitting from any source.
            state.processed_comments.get_mut(&t.key()).unwrap().status = STATUS_COMMENTED.into();
            save_state(&state).unwrap();
            inbox::persist(&t, "cli").unwrap();
            assert_eq!(import_mentions(&mut state).unwrap(), 0);
            assert_eq!(
                disposition(existing(&state, &t).unwrap()),
                "already_processed"
            );
            inbox::next_pending_with(&mut state, |_| panic!("processed tasks must not execute"))
                .unwrap();
            // Legacy crash-claimed spool and active spool both migrate, preserving delivery IDs.
            let event = SpooledWebhookEvent {
                event: "ping".into(),
                delivery_id: "old".into(),
                payload: json!({}),
                spooled_at: 1,
            };
            fs::write(
                webhook_spool_path().unwrap(),
                format!("{}\n", serde_json::to_string(&event).unwrap()),
            )
            .unwrap();
            fs::write(
                temp.path().join("webhook-events.imported-123.jsonl"),
                serde_json::to_string(&event).unwrap(),
            )
            .unwrap();
            assert_eq!(import_webhooks(&Config::default(), &mut state).unwrap(), 1);
            assert!(state.webhook_deliveries.contains_key("old"));
            persist_webhook("ping", "old", &json!({})).unwrap();
            import_webhooks(&Config::default(), &mut state).unwrap();
            assert_eq!(state.webhook_deliveries.len(), 1);
            fs::write(state_path().unwrap(), "broken JSON").unwrap();
            assert!(load_state().is_err());
            assert!(read_state().is_err());
        })
        .await;
}

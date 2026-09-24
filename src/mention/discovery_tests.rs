use super::*;
use std::collections::VecDeque;

#[derive(Default)]
struct Fake {
    responses: VecDeque<Result<github::Reply>>,
    received: Vec<Target>,
    requests: Vec<(String, Option<String>)>,
    fail_receipt: bool,
}
impl Transport for Fake {
    fn get(&mut self, url: &str, etag: Option<&str>) -> Result<github::Reply> {
        self.requests.push((url.into(), etag.map(str::to_owned)));
        self.responses.pop_front().expect("unexpected API request")
    }
    fn receive(&mut self, target: &Target, _: &str) -> Result<()> {
        anyhow::ensure!(!self.fail_receipt, "disk write failed");
        self.received.push(target.clone());
        Ok(())
    }
}
fn reply(body: Value, next: bool) -> Result<github::Reply> {
    Ok(github::Reply {
        status: 200,
        body,
        headers: if next {
            BTreeMap::from([("link".into(), "<next>; rel=\"next\"".into())])
        } else {
            BTreeMap::new()
        },
    })
}
fn comment(id: u64) -> Value {
    json!({"id":id,"html_url":format!("https://github.com/AstrBotDevs/AstrBot/pull/10194#issuecomment-{id}"),"body":"@whatevertogo 告诉我为啥单元测试失败了喵","user":{"login":"whatevertogo"}})
}
fn event(id: u64, at: u64) -> Value {
    json!({"id":id.to_string(),"created_at":github::timestamp(at).unwrap(),"type":"IssueCommentEvent","payload":{"action":"created","issue":{"pull_request":{}},"comment":comment(id)}})
}
#[test]
fn empty_search_does_not_block_out_of_order_activity_or_repo_pages() {
    let config = Config::default();
    let source = Source::Activity("whatevertogo".into());
    let mut p = Progress::new(&source);
    let mut fake = Fake {
        responses: VecDeque::from([
            reply(json!({"items":[],"total_count":0}), false),
            reply(
                json!([event(1, p.since - 10), event(5805056057, p.since + 1)]),
                true,
            ),
            reply(json!([event(2, p.since + 2)]), false),
            reply(json!([comment(3)]), true),
            reply(json!([comment(4)]), false),
        ]),
        ..Fake::default()
    };
    scan(
        &mut fake,
        &config,
        &Source::Search,
        &mut Progress::new(&Source::Search),
    )
    .unwrap();
    assert!(fake.received.is_empty());
    scan(&mut fake, &config, &source, &mut p).unwrap();
    assert_eq!(
        fake.received.iter().map(|t| t.comment).collect::<Vec<_>>(),
        [5805056057, 2]
    );
    let repo = Source::Repo("VitaDynamics/Vvbot".into());
    let mut rp = Progress::new(&repo);
    let previous = rp.since;
    rp.last_success = Some(previous);
    scan(&mut fake, &config, &repo, &mut rp).unwrap();
    assert_eq!(fake.received.len(), 4);
    assert!(fake.requests[3]
        .0
        .contains(&github::timestamp(previous - 600).unwrap()));
    assert!(rp.since >= previous);
    assert!(fake.responses.is_empty());
}
#[test]
fn conditional_pages_poll_interval_and_window_limits_are_visible() {
    let config = Config::default();
    let source = Source::Activity("whatevertogo".into());
    let mut p = Progress::new(&source);
    let mut first = reply(json!([event(1, p.since + 1)]), true).unwrap();
    first.headers.insert("etag".into(), "page-one".into());
    first.headers.insert("x-poll-interval".into(), "180".into());
    let mut fake = Fake {
        responses: VecDeque::from([Ok(first), reply(json!([event(2, p.since + 2)]), false)]),
        ..Fake::default()
    };
    scan(&mut fake, &config, &source, &mut p).unwrap();
    assert_eq!(p.min_interval, 180);
    fake.responses.extend([
        Ok(github::Reply {
            status: 304,
            headers: BTreeMap::new(),
            body: Value::Null,
        }),
        reply(json!([event(2, p.since + 2), event(3, p.since + 3)]), false),
    ]);
    scan(&mut fake, &config, &source, &mut p).unwrap();
    assert_eq!(fake.requests[2].1.as_deref(), Some("page-one"));
    assert_eq!(
        fake.received.iter().map(|t| t.comment).collect::<Vec<_>>(),
        [1, 2, 3]
    );
    for page in 0..3 {
        fake.responses.push_back(reply(
            Value::Array(
                (0..100)
                    .map(|n| event(10 + page * 100 + n, p.since + 10))
                    .collect(),
            ),
            page < 2,
        ));
    }
    scan(&mut fake, &config, &source, &mut p).unwrap();
    assert!(p.warnings.iter().any(|w| w.contains("300 events")));
    assert!(p.warnings.iter().any(|w| w.contains("no longer overlaps")));
}
#[test]
fn interrupted_scans_and_failed_persistence_do_not_commit_progress() {
    let config = Config::default();
    let source = Source::Repo("VitaDynamics/Vvbot".into());
    for status in [401, 403, 404, 429, 500] {
        let mut progress = Progress::new(&source);
        let original = progress.since;
        let mut fake = Fake {
            responses: VecDeque::from([
                reply(json!([comment(1)]), true),
                Err(github::ApiFailure {
                    status,
                    retry_at: 0,
                    message: "fixture".into(),
                }
                .into()),
            ]),
            ..Fake::default()
        };
        let mut attempt = progress.clone();
        assert!(scan(&mut fake, &config, &source, &mut attempt).is_err());
        assert_eq!(progress.since, original);
        // Next attempt resumes from the old cursor; previously received IDs are safe to replay.
        fake.responses
            .extend([reply(json!([comment(1), comment(2)]), false)]);
        scan(&mut fake, &config, &source, &mut progress).unwrap();
        assert_eq!(
            fake.received.iter().map(|t| t.comment).collect::<Vec<_>>(),
            [1, 1, 2]
        );
    }
    let mut p = Progress::new(&source);
    let old = p.since;
    let mut fake = Fake {
        responses: VecDeque::from([reply(json!([comment(1)]), false)]),
        fail_receipt: true,
        ..Fake::default()
    };
    assert!(scan(&mut fake, &config, &source, &mut p).is_err());
    assert_eq!(p.since, old);
}
#[test]
fn search_paginates_and_reports_truncation() {
    let config = Config::default();
    let mut p = Progress::new(&Source::Search);
    let mut fake = Fake {
        responses: VecDeque::from([
            reply(
                json!({"items":[],"total_count":1001,"incomplete_results":true}),
                true,
            ),
            reply(
                json!({"items":[{"repository_url":"https://api.github.com/repos/AstrBotDevs/AstrBot","number":10194}],"total_count":1001}),
                false,
            ),
            reply(json!([comment(5805056057)]), false),
        ]),
        ..Fake::default()
    };
    scan(&mut fake, &config, &Source::Search, &mut p).unwrap();
    assert_eq!(fake.received[0].pr, 10194);
    assert!(fake.requests[1].0.ends_with("page=2"));
    assert!(p.warnings.iter().any(|w| w.contains("incomplete_results")));
    assert!(p.warnings.iter().any(|w| w.contains("1000-result")));
}

//! Discord destination tests: one labelled, non-pinging message per click, with redacted,
//! actionable results. Runs against a local mock server only.

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use besideme::webhook_test::{TestDestination, TestOutcome, TestSettings, send_test, validate_url};

const TOKEN: &str = "super_secret_token_value";

fn local() -> TestSettings {
    TestSettings {
        allow_any_webhook: true,
        timeout: Duration::from_secs(5),
        ..TestSettings::default()
    }
}

fn url(server: &MockServer) -> String {
    format!("{}/api/webhooks/100/{TOKEN}", server.uri())
}

async fn answer(server: &MockServer, template: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path(format!("/api/webhooks/100/{TOKEN}")))
        .respond_with(template)
        .mount(server)
        .await;
}

#[tokio::test]
async fn each_destination_sends_one_labelled_message_that_cannot_ping() {
    for (dest, username, label) in [
        (
            TestDestination::Yamada,
            "Yamada",
            "Yamada · chapter updates destination",
        ),
        (
            TestDestination::Sasaki,
            "Sasaki",
            "Sasaki · health alerts destination",
        ),
    ] {
        let server = MockServer::start().await;
        answer(
            &server,
            ResponseTemplate::new(200).set_body_json(json!({"id": "1"})),
        )
        .await;
        let result = send_test(dest, &url(&server), &local()).await;
        assert!(result.delivered(), "{result:?}");
        assert!(result.message().contains(username));

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1, "exactly one message per test");
        assert!(requests[0].url.query().unwrap().contains("wait=true"));
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["username"], username);
        assert!(
            body["content"].is_null(),
            "no message text that could mention anyone"
        );
        assert_eq!(body["allowed_mentions"]["parse"], json!([]));
        assert!(
            body["embeds"][0]["description"]
                .as_str()
                .unwrap()
                .contains(label.split(" · ").next().unwrap())
        );
    }
}

#[tokio::test]
async fn revoked_and_missing_webhooks_get_actionable_feedback() {
    for status in [401, 403, 404] {
        let server = MockServer::start().await;
        answer(&server, ResponseTemplate::new(status)).await;
        let result = send_test(TestDestination::Yamada, &url(&server), &local()).await;
        assert_eq!(result.outcome, TestOutcome::Revoked { status });
        assert!(!result.delivered());
        let message = result.message();
        assert!(message.contains("Channel Settings"), "{message}");
        assert!(message.contains(&status.to_string()));
    }
}

#[tokio::test]
async fn other_rejections_server_errors_and_rate_limits_are_distinguished() {
    let server = MockServer::start().await;
    answer(
        &server,
        ResponseTemplate::new(400).set_body_string("bad payload"),
    )
    .await;
    let rejected = send_test(TestDestination::Sasaki, &url(&server), &local()).await;
    assert_eq!(rejected.outcome, TestOutcome::Rejected { status: 400 });

    let server = MockServer::start().await;
    answer(&server, ResponseTemplate::new(502)).await;
    let down = send_test(TestDestination::Sasaki, &url(&server), &local()).await;
    assert_eq!(down.outcome, TestOutcome::Unreachable);
    assert!(down.message().contains("internet connection"));

    // A rate limit is reported at once instead of being waited out.
    let server = MockServer::start().await;
    answer(
        &server,
        ResponseTemplate::new(429).set_body_json(json!({"retry_after": 2.2})),
    )
    .await;
    let started = Instant::now();
    let limited = send_test(TestDestination::Yamada, &url(&server), &local()).await;
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(
        limited.outcome,
        TestOutcome::RateLimited {
            retry_after_secs: 3
        }
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    assert!(limited.message().contains("3 s"));
}

#[tokio::test]
async fn an_unreachable_host_is_reported_as_unreachable() {
    // Port 9 (discard) on loopback is refused immediately on a normal machine.
    let result = send_test(
        TestDestination::Yamada,
        "http://127.0.0.1:9/api/webhooks/100/tok",
        &local(),
    )
    .await;
    assert_eq!(result.outcome, TestOutcome::Unreachable);
}

#[tokio::test]
async fn invalid_urls_are_refused_before_anything_is_sent() {
    let server = MockServer::start().await;
    answer(&server, ResponseTemplate::new(200)).await;
    let strict = TestSettings::default();
    for bad in [
        "",
        "   ",
        "not a url",
        "https://example.com/api/webhooks/1/abc",
        "http://discord.com/api/webhooks/1/abc",
        "https://discord.com/api/webhooks/abc/def",
        "https://discord.com/api/webhooks/1/abc extra",
    ] {
        let result = send_test(TestDestination::Yamada, bad, &strict).await;
        assert!(
            matches!(result.outcome, TestOutcome::InvalidUrl(_)),
            "{bad}: {result:?}"
        );
        assert!(!result.delivered());
    }
    // The mock server is only reachable with the loopback allowance, so nothing was sent.
    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(
        validate_url(TestDestination::Sasaki, "", false)
            .unwrap_err()
            .contains("Sasaki")
    );
    assert!(
        validate_url(
            TestDestination::Yamada,
            "https://discord.com/api/webhooks/1/abc",
            false
        )
        .is_ok()
    );
}

#[tokio::test]
async fn no_result_message_contains_the_webhook_url_or_token() {
    let server = MockServer::start().await;
    for status in [200, 400, 404, 500] {
        server.reset().await;
        answer(&server, ResponseTemplate::new(status)).await;
        let result = send_test(TestDestination::Yamada, &url(&server), &local()).await;
        let rendered = format!("{} | {result:?}", result.message());
        assert!(!rendered.contains(TOKEN), "{rendered}");
        assert!(!rendered.contains(&server.uri()), "{rendered}");
    }
    let invalid = send_test(
        TestDestination::Yamada,
        &format!("https://discord.com/api/webhooks/x/{TOKEN}"),
        &TestSettings::default(),
    )
    .await;
    assert!(!format!("{} {invalid:?}", invalid.message()).contains(TOKEN));
}

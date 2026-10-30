use mockito::{Matcher, Server};
use serde_json::json;
use std::fs;
use std::path::Path;
use svm::{ReleaseCache, ReleaseLookup, fetch_release_assets, fetch_releases_impl};
use tempfile::TempDir;

fn make_releases(n: usize) -> Vec<serde_json::Value> {
    (0..n)
        .map(|i| json!({"tag_name": format!("mainnet-v1.{}.0", i)}))
        .collect()
}

fn write_cache(svm_dir: &Path, source: &str, cache: &ReleaseCache) {
    let cache_dir = svm_dir.join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    let mut stored = serde_json::to_value(cache).unwrap();
    stored["source"] = json!(source);
    fs::write(
        cache_dir.join("releases.json"),
        serde_json::to_string(&stored).unwrap(),
    )
    .unwrap();
}

fn read_cache(svm_dir: &Path) -> Option<ReleaseCache> {
    fs::read_to_string(svm_dir.join("cache/releases.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

fn test_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .user_agent("svm-test")
        .build()
        .unwrap()
}

fn page_matcher(page: u32) -> Matcher {
    Matcher::AllOf(vec![
        Matcher::UrlEncoded("per_page".into(), "100".into()),
        Matcher::UrlEncoded("page".into(), page.to_string()),
    ])
}

// --- Cache structure tests ---

#[test]
fn cache_serialization_roundtrip() {
    let cache = ReleaseCache {
        etag: Some("\"abc123\"".into()),
        pages: 2,
        releases: make_releases(5),
    };
    let json = serde_json::to_string(&cache).unwrap();
    let parsed: ReleaseCache = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.etag, Some("\"abc123\"".into()));
    assert_eq!(parsed.pages, 2);
    assert_eq!(parsed.releases.len(), 5);
}

#[test]
fn cache_serialization_roundtrip_no_etag() {
    let cache = ReleaseCache {
        etag: None,
        pages: 1,
        releases: make_releases(3),
    };
    let json = serde_json::to_string(&cache).unwrap();
    let parsed: ReleaseCache = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.etag, None);
    assert_eq!(parsed.pages, 1);
    assert_eq!(parsed.releases.len(), 3);
}

#[test]
fn corrupted_cache_treated_as_empty() {
    let tmp = TempDir::new().unwrap();
    let cache_dir = tmp.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    fs::write(cache_dir.join("releases.json"), "not valid json{{{").unwrap();

    let mut server = Server::new();
    let releases = make_releases(5);
    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(200)
        .with_header("etag", "\"new\"")
        .with_body(serde_json::to_string(&releases).unwrap())
        .create();

    let url = format!("{}/releases", server.url());
    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 1).unwrap();
    assert_eq!(result.len(), 5);
}

// --- Fresh fetch tests ---

#[test]
fn fresh_fetch_no_cache_writes_cache() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();
    let releases = make_releases(10);

    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(200)
        .with_header("etag", "\"fresh-etag\"")
        .with_body(serde_json::to_string(&releases).unwrap())
        .create();

    let url = format!("{}/releases", server.url());
    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 1).unwrap();
    assert_eq!(result.len(), 10);

    let saved = read_cache(tmp.path()).unwrap();
    assert_eq!(saved.pages, 1);
    assert_eq!(saved.etag, Some("\"fresh-etag\"".into()));
    assert_eq!(saved.releases.len(), 10);
}

#[test]
fn fresh_fetch_multi_page_stops_on_partial_last_page() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();

    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(200)
        .with_header("etag", "\"e\"")
        .with_body(serde_json::to_string(&make_releases(100)).unwrap())
        .create();

    server
        .mock("GET", "/releases")
        .match_query(page_matcher(2))
        .with_status(200)
        .with_body(serde_json::to_string(&make_releases(30)).unwrap())
        .create();

    // Page 3 should NOT be requested
    let m3 = server
        .mock("GET", "/releases")
        .match_query(page_matcher(3))
        .expect(0)
        .create();

    let url = format!("{}/releases", server.url());
    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 3).unwrap();
    assert_eq!(result.len(), 130);
    m3.assert();
}

// --- ETag / cache hit tests ---

#[test]
fn etag_304_returns_cached_data() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();
    let url = format!("{}/releases", server.url());
    write_cache(
        tmp.path(),
        &url,
        &ReleaseCache {
            etag: Some("\"my-etag\"".into()),
            pages: 2,
            releases: make_releases(200),
        },
    );

    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .match_header("If-None-Match", "\"my-etag\"")
        .with_status(304)
        .create();
    let second_page = server
        .mock("GET", "/releases")
        .match_query(page_matcher(2))
        .with_body(serde_json::to_string(&make_releases(200)[100..]).unwrap())
        .expect(1)
        .create();

    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 2).unwrap();
    assert_eq!(result.len(), 200);
    second_page.assert();
}

#[test]
fn etag_304_truncates_to_requested_pages() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();
    let url = format!("{}/releases", server.url());
    write_cache(
        tmp.path(),
        &url,
        &ReleaseCache {
            etag: Some("\"e\"".into()),
            pages: 3,
            releases: make_releases(300),
        },
    );

    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(304)
        .create();

    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 1).unwrap();
    assert_eq!(result.len(), 100);
}

#[test]
fn etag_304_truncate_when_cache_has_fewer_than_page_boundary() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();
    let url = format!("{}/releases", server.url());
    // Cache says 2 pages but only has 150 releases (page 2 was partial)
    write_cache(
        tmp.path(),
        &url,
        &ReleaseCache {
            etag: Some("\"e\"".into()),
            pages: 2,
            releases: make_releases(150),
        },
    );

    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(304)
        .expect_at_least(1)
        .create();
    let second_page = server
        .mock("GET", "/releases")
        .match_query(page_matcher(2))
        .with_body(serde_json::to_string(&make_releases(150)[100..]).unwrap())
        .expect(1)
        .create();

    // limit=100, cache has 150 -> 100
    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 1).unwrap();
    assert_eq!(result.len(), 100);

    // limit=200, cache has 150 -> 150
    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 2).unwrap();
    assert_eq!(result.len(), 150);
    second_page.assert();
}

#[test]
fn unchanged_first_page_refreshes_edited_older_release_metadata_within_page_limit() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();
    let url = format!("{}/releases", server.url());
    let original = make_releases(300);
    write_cache(
        tmp.path(),
        &url,
        &ReleaseCache {
            etag: Some("\"same-first-page\"".into()),
            pages: 3,
            releases: original.clone(),
        },
    );
    let first_page = server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .match_header("if-none-match", "\"same-first-page\"")
        .with_status(304)
        .expect(1)
        .create();
    let mut edited = original[100..200].to_vec();
    edited[0]["assets"] = json!([{"name": "newly-published-archive.tgz"}]);
    let second_page = server
        .mock("GET", "/releases")
        .match_query(page_matcher(2))
        .with_body(serde_json::to_vec(&edited).unwrap())
        .expect(1)
        .create();
    let third_page = server
        .mock("GET", "/releases")
        .match_query(page_matcher(3))
        .expect(0)
        .create();
    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 2).unwrap();
    let expected = [original[..100].to_vec(), edited].concat();
    assert_eq!(
        result, expected,
        "an unchanged first page must not hide changes to older releases"
    );
    let saved = read_cache(tmp.path()).unwrap();
    assert_eq!(saved.releases, expected);
    assert_eq!(saved.pages, 2);
    assert_eq!(saved.etag.as_deref(), Some("\"same-first-page\""));
    first_page.assert();
    second_page.assert();
    third_page.assert();
}

#[test]
fn unchanged_first_page_later_failure_preserves_complete_original_snapshot() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();
    let url = format!("{}/releases", server.url());
    let original = make_releases(300);
    write_cache(
        tmp.path(),
        &url,
        &ReleaseCache {
            etag: Some("\"same-first-page\"".into()),
            pages: 3,
            releases: original.clone(),
        },
    );
    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(304)
        .create();
    let mut edited = original[100..200].to_vec();
    edited[0]["assets"] = json!([{"name": "new-asset"}]);
    let second_page = server
        .mock("GET", "/releases")
        .match_query(page_matcher(2))
        .with_body(serde_json::to_vec(&edited).unwrap())
        .expect(1)
        .create();
    let third_page = server
        .mock("GET", "/releases")
        .match_query(page_matcher(3))
        .with_status(503)
        .expect(1)
        .create();
    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 3).unwrap();
    assert_eq!(
        result, original,
        "fresh and stale pages must not be combined after a failure"
    );
    assert_eq!(read_cache(tmp.path()).unwrap().releases, original);
    second_page.assert();
    third_page.assert();
}

#[test]
fn unchanged_short_first_page_needs_no_later_requests() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();
    let url = format!("{}/releases", server.url());
    let original = make_releases(50);
    write_cache(
        tmp.path(),
        &url,
        &ReleaseCache {
            etag: Some("\"short-first-page\"".into()),
            pages: 3,
            releases: original.clone(),
        },
    );
    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(304)
        .create();
    let second_page = server
        .mock("GET", "/releases")
        .match_query(page_matcher(2))
        .expect(0)
        .create();
    assert_eq!(
        fetch_releases_impl(&test_client(), &url, tmp.path(), 3).unwrap(),
        original
    );
    second_page.assert();
}

#[test]
fn unchanged_first_page_short_later_page_removes_stale_tail_and_stops_fetching() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();
    let url = format!("{}/releases", server.url());
    let original = make_releases(300);
    write_cache(
        tmp.path(),
        &url,
        &ReleaseCache {
            etag: Some("\"same-first-page\"".into()),
            pages: 3,
            releases: original.clone(),
        },
    );
    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(304)
        .create();
    let second_page = server
        .mock("GET", "/releases")
        .match_query(page_matcher(2))
        .with_body(serde_json::to_vec(&original[100..125]).unwrap())
        .expect(1)
        .create();
    let third_page = server
        .mock("GET", "/releases")
        .match_query(page_matcher(3))
        .expect(0)
        .create();
    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 3).unwrap();
    assert_eq!(result, original[..125]);
    assert_eq!(read_cache(tmp.path()).unwrap().releases, result);
    second_page.assert();
    third_page.assert();
}

#[test]
fn zero_requested_pages_does_not_fetch_or_change_cache() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();
    let url = format!("{}/releases", server.url());
    let original = make_releases(200);
    write_cache(
        tmp.path(),
        &url,
        &ReleaseCache {
            etag: Some("\"existing\"".into()),
            pages: 2,
            releases: original.clone(),
        },
    );
    let request = server
        .mock("GET", "/releases")
        .match_query(Matcher::Any)
        .expect(0)
        .create();
    assert!(
        fetch_releases_impl(&test_client(), &url, tmp.path(), 0)
            .unwrap()
            .is_empty()
    );
    assert_eq!(read_cache(tmp.path()).unwrap().releases, original);
    request.assert();
}

#[test]
fn etag_changed_triggers_full_refetch() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();
    let url = format!("{}/releases", server.url());
    write_cache(
        tmp.path(),
        &url,
        &ReleaseCache {
            etag: Some("\"old-etag\"".into()),
            pages: 1,
            releases: make_releases(50),
        },
    );

    let new_releases = make_releases(75);

    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(200)
        .with_header("etag", "\"new-etag\"")
        .with_body(serde_json::to_string(&new_releases).unwrap())
        .expect(1) // ETag check response is reused as page 1 — no second request
        .create();

    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 1).unwrap();
    assert_eq!(result.len(), 75);

    let saved = read_cache(tmp.path()).unwrap();
    assert_eq!(saved.etag, Some("\"new-etag\"".into()));
    assert_eq!(saved.releases.len(), 75);
}

#[test]
fn etag_changed_multi_page_reuses_first_response_and_continues() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();
    let url = format!("{}/releases", server.url());
    write_cache(
        tmp.path(),
        &url,
        &ReleaseCache {
            etag: Some("\"old\"".into()),
            pages: 2,
            releases: make_releases(150),
        },
    );

    // Page 1: served once for the ETag check, reused as refetch page 1
    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(200)
        .with_header("etag", "\"new\"")
        .with_body(serde_json::to_string(&make_releases(100)).unwrap())
        .expect(1)
        .create();

    server
        .mock("GET", "/releases")
        .match_query(page_matcher(2))
        .with_status(200)
        .with_body(serde_json::to_string(&make_releases(60)).unwrap())
        .expect(1)
        .create();

    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 2).unwrap();
    assert_eq!(result.len(), 160);

    let saved = read_cache(tmp.path()).unwrap();
    assert_eq!(saved.etag, Some("\"new\"".into()));
    assert_eq!(saved.releases.len(), 160);
}

#[test]
fn etag_changed_short_first_page_stops_pagination() {
    // Reused page 1 has < 100 items → no page 2 request even with max_pages=3
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();
    let url = format!("{}/releases", server.url());
    write_cache(
        tmp.path(),
        &url,
        &ReleaseCache {
            etag: Some("\"old\"".into()),
            pages: 3,
            releases: make_releases(300),
        },
    );

    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(200)
        .with_header("etag", "\"new\"")
        .with_body(serde_json::to_string(&make_releases(40)).unwrap())
        .expect(1)
        .create();

    let m2 = server
        .mock("GET", "/releases")
        .match_query(page_matcher(2))
        .expect(0)
        .create();

    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 3).unwrap();
    assert_eq!(result.len(), 40);
    m2.assert();
}

// --- Page count awareness ---

#[test]
fn cache_fewer_pages_bypasses_etag_and_fetches() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();
    let url = format!("{}/releases", server.url());
    write_cache(
        tmp.path(),
        &url,
        &ReleaseCache {
            etag: Some("\"e\"".into()),
            pages: 1,
            releases: make_releases(100),
        },
    );

    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(200)
        .with_header("etag", "\"e2\"")
        .with_body(serde_json::to_string(&make_releases(100)).unwrap())
        .create();

    server
        .mock("GET", "/releases")
        .match_query(page_matcher(2))
        .with_status(200)
        .with_body(serde_json::to_string(&make_releases(40)).unwrap())
        .create();

    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 2).unwrap();
    assert_eq!(result.len(), 140);

    let saved = read_cache(tmp.path()).unwrap();
    assert_eq!(saved.pages, 2);
}

#[test]
fn no_etag_in_cache_does_full_fetch() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();
    let url = format!("{}/releases", server.url());
    write_cache(
        tmp.path(),
        &url,
        &ReleaseCache {
            etag: None,
            pages: 2,
            releases: make_releases(200),
        },
    );

    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(200)
        .with_header("etag", "\"new\"")
        .with_body(serde_json::to_string(&make_releases(20)).unwrap())
        .create();

    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 1).unwrap();
    assert_eq!(result.len(), 20);
}

// --- Fallback behavior ---

#[test]
fn api_error_with_cache_falls_back() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();
    let url = format!("{}/releases", server.url());
    write_cache(
        tmp.path(),
        &url,
        &ReleaseCache {
            etag: None,
            pages: 1,
            releases: make_releases(80),
        },
    );

    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(403)
        .with_body("rate limited")
        .create();

    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 1).unwrap();
    assert_eq!(result.len(), 80);
}

#[test]
fn api_error_with_cache_truncates_to_requested_pages() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();
    let url = format!("{}/releases", server.url());
    write_cache(
        tmp.path(),
        &url,
        &ReleaseCache {
            etag: None,
            pages: 2,
            releases: make_releases(200),
        },
    );

    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(403)
        .create();

    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 1).unwrap();
    assert_eq!(result.len(), 100);
}

#[test]
fn api_error_no_cache_returns_error() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();
    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(403)
        .create();

    let url = format!("{}/releases", server.url());
    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 1);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("403"));
}

#[test]
fn network_error_with_cache_falls_back_truncated() {
    let tmp = TempDir::new().unwrap();
    let url = "http://127.0.0.1:1/releases";
    write_cache(
        tmp.path(),
        url,
        &ReleaseCache {
            etag: Some("\"e\"".into()),
            pages: 3,
            releases: make_releases(300),
        },
    );

    let result = fetch_releases_impl(&test_client(), url, tmp.path(), 2).unwrap();
    assert_eq!(result.len(), 200);
}

#[test]
fn network_error_no_cache_returns_error() {
    let tmp = TempDir::new().unwrap();
    let url = "http://127.0.0.1:1/releases";
    let result = fetch_releases_impl(&test_client(), url, tmp.path(), 1);
    assert!(result.is_err());
}

// --- Cache write protection ---

#[test]
fn empty_success_replaces_stale_release_snapshot_and_etag() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();
    let url = format!("{}/releases", server.url());
    let original = ReleaseCache {
        etag: Some("\"e\"".into()),
        pages: 1,
        releases: make_releases(50),
    };
    write_cache(tmp.path(), &url, &original);

    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(200)
        .with_header("etag", "\"empty\"")
        .with_body("[]")
        .expect(1)
        .create();

    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 1).unwrap();
    assert_eq!(result.len(), 0);

    let saved = read_cache(tmp.path()).unwrap();
    assert!(saved.releases.is_empty());
    assert_eq!(saved.etag, Some("\"empty\"".into()));
    let revalidation = server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .match_header("if-none-match", "\"empty\"")
        .with_status(304)
        .expect(1)
        .create();
    assert!(
        fetch_releases_impl(&test_client(), &url, tmp.path(), 1)
            .unwrap()
            .is_empty()
    );
    revalidation.assert();
}

#[test]
fn fresh_empty_success_creates_empty_snapshot() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();
    let url = format!("{}/releases", server.url());
    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_body("[]")
        .create();
    assert!(
        fetch_releases_impl(&test_client(), &url, tmp.path(), 1)
            .unwrap()
            .is_empty()
    );
    let saved = read_cache(tmp.path()).expect("successful empty data should be available offline");
    assert!(saved.releases.is_empty());
    assert_eq!(saved.pages, 1);
    assert!(saved.etag.is_none());
}

#[test]
fn no_cache_file_at_all_fetches_fresh() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();
    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(200)
        .with_header("etag", "\"first\"")
        .with_body(serde_json::to_string(&make_releases(42)).unwrap())
        .create();

    let url = format!("{}/releases", server.url());
    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 1).unwrap();
    assert_eq!(result.len(), 42);

    let saved = read_cache(tmp.path()).unwrap();
    assert_eq!(saved.pages, 1);
}

// --- Mid-pagination failure ---

#[test]
fn api_error_on_page_2_discards_partial_data_and_errors() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();

    // Page 1 succeeds
    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(200)
        .with_header("etag", "\"e\"")
        .with_body(serde_json::to_string(&make_releases(100)).unwrap())
        .create();

    // Page 2 fails
    server
        .mock("GET", "/releases")
        .match_query(page_matcher(2))
        .with_status(500)
        .create();

    let url = format!("{}/releases", server.url());
    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 2);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("500"));

    // Cache should NOT be written (partial data)
    assert!(read_cache(tmp.path()).is_none());
}

#[test]
fn network_error_on_page_2_discards_partial_data_and_errors() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();

    // Page 1 succeeds on mockito
    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(200)
        .with_header("etag", "\"e\"")
        .with_body(serde_json::to_string(&make_releases(100)).unwrap())
        .create();

    // We can't easily make page 2 fail with a network error on the same server,
    // so we test that page 2+ failures still return errors (not partial data).
    // Page 2 returns 500, same behavior.
    server
        .mock("GET", "/releases")
        .match_query(page_matcher(2))
        .with_status(502)
        .create();

    let url = format!("{}/releases", server.url());
    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 2);
    assert!(result.is_err());
}

#[test]
fn api_error_on_page_2_falls_back_to_entire_existing_snapshot() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();
    let url = format!("{}/releases", server.url());
    write_cache(
        tmp.path(),
        &url,
        &ReleaseCache {
            etag: None,
            pages: 2,
            releases: make_releases(200),
        },
    );

    let fresh = json!({"tag_name": "mainnet-v9.9.9"});
    let mut page1 = make_releases(100);
    page1[0] = fresh.clone();
    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(200)
        .with_header("etag", "\"e\"")
        .with_body(serde_json::to_string(&page1).unwrap())
        .create();

    server
        .mock("GET", "/releases")
        .match_query(page_matcher(2))
        .with_status(500)
        .create();

    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 2).unwrap();
    assert_eq!(result, make_releases(200));
    assert!(
        !result.contains(&fresh),
        "never combine fresh and stale pages"
    );
    assert_eq!(read_cache(tmp.path()).unwrap().releases, result);
}

// --- Boundary: exactly 100 items ---

#[test]
fn exactly_100_items_fetches_next_page_to_confirm_end() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();

    // Page 1: exactly 100 items — code can't tell if there's more
    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(200)
        .with_header("etag", "\"e\"")
        .with_body(serde_json::to_string(&make_releases(100)).unwrap())
        .create();

    // Page 2: empty — confirms page 1 was the last
    let m2 = server
        .mock("GET", "/releases")
        .match_query(page_matcher(2))
        .with_status(200)
        .with_body("[]")
        .expect(1)
        .create();

    let url = format!("{}/releases", server.url());
    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 2).unwrap();
    assert_eq!(result.len(), 100);
    m2.assert(); // Confirms page 2 WAS requested

    let saved = read_cache(tmp.path()).unwrap();
    assert_eq!(saved.releases.len(), 100);
    assert_eq!(saved.pages, 2);
}

#[test]
fn all_pages_full_fetches_up_to_max_pages() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();

    // Both pages return exactly 100 — there might be more, but we stop at max_pages
    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(200)
        .with_header("etag", "\"e\"")
        .with_body(serde_json::to_string(&make_releases(100)).unwrap())
        .create();

    server
        .mock("GET", "/releases")
        .match_query(page_matcher(2))
        .with_status(200)
        .with_body(serde_json::to_string(&make_releases(100)).unwrap())
        .create();

    // Page 3 should NOT be requested (max_pages=2)
    let m3 = server
        .mock("GET", "/releases")
        .match_query(page_matcher(3))
        .expect(0)
        .create();

    let url = format!("{}/releases", server.url());
    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 2).unwrap();
    assert_eq!(result.len(), 200);
    m3.assert();
}

// --- ETag edge cases ---

#[test]
fn server_returns_no_etag_header_caches_with_none() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();

    // No etag header in response
    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(200)
        .with_body(serde_json::to_string(&make_releases(10)).unwrap())
        .create();

    let url = format!("{}/releases", server.url());
    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 1).unwrap();
    assert_eq!(result.len(), 10);

    let saved = read_cache(tmp.path()).unwrap();
    assert_eq!(saved.etag, None);
    // Without etag, next call will do a full fetch (no ETag shortcut)
}

#[test]
fn cache_with_pages_zero_treated_as_insufficient() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();
    let url = format!("{}/releases", server.url());
    write_cache(
        tmp.path(),
        &url,
        &ReleaseCache {
            etag: Some("\"e\"".into()),
            pages: 0,
            releases: vec![],
        },
    );

    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(200)
        .with_header("etag", "\"new\"")
        .with_body(serde_json::to_string(&make_releases(15)).unwrap())
        .create();

    // pages=0 < max_pages=1, so ETag path is skipped, full fetch happens
    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 1).unwrap();
    assert_eq!(result.len(), 15);
}

// --- Cache overwrite behavior ---

#[test]
fn fresh_fetch_overwrites_stale_cache() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();
    let url = format!("{}/releases", server.url());
    write_cache(
        tmp.path(),
        &url,
        &ReleaseCache {
            etag: None,
            pages: 1,
            releases: make_releases(50),
        },
    );

    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(200)
        .with_header("etag", "\"new\"")
        .with_body(serde_json::to_string(&make_releases(75)).unwrap())
        .create();

    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 1).unwrap();
    assert_eq!(result.len(), 75);

    let saved = read_cache(tmp.path()).unwrap();
    assert_eq!(saved.releases.len(), 75);
    assert_eq!(saved.etag, Some("\"new\"".into()));
    assert_eq!(saved.pages, 1);
}

#[test]
fn refetch_with_more_pages_updates_cache_page_count() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();
    let url = format!("{}/releases", server.url());
    write_cache(
        tmp.path(),
        &url,
        &ReleaseCache {
            etag: None,
            pages: 1,
            releases: make_releases(100),
        },
    );

    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(200)
        .with_header("etag", "\"e2\"")
        .with_body(serde_json::to_string(&make_releases(100)).unwrap())
        .create();

    server
        .mock("GET", "/releases")
        .match_query(page_matcher(2))
        .with_status(200)
        .with_body(serde_json::to_string(&make_releases(50)).unwrap())
        .create();

    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 2).unwrap();
    assert_eq!(result.len(), 150);

    let saved = read_cache(tmp.path()).unwrap();
    assert_eq!(saved.pages, 2);
    assert_eq!(saved.releases.len(), 150);
}

// --- ETag validation returns non-2xx, non-304 (e.g. 403) ---

// --- fetch_release_assets classification ---

#[test]
fn release_lookup_found_returns_assets() {
    let mut server = Server::new();
    server
        .mock("GET", "/releases/tags/mainnet-v1.0.0")
        .with_status(200)
        .with_body(
            json!({
                "tag_name": "mainnet-v1.0.0",
                "assets": [
                    {"name": "sui-mainnet-v1.0.0-macos-arm64.tgz", "digest": "sha256:abc123"},
                    {"name": "sui-mainnet-v1.0.0-ubuntu-x86_64.tgz", "digest": null}
                ]
            })
            .to_string(),
        )
        .create();

    let base = format!("{}/releases", server.url());
    match fetch_release_assets(&test_client(), &base, "mainnet-v1.0.0") {
        ReleaseLookup::Found(assets) => {
            assert_eq!(assets.len(), 2);
            assert_eq!(svm::asset_sha256(&assets[0]), Some("abc123".into()));
            assert_eq!(svm::asset_sha256(&assets[1]), None);
        }
        _ => panic!("expected Found"),
    }
}

#[test]
fn release_lookup_404_is_not_found() {
    let mut server = Server::new();
    server
        .mock("GET", "/releases/tags/mainnet-v9.9.9")
        .with_status(404)
        .create();

    let base = format!("{}/releases", server.url());
    assert!(matches!(
        fetch_release_assets(&test_client(), &base, "mainnet-v9.9.9"),
        ReleaseLookup::NotFound
    ));
}

#[test]
fn release_lookup_server_error_is_unavailable() {
    let mut server = Server::new();
    server
        .mock("GET", "/releases/tags/mainnet-v1.0.0")
        .with_status(503)
        .create();

    let base = format!("{}/releases", server.url());
    assert!(matches!(
        fetch_release_assets(&test_client(), &base, "mainnet-v1.0.0"),
        ReleaseLookup::Unavailable
    ));
}

#[test]
fn release_lookup_garbled_body_is_unavailable() {
    let mut server = Server::new();
    server
        .mock("GET", "/releases/tags/mainnet-v1.0.0")
        .with_status(200)
        .with_body("not json at all")
        .create();

    let base = format!("{}/releases", server.url());
    assert!(matches!(
        fetch_release_assets(&test_client(), &base, "mainnet-v1.0.0"),
        ReleaseLookup::Unavailable
    ));
}

// --- Stale-cache fallback on garbled page-1 body ---

#[test]
fn bad_page1_body_falls_back_to_cache() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();
    let url = format!("{}/releases", server.url());
    write_cache(
        tmp.path(),
        &url,
        &ReleaseCache {
            etag: None,
            pages: 1,
            releases: make_releases(70),
        },
    );

    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(200)
        .with_body("<html>proxy error page</html>")
        .create();

    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 1).unwrap();
    assert_eq!(result.len(), 70);
}

#[test]
fn bad_etag_refresh_body_falls_back_to_cache() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();
    let url = format!("{}/releases", server.url());
    write_cache(
        tmp.path(),
        &url,
        &ReleaseCache {
            etag: Some("\"old\"".into()),
            pages: 1,
            releases: make_releases(50),
        },
    );

    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(200)
        .with_header("etag", "\"new\"")
        .with_body("garbled")
        .create();

    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 1).unwrap();
    assert_eq!(result.len(), 50);
}

#[test]
fn etag_check_returns_403_falls_back_to_cache() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();
    let url = format!("{}/releases", server.url());
    write_cache(
        tmp.path(),
        &url,
        &ReleaseCache {
            etag: Some("\"e\"".into()),
            pages: 1,
            releases: make_releases(80),
        },
    );

    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(403)
        .create();

    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 1).unwrap();
    // The wildcard arm in the ETag match catches non-2xx non-304 and falls back
    assert_eq!(result.len(), 80);
}

#[test]
fn changing_api_source_does_not_reuse_metadata_or_etag() {
    let tmp = TempDir::new().unwrap();
    let mut first = Server::new();
    let mut second = Server::new();
    let first_url = format!("{}/releases", first.url());
    let second_url = format!("{}/releases", second.url());
    let original = make_releases(2);
    first
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_header("etag", "\"first-source\"")
        .with_body(serde_json::to_string(&original).unwrap())
        .create();
    fetch_releases_impl(&test_client(), &first_url, tmp.path(), 1).unwrap();

    let replacement = vec![json!({"tag_name": "testnet-v9.9.9"})];
    let request = second
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .match_header("if-none-match", Matcher::Missing)
        .with_body(serde_json::to_string(&replacement).unwrap())
        .expect(1)
        .create();
    let result = fetch_releases_impl(&test_client(), &second_url, tmp.path(), 1).unwrap();
    assert_eq!(result, replacement);
    request.assert();

    let stored: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(tmp.path().join("cache/releases.json")).unwrap())
            .unwrap();
    assert_eq!(stored["source"], second_url);
}

#[test]
fn failed_new_source_never_falls_back_to_previous_source() {
    let tmp = TempDir::new().unwrap();
    let mut first = Server::new();
    let mut second = Server::new();
    first
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_header("etag", "\"first-source\"")
        .with_body(serde_json::to_string(&make_releases(2)).unwrap())
        .create();
    fetch_releases_impl(
        &test_client(),
        &format!("{}/releases", first.url()),
        tmp.path(),
        1,
    )
    .unwrap();
    second
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(503)
        .create();
    let result = fetch_releases_impl(
        &test_client(),
        &format!("{}/releases", second.url()),
        tmp.path(),
        1,
    );
    assert!(result.is_err());
    assert_eq!(read_cache(tmp.path()).unwrap().releases, make_releases(2));
}

#[test]
fn legacy_unsourced_cache_is_never_sent_or_used_for_custom_api() {
    let tmp = TempDir::new().unwrap();
    let cache_dir = tmp.path().join("cache");
    fs::create_dir(&cache_dir).unwrap();
    let legacy = ReleaseCache {
        etag: Some("\"official-api-etag\"".into()),
        pages: 1,
        releases: make_releases(2),
    };
    fs::write(
        cache_dir.join("releases.json"),
        serde_json::to_vec(&legacy).unwrap(),
    )
    .unwrap();
    let mut server = Server::new();
    let request = server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .match_header("if-none-match", Matcher::Missing)
        .with_status(503)
        .expect(1)
        .create();
    let result = fetch_releases_impl(
        &test_client(),
        &format!("{}/releases", server.url()),
        tmp.path(),
        1,
    );
    assert!(
        result.is_err(),
        "mirror failures must not return unsourced metadata"
    );
    request.assert();
    assert_eq!(read_cache(tmp.path()).unwrap().releases, legacy.releases);
}

#[test]
fn cache_refresh_keeps_existing_readers_on_complete_snapshot() {
    use std::io::Read;

    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();
    let url = format!("{}/releases", server.url());
    write_cache(
        tmp.path(),
        &url,
        &ReleaseCache {
            etag: None,
            pages: 1,
            releases: make_releases(5),
        },
    );
    let cache_path = tmp.path().join("cache/releases.json");
    // A reader opened before replacement must retain its complete old file,
    // while readers opened after replacement see the complete new file.
    let mut existing_reader = fs::File::open(&cache_path).unwrap();
    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_body(serde_json::to_string(&make_releases(10)).unwrap())
        .create();
    fetch_releases_impl(
        &test_client(),
        &format!("{}/releases", server.url()),
        tmp.path(),
        1,
    )
    .unwrap();
    let mut old_contents = String::new();
    existing_reader.read_to_string(&mut old_contents).unwrap();
    assert_eq!(
        serde_json::from_str::<ReleaseCache>(&old_contents)
            .unwrap()
            .releases
            .len(),
        5
    );
    assert_eq!(read_cache(tmp.path()).unwrap().releases.len(), 10);
    assert_eq!(fs::read_dir(tmp.path().join("cache")).unwrap().count(), 1);
}

#[test]
fn malformed_later_page_falls_back_without_overwriting_snapshot() {
    let tmp = TempDir::new().unwrap();
    let mut server = Server::new();
    let url = format!("{}/releases", server.url());
    write_cache(
        tmp.path(),
        &url,
        &ReleaseCache {
            etag: None,
            pages: 2,
            releases: make_releases(150),
        },
    );
    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_body(serde_json::to_string(&make_releases(100)).unwrap())
        .create();
    server
        .mock("GET", "/releases")
        .match_query(page_matcher(2))
        .with_body("{truncated")
        .create();
    let result = fetch_releases_impl(
        &test_client(),
        &format!("{}/releases", server.url()),
        tmp.path(),
        2,
    )
    .unwrap();
    assert_eq!(result, make_releases(150));
    assert_eq!(read_cache(tmp.path()).unwrap().releases, result);
}

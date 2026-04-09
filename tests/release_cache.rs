use mockito::{Matcher, Server};
use serde_json::json;
use std::fs;
use std::path::Path;
use svm::{fetch_releases_impl, ReleaseCache};
use tempfile::TempDir;

fn make_releases(n: usize) -> Vec<serde_json::Value> {
    (0..n)
        .map(|i| json!({"tag_name": format!("mainnet-v1.{}.0", i)}))
        .collect()
}

fn write_cache(svm_dir: &Path, cache: &ReleaseCache) {
    let cache_dir = svm_dir.join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    fs::write(
        cache_dir.join("releases.json"),
        serde_json::to_string(cache).unwrap(),
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
    write_cache(
        tmp.path(),
        &ReleaseCache {
            etag: Some("\"my-etag\"".into()),
            pages: 2,
            releases: make_releases(200),
        },
    );

    let mut server = Server::new();
    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .match_header("If-None-Match", "\"my-etag\"")
        .with_status(304)
        .create();

    let url = format!("{}/releases", server.url());
    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 2).unwrap();
    assert_eq!(result.len(), 200);
}

#[test]
fn etag_304_truncates_to_requested_pages() {
    let tmp = TempDir::new().unwrap();
    write_cache(
        tmp.path(),
        &ReleaseCache {
            etag: Some("\"e\"".into()),
            pages: 3,
            releases: make_releases(300),
        },
    );

    let mut server = Server::new();
    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(304)
        .create();

    let url = format!("{}/releases", server.url());
    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 1).unwrap();
    assert_eq!(result.len(), 100);
}

#[test]
fn etag_304_truncate_when_cache_has_fewer_than_page_boundary() {
    let tmp = TempDir::new().unwrap();
    // Cache says 2 pages but only has 150 releases (page 2 was partial)
    write_cache(
        tmp.path(),
        &ReleaseCache {
            etag: Some("\"e\"".into()),
            pages: 2,
            releases: make_releases(150),
        },
    );

    let mut server = Server::new();
    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(304)
        .expect_at_least(1)
        .create();

    let url = format!("{}/releases", server.url());
    // limit=100, cache has 150 -> 100
    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 1).unwrap();
    assert_eq!(result.len(), 100);

    // limit=200, cache has 150 -> 150
    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 2).unwrap();
    assert_eq!(result.len(), 150);
}

#[test]
fn etag_changed_triggers_full_refetch() {
    let tmp = TempDir::new().unwrap();
    write_cache(
        tmp.path(),
        &ReleaseCache {
            etag: Some("\"old-etag\"".into()),
            pages: 1,
            releases: make_releases(50),
        },
    );

    let mut server = Server::new();
    let new_releases = make_releases(75);

    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(200)
        .with_header("etag", "\"new-etag\"")
        .with_body(serde_json::to_string(&new_releases).unwrap())
        .expect(2) // ETag check + full fetch
        .create();

    let url = format!("{}/releases", server.url());
    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 1).unwrap();
    assert_eq!(result.len(), 75);

    let saved = read_cache(tmp.path()).unwrap();
    assert_eq!(saved.etag, Some("\"new-etag\"".into()));
    assert_eq!(saved.releases.len(), 75);
}

// --- Page count awareness ---

#[test]
fn cache_fewer_pages_bypasses_etag_and_fetches() {
    let tmp = TempDir::new().unwrap();
    write_cache(
        tmp.path(),
        &ReleaseCache {
            etag: Some("\"e\"".into()),
            pages: 1,
            releases: make_releases(100),
        },
    );

    let mut server = Server::new();

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

    let url = format!("{}/releases", server.url());
    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 2).unwrap();
    assert_eq!(result.len(), 140);

    let saved = read_cache(tmp.path()).unwrap();
    assert_eq!(saved.pages, 2);
}

#[test]
fn no_etag_in_cache_does_full_fetch() {
    let tmp = TempDir::new().unwrap();
    write_cache(
        tmp.path(),
        &ReleaseCache {
            etag: None,
            pages: 2,
            releases: make_releases(200),
        },
    );

    let mut server = Server::new();
    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(200)
        .with_header("etag", "\"new\"")
        .with_body(serde_json::to_string(&make_releases(20)).unwrap())
        .create();

    let url = format!("{}/releases", server.url());
    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 1).unwrap();
    assert_eq!(result.len(), 20);
}

// --- Fallback behavior ---

#[test]
fn api_error_with_cache_falls_back() {
    let tmp = TempDir::new().unwrap();
    write_cache(
        tmp.path(),
        &ReleaseCache {
            etag: None,
            pages: 1,
            releases: make_releases(80),
        },
    );

    let mut server = Server::new();
    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(403)
        .with_body("rate limited")
        .create();

    let url = format!("{}/releases", server.url());
    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 1).unwrap();
    assert_eq!(result.len(), 80);
}

#[test]
fn api_error_with_cache_truncates_to_requested_pages() {
    let tmp = TempDir::new().unwrap();
    write_cache(
        tmp.path(),
        &ReleaseCache {
            etag: None,
            pages: 2,
            releases: make_releases(200),
        },
    );

    let mut server = Server::new();
    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(403)
        .create();

    let url = format!("{}/releases", server.url());
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
    write_cache(
        tmp.path(),
        &ReleaseCache {
            etag: Some("\"e\"".into()),
            pages: 3,
            releases: make_releases(300),
        },
    );

    let url = "http://127.0.0.1:1/releases";
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
fn empty_response_does_not_overwrite_cache() {
    let tmp = TempDir::new().unwrap();
    let original = ReleaseCache {
        etag: Some("\"e\"".into()),
        pages: 1,
        releases: make_releases(50),
    };
    write_cache(tmp.path(), &original);

    let mut server = Server::new();
    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(200)
        .with_header("etag", "\"empty\"")
        .with_body("[]")
        .expect_at_least(1)
        .create();

    let url = format!("{}/releases", server.url());
    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 1).unwrap();
    assert_eq!(result.len(), 0);

    let saved = read_cache(tmp.path()).unwrap();
    assert_eq!(saved.releases.len(), 50);
    assert_eq!(saved.etag, Some("\"e\"".into()));
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
fn api_error_on_page_2_with_existing_cache_does_not_fallback() {
    // Fallback to cache only happens on page 1 failure.
    // Page 2+ failure is a hard error even if cache exists.
    let tmp = TempDir::new().unwrap();
    write_cache(
        tmp.path(),
        &ReleaseCache {
            etag: None,
            pages: 2,
            releases: make_releases(200),
        },
    );

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
        .with_status(500)
        .create();

    let url = format!("{}/releases", server.url());
    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 2);
    // Does NOT fall back to cache — page 2+ errors are hard failures
    assert!(result.is_err());
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
    write_cache(
        tmp.path(),
        &ReleaseCache {
            etag: Some("\"e\"".into()),
            pages: 0,
            releases: vec![],
        },
    );

    let mut server = Server::new();
    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(200)
        .with_header("etag", "\"new\"")
        .with_body(serde_json::to_string(&make_releases(15)).unwrap())
        .create();

    let url = format!("{}/releases", server.url());
    // pages=0 < max_pages=1, so ETag path is skipped, full fetch happens
    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 1).unwrap();
    assert_eq!(result.len(), 15);
}

// --- Cache overwrite behavior ---

#[test]
fn fresh_fetch_overwrites_stale_cache() {
    let tmp = TempDir::new().unwrap();
    write_cache(
        tmp.path(),
        &ReleaseCache {
            etag: None,
            pages: 1,
            releases: make_releases(50),
        },
    );

    let mut server = Server::new();
    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(200)
        .with_header("etag", "\"new\"")
        .with_body(serde_json::to_string(&make_releases(75)).unwrap())
        .create();

    let url = format!("{}/releases", server.url());
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
    write_cache(
        tmp.path(),
        &ReleaseCache {
            etag: None,
            pages: 1,
            releases: make_releases(100),
        },
    );

    let mut server = Server::new();
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

    let url = format!("{}/releases", server.url());
    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 2).unwrap();
    assert_eq!(result.len(), 150);

    let saved = read_cache(tmp.path()).unwrap();
    assert_eq!(saved.pages, 2);
    assert_eq!(saved.releases.len(), 150);
}

// --- ETag validation returns non-2xx, non-304 (e.g. 403) ---

#[test]
fn etag_check_returns_403_falls_back_to_cache() {
    let tmp = TempDir::new().unwrap();
    write_cache(
        tmp.path(),
        &ReleaseCache {
            etag: Some("\"e\"".into()),
            pages: 1,
            releases: make_releases(80),
        },
    );

    let mut server = Server::new();
    server
        .mock("GET", "/releases")
        .match_query(page_matcher(1))
        .with_status(403)
        .create();

    let url = format!("{}/releases", server.url());
    let result = fetch_releases_impl(&test_client(), &url, tmp.path(), 1).unwrap();
    // The wildcard arm in the ETag match catches non-2xx non-304 and falls back
    assert_eq!(result.len(), 80);
}

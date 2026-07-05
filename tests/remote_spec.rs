use serde_json::json;
use svm::{parse_remote_spec, parse_version_triple, resolve_remote_spec, tag_semver, RemoteSpec};

fn releases_from(tags: &[&str]) -> Vec<serde_json::Value> {
    tags.iter().map(|t| json!({"tag_name": t})).collect()
}

// --- parse_version_triple ---

#[test]
fn triple_parses_with_and_without_v() {
    assert_eq!(parse_version_triple("v1.63.4"), Some((1, 63, 4)));
    assert_eq!(parse_version_triple("1.63.4"), Some((1, 63, 4)));
}

#[test]
fn triple_rejects_partial_and_garbage() {
    assert_eq!(parse_version_triple("v1.63"), None);
    assert_eq!(parse_version_triple("1"), None);
    assert_eq!(parse_version_triple("1.2.3.4"), None);
    assert_eq!(parse_version_triple("abc"), None);
    assert_eq!(parse_version_triple("v1.x.3"), None);
    assert_eq!(parse_version_triple(""), None);
}

#[test]
fn tag_semver_extracts_from_full_tags() {
    assert_eq!(tag_semver("mainnet-v1.63.4"), Some((1, 63, 4)));
    assert_eq!(tag_semver("testnet-v2.0.1"), Some((2, 0, 1)));
    assert_eq!(tag_semver("v1.2.3"), Some((1, 2, 3)));
    assert_eq!(tag_semver("custom-build"), None);
}

// --- parse_remote_spec ---

#[test]
fn spec_latest_means_newest_mainnet() {
    assert_eq!(
        parse_remote_spec("latest"),
        RemoteSpec::LatestForNetwork("mainnet".into())
    );
}

#[test]
fn spec_network_names_mean_newest_on_network() {
    for net in ["mainnet", "testnet", "devnet"] {
        assert_eq!(
            parse_remote_spec(net),
            RemoteSpec::LatestForNetwork(net.into())
        );
    }
}

#[test]
fn spec_network_latest_combined() {
    assert_eq!(
        parse_remote_spec("testnet-latest"),
        RemoteSpec::LatestForNetwork("testnet".into())
    );
}

#[test]
fn spec_partial_version_defaults_to_mainnet() {
    assert_eq!(
        parse_remote_spec("v1.63"),
        RemoteSpec::Partial { network: "mainnet".into(), major: 1, minor: 63 }
    );
    assert_eq!(
        parse_remote_spec("1.63"),
        RemoteSpec::Partial { network: "mainnet".into(), major: 1, minor: 63 }
    );
}

#[test]
fn spec_partial_version_with_network_prefix() {
    assert_eq!(
        parse_remote_spec("testnet-v1.63"),
        RemoteSpec::Partial { network: "testnet".into(), major: 1, minor: 63 }
    );
    assert_eq!(
        parse_remote_spec("devnet-1.2"),
        RemoteSpec::Partial { network: "devnet".into(), major: 1, minor: 2 }
    );
}

#[test]
fn spec_full_version_is_exact_mainnet_tag() {
    assert_eq!(
        parse_remote_spec("v1.63.4"),
        RemoteSpec::Exact("mainnet-v1.63.4".into())
    );
    assert_eq!(
        parse_remote_spec("1.63.4"),
        RemoteSpec::Exact("mainnet-v1.63.4".into())
    );
}

#[test]
fn spec_full_tag_is_exact_unchanged() {
    assert_eq!(
        parse_remote_spec("testnet-v1.63.4"),
        RemoteSpec::Exact("testnet-v1.63.4".into())
    );
}

#[test]
fn spec_network_prefixed_bare_triple_gets_v_inserted() {
    // "1.63.4" works bare and "testnet-1.63" works — "testnet-1.63.4" must too
    assert_eq!(
        parse_remote_spec("testnet-1.63.4"),
        RemoteSpec::Exact("testnet-v1.63.4".into())
    );
    assert_eq!(
        parse_remote_spec("mainnet-1.2.3"),
        RemoteSpec::Exact("mainnet-v1.2.3".into())
    );
}

#[test]
fn spec_custom_names_stay_exact() {
    assert_eq!(
        parse_remote_spec("custom-build"),
        RemoteSpec::Exact("custom-build".into())
    );
    assert_eq!(parse_remote_spec("vnet-foo"), RemoteSpec::Exact("vnet-foo".into()));
    assert_eq!(parse_remote_spec("my-dev"), RemoteSpec::Exact("my-dev".into()));
}

// --- resolve_remote_spec ---

#[test]
fn resolve_latest_picks_highest_semver_not_first() {
    // Release lists are usually newest-first, but don't rely on ordering
    let releases = releases_from(&[
        "testnet-v1.74.1",
        "mainnet-v1.74.0",
        "mainnet-v1.74.1",
        "devnet-v1.75.0",
        "mainnet-v1.73.9",
    ]);
    assert_eq!(resolve_remote_spec("latest", &releases).unwrap(), "mainnet-v1.74.1");
}

#[test]
fn resolve_latest_for_specific_network() {
    let releases = releases_from(&[
        "mainnet-v1.74.1",
        "testnet-v1.74.1",
        "testnet-v1.74.0",
        "devnet-v1.75.0",
    ]);
    assert_eq!(resolve_remote_spec("testnet", &releases).unwrap(), "testnet-v1.74.1");
    assert_eq!(resolve_remote_spec("devnet", &releases).unwrap(), "devnet-v1.75.0");
}

#[test]
fn resolve_partial_picks_highest_patch() {
    let releases = releases_from(&[
        "mainnet-v1.74.1",
        "mainnet-v1.63.2",
        "mainnet-v1.63.11",
        "mainnet-v1.63.4",
        "testnet-v1.63.99",
    ]);
    // 1.63.11 > 1.63.4 numerically (would fail with lexicographic compare)
    assert_eq!(resolve_remote_spec("v1.63", &releases).unwrap(), "mainnet-v1.63.11");
}

#[test]
fn resolve_partial_respects_network() {
    let releases = releases_from(&[
        "mainnet-v1.63.4",
        "testnet-v1.63.7",
    ]);
    assert_eq!(
        resolve_remote_spec("testnet-v1.63", &releases).unwrap(),
        "testnet-v1.63.7"
    );
}

#[test]
fn resolve_partial_no_match_errors() {
    let releases = releases_from(&["mainnet-v1.74.1"]);
    assert!(resolve_remote_spec("v9.99", &releases).is_err());
}

#[test]
fn resolve_latest_empty_releases_errors() {
    assert!(resolve_remote_spec("latest", &[]).is_err());
}

#[test]
fn resolve_exact_needs_no_releases() {
    // Exact specs resolve even with an empty release list
    assert_eq!(resolve_remote_spec("v1.63.4", &[]).unwrap(), "mainnet-v1.63.4");
    assert_eq!(
        resolve_remote_spec("testnet-v1.0.0", &[]).unwrap(),
        "testnet-v1.0.0"
    );
    assert_eq!(resolve_remote_spec("my-dev", &[]).unwrap(), "my-dev");
}

#[test]
fn resolve_ignores_releases_with_unparsable_tags() {
    let releases = releases_from(&["mainnet-v1.63.4", "mainnet-preview", "weird-tag"]);
    assert_eq!(resolve_remote_spec("latest", &releases).unwrap(), "mainnet-v1.63.4");
}

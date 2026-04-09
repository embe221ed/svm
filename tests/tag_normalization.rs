use svm::normalize_install_tag;

#[test]
fn bare_version_gets_mainnet_prefix() {
    assert_eq!(normalize_install_tag("v1.63.4"), "mainnet-v1.63.4");
}

#[test]
fn mainnet_tag_unchanged() {
    assert_eq!(
        normalize_install_tag("mainnet-v1.63.4"),
        "mainnet-v1.63.4"
    );
}

#[test]
fn testnet_tag_unchanged() {
    assert_eq!(
        normalize_install_tag("testnet-v1.63.4"),
        "testnet-v1.63.4"
    );
}

#[test]
fn devnet_tag_unchanged() {
    assert_eq!(normalize_install_tag("devnet-v1.0.0"), "devnet-v1.0.0");
}

#[test]
fn custom_name_no_v_prefix_unchanged() {
    assert_eq!(normalize_install_tag("custom-build"), "custom-build");
}

#[test]
fn bare_v_gets_mainnet_prefix() {
    // Edge case: just "v" with no number
    assert_eq!(normalize_install_tag("v"), "mainnet-v");
}

#[test]
fn empty_string_unchanged() {
    assert_eq!(normalize_install_tag(""), "");
}

#[test]
fn version_with_net_in_name_but_no_standard_prefix() {
    // "vnet-foo" starts with 'v' and contains 'net', so stays unchanged
    assert_eq!(normalize_install_tag("vnet-foo"), "vnet-foo");
}

#[test]
fn number_only_no_v_unchanged() {
    assert_eq!(normalize_install_tag("1.63.4"), "1.63.4");
}

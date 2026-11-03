//! A data folder of its own (FLE_HOME) keeps its own config.toml (3 Nov 2026: a test home shared ~/.config/fle).
//! One test per process-wide variable, so no other test reads FLE_HOME at the same time.

#[test]
fn fle_home_keeps_its_own_config_unless_one_is_named() {
    let dir = std::env::temp_dir().join("flelink-config-home-test");
    std::env::remove_var("FLE_CONFIG");
    std::env::set_var("FLE_HOME", &dir);
    assert_eq!(
        flelink_core::config::config_path(None),
        dir.join("config.toml")
    );
    // a file named on the command line or in FLE_CONFIG still wins
    assert_eq!(
        flelink_core::config::config_path(Some("/tmp/eigene.toml")),
        std::path::PathBuf::from("/tmp/eigene.toml")
    );
    std::env::set_var("FLE_CONFIG", "/tmp/aus-der-umgebung.toml");
    assert_eq!(
        flelink_core::config::config_path(None),
        std::path::PathBuf::from("/tmp/aus-der-umgebung.toml")
    );
    std::env::remove_var("FLE_CONFIG");
    std::env::remove_var("FLE_HOME");
}

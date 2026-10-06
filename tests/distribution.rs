//! Distribution data integrity belongs at the composition boundary.
use codex_panel::{
    config::{ConfigPaths, load_with_registry},
    dest::builtins,
};
use std::path::PathBuf;

#[test]
fn shipped_catalog_routes_match_the_distribution_metadata() {
    let paths = ConfigPaths {
        defaults: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("destinations.toml"),
        user: None,
    };
    let loaded = load_with_registry(&paths, &builtins::registry().unwrap()).unwrap();
    let metadata: serde_json::Value =
        serde_json::from_str(include_str!("../apikey-names.json")).unwrap();
    for (url, instance) in metadata["mappings"].as_object().unwrap() {
        let routed = loaded.mappings.destination_id(url).unwrap();
        assert_eq!(routed, instance.as_str().unwrap(), "{url}");
        assert_eq!(loaded.instance(routed).kind, "models_dev");
    }
}

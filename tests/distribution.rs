//! Distribution data integrity belongs at the composition boundary.
use codex_panel::{
    config::{ConfigPaths, load_with_registry},
    dest::builtins,
};
use std::path::PathBuf;

#[test]
fn shipped_defaults_load_all_enabled_instances_and_route_their_api_urls() {
    let paths = ConfigPaths {
        defaults: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("destinations.toml"),
        user: None,
    };
    let loaded = load_with_registry(&paths, &builtins::registry().unwrap()).unwrap();
    let document: toml::Value = include_str!("../destinations.toml").parse().unwrap();
    let enabled: Vec<_> = document["destinations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|instance| instance.get("enabled").and_then(toml::Value::as_bool) != Some(false))
        .collect();
    assert_eq!(loaded.instances.len(), enabled.len());
    for instance in enabled {
        let id = instance["id"].as_str().unwrap();
        assert_eq!(loaded.instance(id).kind, instance["type"].as_str().unwrap());
        for url in instance["api_urls"].as_array().unwrap() {
            let url = url.as_str().unwrap();
            assert_eq!(loaded.mappings.destination_id(url).unwrap(), id, "{url}");
        }
    }
}

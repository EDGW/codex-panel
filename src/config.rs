//! Deterministic discovery, partial validation and two-layer instance merging.
use crate::conversion::{PaymentConversion, config as conversion_config};
use crate::dest::{
    Result,
    registry::{DestinationFactory, DestinationMappings, Registry, normalize_url, validate_id},
};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct ConfigPaths {
    pub defaults: PathBuf,
    pub user: Option<PathBuf>,
}

pub struct Instance {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub factory: Arc<dyn DestinationFactory>,
    pub conversion: Option<Arc<dyn PaymentConversion>>,
}

pub struct LoadedConfig {
    pub instances: Vec<Instance>,
    pub mappings: DestinationMappings,
}

impl LoadedConfig {
    pub fn instance(&self, id: &str) -> &Instance {
        self.instances
            .iter()
            .find(|instance| instance.id == id)
            .expect("route references a validated instance")
    }
}

pub fn discover() -> Result<ConfigPaths> {
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let user_override = std::env::var_os("CC_PANEL_CONFIG").map(PathBuf::from);
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| user_override.as_ref().map(|_| PathBuf::new()))
        .ok_or("cannot locate user home for ~/.codex-panel/destinations.toml")?;
    discover_at(
        std::env::var_os("CC_PANEL_DEFAULTS_CONFIG").map(PathBuf::from),
        user_override,
        &executable,
        &home,
        cfg!(debug_assertions),
    )
}

pub fn discover_at(
    defaults_override: Option<PathBuf>,
    user_override: Option<PathBuf>,
    executable: &Path,
    home: &Path,
    development: bool,
) -> Result<ConfigPaths> {
    let defaults = match defaults_override {
        Some(path) if path.as_os_str().is_empty() => {
            return Err("CC_PANEL_DEFAULTS_CONFIG must not be empty".into());
        }
        Some(path) => path,
        None if development => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("destinations.toml"),
        None => {
            let executable = std::fs::canonicalize(executable).map_err(|e| {
                format!(
                    "{}: cannot resolve executable path: {e}",
                    executable.display()
                )
            })?;
            let adjacent = executable
                .parent()
                .unwrap_or(Path::new(""))
                .join("destinations.toml");
            let mut candidates = vec![adjacent];
            if cfg!(target_os = "linux") {
                let data_home = std::env::var_os("XDG_DATA_HOME")
                    .filter(|value| !value.is_empty())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join(".local/share"));
                candidates.extend([
                    data_home.join("codex-panel/destinations.toml"),
                    PathBuf::from("/usr/local/share/codex-panel/destinations.toml"),
                    PathBuf::from("/usr/share/codex-panel/destinations.toml"),
                ]);
            }
            select_defaults(&candidates)?
        }
    };
    let defaults = std::fs::canonicalize(&defaults).map_err(|e| {
        format!(
            "{}: distribution defaults missing or unreadable; set CC_PANEL_DEFAULTS_CONFIG: {e}",
            defaults.display()
        )
    })?;
    let explicit = user_override.is_some();
    let user = user_override.unwrap_or_else(|| home.join(".codex-panel/destinations.toml"));
    let user =
        if explicit
            || user.try_exists().map_err(|e| {
                format!("{}: cannot inspect user configuration: {e}", user.display())
            })?
        {
            Some(std::fs::canonicalize(&user).map_err(|e| {
                format!("{}: cannot locate user configuration: {e}", user.display())
            })?)
        } else {
            None
        };
    Ok(ConfigPaths { defaults, user })
}

fn select_defaults(candidates: &[PathBuf]) -> Result<PathBuf> {
    for path in candidates {
        // Inspect the entry itself so a broken symlink fails instead of falling through.
        match std::fs::symlink_metadata(path) {
            Ok(_) => {
                std::fs::File::open(path).map_err(|e| {
                    format!("{}: cannot read default configuration: {e}", path.display())
                })?;
                return Ok(path.clone());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(format!(
                    "{}: cannot inspect default configuration: {e}",
                    path.display()
                ));
            }
        }
    }
    Err(format!(
        "distribution defaults not found; set CC_PANEL_DEFAULTS_CONFIG; searched: {}",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PartialInstance {
    id: String,
    #[serde(rename = "type")]
    kind: Option<String>,
    name: Option<String>,
    api_urls: Option<Vec<String>>,
    enabled: Option<bool>,
    config: Option<toml::Value>,
    conversion: Option<toml::Value>,
}

struct Entry {
    value: toml::Value,
    // Field provenance remains separate from user configuration.
    origins: HashMap<String, String>,
}

fn context(path: &Path, id: &str, error: impl std::fmt::Display) -> String {
    format!("{}: instance '{id}': {error}", path.display())
}

fn generic(value: &toml::Value) -> Result<PartialInstance> {
    value
        .clone()
        .try_into()
        .map_err(|e| format!("destinations: {e}"))
}

fn validate_partial(
    value: &toml::Value,
    registry: &Registry,
    inherited_type: Option<&str>,
) -> Result<PartialInstance> {
    let instance = generic(value)?;
    validate_id(&instance.id).map_err(|e| format!("id: {e}"))?;
    if instance
        .name
        .as_ref()
        .is_some_and(|name| name.trim().is_empty())
    {
        return Err("name: must not be empty".into());
    }
    if let Some(urls) = &instance.api_urls {
        for url in urls {
            normalize_url(url).map_err(|e| format!("api_urls: {e}"))?;
        }
    }
    if let Some(config) = &instance.config
        && !config.is_table()
    {
        return Err("config: must be a table".into());
    }
    if let Some(kind) = instance.kind.as_deref().or(inherited_type) {
        registry.parse(
            kind,
            instance.config.as_ref().unwrap_or(&empty_table()),
            false,
        )?;
    }
    if let Some(conversion) = &instance.conversion {
        conversion_config::parse(conversion, false)?;
    }
    Ok(instance)
}

fn empty_table() -> toml::Value {
    toml::Value::Table(Default::default())
}

fn read_document(path: &Path) -> Result<Vec<toml::Value>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("{}: cannot read configuration: {e}", path.display()))?;
    let value: toml::Value = text
        .parse()
        .map_err(|e| format!("{}: invalid TOML: {e}", path.display()))?;
    let root = value
        .as_table()
        .ok_or_else(|| format!("{}: configuration must be a table", path.display()))?;
    for field in root.keys() {
        if !matches!(field.as_str(), "version" | "destinations") {
            return Err(format!("{}: {field}: unknown field", path.display()));
        }
    }
    if root.get("version").and_then(toml::Value::as_integer) != Some(1) {
        return Err(format!("{}: version: expected 1", path.display()));
    }
    let entries = match root.get("destinations") {
        None => Vec::new(),
        Some(value) => value
            .as_array()
            .ok_or_else(|| format!("{}: destinations: expected array of tables", path.display()))?
            .clone(),
    };
    let mut ids = HashSet::new();
    for value in &entries {
        let id = value
            .get("id")
            .and_then(toml::Value::as_str)
            .unwrap_or("<missing>");
        generic(value).map_err(|e| context(path, id, e))?;
        if !ids.insert(id) {
            return Err(context(path, id, "id: duplicate instance id"));
        }
    }
    Ok(entries)
}

fn origins(value: &toml::Value, prefix: &str, path: &Path, output: &mut HashMap<String, String>) {
    if !prefix.is_empty() {
        output.insert(prefix.into(), path.display().to_string());
    }
    if let Some(table) = value.as_table() {
        for (key, value) in table {
            let field = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            origins(value, &field, path, output);
        }
    }
}

fn merge(
    base: &mut toml::Value,
    patch: &toml::Value,
    field: &str,
    path: &Path,
    provenance: &mut HashMap<String, String>,
) {
    if let (Some(base_table), Some(patch_table)) = (base.as_table_mut(), patch.as_table()) {
        let changed_type = patch_table
            .get("type")
            .is_some_and(|kind| base_table.get("type") != Some(kind));
        if changed_type && field.is_empty() {
            base_table.remove("config");
            provenance.retain(|key, _| key != "config" && !key.starts_with("config."));
            // Missing fields in the new config belong to the layer that changed the type.
            provenance.insert("config".into(), path.display().to_string());
        }
        if changed_type && field == "conversion.source" {
            *base_table = patch_table.clone();
            provenance.retain(|key, _| key != field && !key.starts_with(&format!("{field}.")));
            origins(patch, field, path, provenance);
            return;
        }
        if !field.is_empty() {
            provenance.insert(field.into(), path.display().to_string());
        }
        for (key, value) in patch_table {
            let child = if field.is_empty() {
                key.clone()
            } else {
                format!("{field}.{key}")
            };
            if let Some(previous) = base_table.get_mut(key) {
                merge(previous, value, &child, path, provenance);
            } else {
                base_table.insert(key.clone(), value.clone());
                origins(value, &child, path, provenance);
            }
        }
    } else {
        *base = patch.clone();
        provenance.retain(|key, _| key != field && !key.starts_with(&format!("{field}.")));
        origins(patch, field, path, provenance);
    }
}

pub fn load_with_registry(paths: &ConfigPaths, registry: &Registry) -> Result<LoadedConfig> {
    let mut entries: Vec<Entry> = Vec::new();
    for path in std::iter::once(&paths.defaults).chain(paths.user.iter()) {
        for value in read_document(path)? {
            let id = value
                .get("id")
                .and_then(toml::Value::as_str)
                .expect("validated id");
            let existing = entries
                .iter()
                .position(|entry| entry.value.get("id").and_then(toml::Value::as_str) == Some(id));
            let inherited_type = existing.and_then(|index| {
                entries[index]
                    .value
                    .get("type")
                    .and_then(toml::Value::as_str)
            });
            validate_partial(&value, registry, inherited_type).map_err(|e| context(path, id, e))?;
            if let Some(index) = existing {
                let entry = &mut entries[index];
                merge(&mut entry.value, &value, "", path, &mut entry.origins);
            } else {
                let mut fields = HashMap::new();
                origins(&value, "", path, &mut fields);
                entries.push(Entry {
                    value,
                    origins: fields,
                });
            }
        }
    }
    let mut instances = Vec::new();
    let mut mappings = DestinationMappings::default();
    let mut route_origins = HashMap::<String, (String, String)>::new();
    for entry in entries {
        let id = entry
            .value
            .get("id")
            .and_then(toml::Value::as_str)
            .expect("validated id");
        let validated = || -> Result<Option<Instance>> {
            let partial = validate_partial(&entry.value, registry, None)?;
            let kind = partial.kind.ok_or("type: missing required field")?;
            let name = partial.name.ok_or("name: missing required field")?;
            let urls = partial.api_urls.ok_or("api_urls: missing required field")?;
            let config = partial.config.unwrap_or_else(empty_table);
            let enabled = partial.enabled.unwrap_or(true);
            let factory = registry.parse(&kind, &config, enabled)?;
            let conversion = partial
                .conversion
                .as_ref()
                .map(|value| conversion_config::parse(value, enabled))
                .transpose()?
                .flatten();
            if !enabled {
                return Ok(None);
            }
            if urls.is_empty() {
                return Err("api_urls: must not be empty for an enabled instance".into());
            }
            Ok(Some(Instance {
                id: partial.id,
                kind,
                name,
                factory: factory.ok_or("config: incomplete type configuration")?,
                conversion,
            }))
        };
        let error_context = |error: String| {
            let field = error.split(':').next().unwrap_or("id");
            let origin = entry
                .origins
                .get(field)
                .or_else(|| entry.origins.get(field.split('.').next().unwrap_or("id")))
                .or_else(|| entry.origins.get("id"))
                .expect("entry provenance");
            format!("{origin}: instance '{id}': {error}")
        };
        if let Some(instance) = validated().map_err(&error_context)? {
            for url in generic(&entry.value)?.api_urls.expect("validated URLs") {
                let normalized = normalize_url(&url)?;
                if let Some((previous_id, previous_path)) = route_origins.get(&normalized) {
                    return Err(error_context(format!(
                        "api_urls: duplicate API URL; conflicts with {previous_path}: instance '{previous_id}': api_urls"
                    )));
                }
                mappings
                    .insert(&url, &instance.id)
                    .map_err(&error_context)?;
                route_origins.insert(
                    normalized,
                    (instance.id.clone(), entry.origins["api_urls"].clone()),
                );
            }
            instances.push(instance);
        }
    }
    Ok(LoadedConfig {
        instances,
        mappings,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn shipped_catalog_routes_are_valid_and_only_deepseek_has_a_note() {
        let paths = ConfigPaths {
            defaults: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("destinations.toml"),
            user: None,
        };
        let registry = crate::dest::builtins::registry().unwrap();
        let loaded = load_with_registry(&paths, &registry).unwrap();
        let metadata: serde_json::Value =
            serde_json::from_str(include_str!("../apikey-names.json")).unwrap();
        for (url, instance) in metadata["mappings"].as_object().unwrap() {
            let routed = loaded.mappings.destination_id(url).unwrap();
            assert_eq!(routed, instance.as_str().unwrap());
            assert_eq!(loaded.instance(routed).kind, "models_dev");
        }
        assert_eq!(
            loaded
                .mappings
                .destination_id("https://cc2.caaa.tech/v1")
                .unwrap(),
            "cc2"
        );
        assert_eq!(
            loaded.instance("deepseek").factory.warnings(),
            ["Only off-peak prices are shown; estimated costs may be lower than actual charges."]
        );
        assert!(loaded.instance("openai").factory.warnings().is_empty());
    }

    #[test]
    fn fallback_uses_first_existing_file_and_reports_missing_paths() {
        let root =
            std::env::temp_dir().join(format!("codex-panel-fallback-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let candidates: Vec<_> = ["adjacent", "user", "local", "system"]
            .iter()
            .map(|name| root.join(name))
            .collect();
        let error = select_defaults(&candidates).unwrap_err();
        for path in &candidates {
            assert!(error.contains(&path.display().to_string()));
        }
        for path in candidates.iter().rev() {
            std::fs::write(path, "version = 1\n").unwrap();
            assert_eq!(select_defaults(&candidates).unwrap(), *path);
        }
        std::fs::write(&candidates[0], "invalid TOML").unwrap();
        let paths = ConfigPaths {
            defaults: select_defaults(&candidates).unwrap(),
            user: None,
        };
        assert!(load_with_registry(&paths, &crate::dest::builtins::registry().unwrap()).is_err());
        std::fs::remove_file(&candidates[0]).unwrap();
        symlink(root.join("missing"), &candidates[0]).unwrap();
        assert!(select_defaults(&candidates).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn release_defaults_follow_executable_symlinks_and_respect_overrides() {
        struct TestDir(PathBuf);
        impl Drop for TestDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir = TestDir(
            std::env::temp_dir().join(format!("codex-panel-discovery-{}", std::process::id())),
        );
        let distribution = dir.0.join("distribution");
        let bin = dir.0.join("bin");
        std::fs::create_dir_all(&distribution).unwrap();
        std::fs::create_dir(&bin).unwrap();
        let executable = distribution.join("codex-panel");
        std::fs::write(&executable, "").unwrap();
        let defaults = distribution.join("destinations.toml");
        std::fs::write(&defaults, "version = 1\n").unwrap();
        // A configuration beside the symlink must not hide the distribution file.
        std::fs::write(bin.join("destinations.toml"), "invalid TOML").unwrap();
        symlink("../distribution/codex-panel", bin.join("panel-link")).unwrap();
        symlink("panel-link", bin.join("codex-panel")).unwrap();

        for path in [&executable, &bin.join("codex-panel")] {
            let paths = discover_at(None, None, path, &dir.0, false).unwrap();
            assert_eq!(paths.defaults, std::fs::canonicalize(&defaults).unwrap());
            assert!(paths.user.is_none());
            assert!(
                load_with_registry(&paths, &Registry::new())
                    .unwrap()
                    .instances
                    .is_empty()
            );
        }
        let override_path = dir.0.join("override.toml");
        std::fs::write(&override_path, "version = 1\n").unwrap();
        let paths = discover_at(
            Some(override_path.clone()),
            None,
            &bin.join("codex-panel"),
            &dir.0,
            false,
        )
        .unwrap();
        assert_eq!(
            paths.defaults,
            std::fs::canonicalize(override_path).unwrap()
        );
        for invalid in [PathBuf::new(), dir.0.join("missing.toml")] {
            assert!(discover_at(Some(invalid), None, &executable, &dir.0, false).is_err());
        }
    }
}

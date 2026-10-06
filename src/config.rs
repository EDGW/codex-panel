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
    use crate::dest::{
        Credentials, Destination, DestinationConfig, PricingInterface, SessionContext,
        SessionPricing, Stats, registry::CreateContext,
    };
    use std::os::unix::fs::symlink;

    struct Fixture {
        root: PathBuf,
        paths: ConfigPaths,
        registry: Registry,
    }

    impl Fixture {
        fn new(label: &str, defaults: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("panel-config-{label}-{}", std::process::id()));
            std::fs::create_dir_all(&root).unwrap();
            let paths = ConfigPaths {
                defaults: root.join("defaults.toml"),
                user: Some(root.join("user.toml")),
            };
            std::fs::write(&paths.defaults, defaults).unwrap();
            let mut registry = Registry::new();
            for (kind, field) in [("fixture-a", "endpoint"), ("fixture-b", "selector")] {
                registry
                    .register(kind, move |value, complete| {
                        let table = value.as_table().ok_or("config: expected table")?;
                        for key in table.keys() {
                            if key != field {
                                return Err(format!("config.{key}: unknown field"));
                            }
                        }
                        match table.get(field) {
                            Some(value) => {
                                let text = value
                                    .as_str()
                                    .filter(|value| !value.is_empty())
                                    .ok_or_else(|| {
                                        format!("config.{field}: expected nonempty string")
                                    })?;
                                Ok(complete.then(|| {
                                    Arc::new(FixtureFactory(text.into()))
                                        as Arc<dyn DestinationFactory>
                                }))
                            }
                            None if complete => {
                                Err(format!("config.{field}: missing required field"))
                            }
                            None => Ok(None),
                        }
                    })
                    .unwrap();
            }
            Self {
                root,
                paths,
                registry,
            }
        }

        fn load(&self, user: &str) -> Result<LoadedConfig> {
            std::fs::write(self.paths.user.as_ref().unwrap(), user).unwrap();
            load_with_registry(&self.paths, &self.registry)
        }

        fn error(&self, user: &str, origin: &Path, instance: &str, field: &str) -> String {
            let error = self
                .load(user)
                .err()
                .expect("invalid configuration must fail before startup");
            assert!(
                error.contains(&format!(
                    "{}: instance '{instance}': {field}:",
                    origin.display()
                )),
                "{error}"
            );
            error
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    struct FixtureFactory(String);
    impl DestinationFactory for FixtureFactory {
        fn create(&self, context: CreateContext) -> Result<Arc<dyn Destination>> {
            Ok(Arc::new(FixtureDestination(format!(
                "{} @ {}",
                context.name, self.0
            ))))
        }
    }
    struct FixtureDestination(String);
    impl Destination for FixtureDestination {
        fn config(&self) -> DestinationConfig {
            DestinationConfig {
                name: self.0.clone(),
                billing_currency: Some("USD".into()),
            }
        }
        fn pricing(&self) -> PricingInterface<'_> {
            PricingInterface::SessionTotals(self)
        }
    }
    impl SessionPricing for FixtureDestination {
        fn session_totals(&self, _: &SessionContext) -> Result<(Stats, bool)> {
            unreachable!("configuration tests do not retrieve bills")
        }
    }
    struct NoCredentials;
    impl Credentials for NoCredentials {
        fn api_key(&self, _: Option<&str>) -> Result<String> {
            panic!("configuration must not read credentials")
        }
    }

    const DEFAULTS: &str = r#"
version = 1
[[destinations]]
id = "alpha"
type = "fixture-a"
name = "Original"
api_urls = ["https://alpha.example/v1"]
[destinations.config]
endpoint = "original"
[destinations.conversion]
currency = "EUR"
multiplier = 2.0
[destinations.conversion.source]
type = "value"
value = 3.0
[[destinations]]
id = "beta"
type = "fixture-a"
name = "Independent"
api_urls = ["https://beta.example/v1"]
[destinations.config]
endpoint = "independent"
"#;

    fn created_name(instance: &Instance) -> String {
        instance
            .factory
            .create(CreateContext {
                name: instance.name.clone(),
                credentials: Arc::new(NoCredentials),
            })
            .unwrap()
            .config()
            .name
    }

    #[test]
    fn overrides_preserve_inherited_fields_and_replace_routes_and_destination_types() {
        let fixture = Fixture::new("merge", DEFAULTS);
        let loaded = fixture
            .load(
                r#"
version = 1
[[destinations]]
id = "alpha"
name = "Renamed"
api_urls = ["https://replacement.example/v1/"]
[destinations.conversion]
multiplier = 4.0
"#,
            )
            .unwrap();
        assert_eq!(created_name(loaded.instance("alpha")), "Renamed @ original");
        assert_eq!(
            loaded
                .mappings
                .destination_id("https://replacement.example/v1")
                .unwrap(),
            "alpha"
        );
        assert!(
            loaded
                .mappings
                .destination_id("https://alpha.example/v1")
                .is_err()
        );
        assert_eq!(
            created_name(loaded.instance("beta")),
            "Independent @ independent"
        );
        let payment = loaded
            .instance("alpha")
            .conversion
            .as_ref()
            .unwrap()
            .initial_payment()
            .unwrap()
            .unwrap();
        assert_eq!(payment.payment_currency, "EUR");
        assert_eq!(payment.exchange_rate, 12.0);
        let switched = fixture
            .load(
                r#"
version = 1
[[destinations]]
id = "alpha"
type = "fixture-b"
[destinations.config]
selector = "replacement"
"#,
            )
            .unwrap();
        // fixture-b rejects endpoint; successful loading proves stale type fields were removed.
        assert_eq!(
            created_name(switched.instance("alpha")),
            "Original @ replacement"
        );
        assert_eq!(switched.instance("alpha").kind, "fixture-b");
        assert!(switched.instance("alpha").conversion.is_some());
    }

    #[test]
    fn conversion_source_type_changes_discard_old_format_fields_and_can_be_disabled() {
        let defaults = DEFAULTS.replace(
            "type = \"value\"\nvalue = 3.0",
            "type = \"json\"\nurl = \"https://rates.example/data\"\npointer = \"/rate\"",
        );
        let fixture = Fixture::new("source-switch", &defaults);
        let loaded = fixture
            .load(
                r#"
version = 1
[[destinations]]
id = "alpha"
[destinations.conversion.source]
type = "value"
value = 5.0
"#,
            )
            .unwrap();
        let conversion = loaded.instance("alpha").conversion.as_ref().unwrap();
        assert_eq!(
            conversion.initial_payment().unwrap().unwrap().exchange_rate,
            10.0
        );
        assert_eq!(conversion.settings().unwrap().currency, "EUR");
        let disabled = fixture.load("version = 1\n[[destinations]]\nid = 'alpha'\n[destinations.conversion]\nenabled = false").unwrap();
        assert!(disabled.instance("alpha").conversion.is_none());
        let bad_override = "version = 1\n[[destinations]]\nid = 'alpha'\n[destinations.conversion]\nenabled = false\nmultiplier = -1";
        fixture.error(
            bad_override,
            fixture.paths.user.as_ref().unwrap(),
            "alpha",
            "conversion.multiplier",
        );
    }

    #[test]
    fn invalid_configuration_reports_instance_field_and_the_responsible_layer() {
        let fixture = Fixture::new("provenance", DEFAULTS);
        let user = fixture.paths.user.as_ref().unwrap();
        fixture.error(
            "version = 1\n[[destinations]]\nid = 'alpha'\n[destinations.config]\nendpoint = 42",
            user,
            "alpha",
            "config.endpoint",
        );
        fixture.error(
            "version = 1\n[[destinations]]\nid = 'alpha'\ntype = 'fixture-b'",
            user,
            "alpha",
            "config.selector",
        );
        fixture.error("version = 1\n[[destinations]]\nid = 'alpha'\n[destinations.conversion.source]\ntype = 'xml'\nurl = 'https://rates.example/data'", user, "alpha", "conversion.source.xpath");
        let error = fixture.error(
            "version = 1\n[[destinations]]\nid = 'alpha'\napi_urls = ['https://beta.example/v1/']",
            &fixture.paths.defaults,
            "beta",
            "api_urls",
        );
        assert!(
            error.contains(&format!("{}: instance 'alpha': api_urls", user.display())),
            "{error}"
        );
        // A valid partial override must not hide a missing required default field.
        std::fs::write(
            &fixture.paths.defaults,
            DEFAULTS.replace("endpoint = \"original\"", ""),
        )
        .unwrap();
        fixture.error(
            "version = 1\n[[destinations]]\nid = 'alpha'\nname = 'Renamed'",
            &fixture.paths.defaults,
            "alpha",
            "config.endpoint",
        );
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
        assert!(load_with_registry(&paths, &Registry::new()).is_err());
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

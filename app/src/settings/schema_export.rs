use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Context;
use galaxy_core::channel::{Channel, ChannelState};
use galaxy_core::features::{
    FeatureFlag, DEBUG_FLAGS, DOGFOOD_FLAGS, PREVIEW_FLAGS, RELEASE_FLAGS,
};
use schemars::SchemaGenerator;
use serde_json::{Map, Value};
use settings::schema::SettingSchemaEntry;

fn strip_numeric_metadata(value: &mut Value) {
    match value {
        Value::Object(map) => {
            let is_numeric = map
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|value_type| value_type == "integer" || value_type == "number");

            if is_numeric {
                map.remove("minimum");
                map.remove("maximum");
                map.remove("format");
            }

            for value in map.values_mut() {
                strip_numeric_metadata(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                strip_numeric_metadata(value);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn strip_empty_enum_entries(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Array(one_of)) = map.get_mut("oneOf") {
                one_of.retain(|entry| {
                    !matches!(entry, Value::Object(object)
                        if object.get("enum").is_some_and(|value| value.as_array().is_some_and(|values| values.is_empty())))
                });
            }

            for value in map.values_mut() {
                strip_empty_enum_entries(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                strip_empty_enum_entries(value);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn active_flags_for_channel(channel: &str) -> HashSet<FeatureFlag> {
    let mut flags = HashSet::new();

    let flag_lists: &[&[FeatureFlag]] = match channel {
        "stable" => &[RELEASE_FLAGS],
        "preview" => &[RELEASE_FLAGS, PREVIEW_FLAGS],
        "dev" => &[RELEASE_FLAGS, PREVIEW_FLAGS, DOGFOOD_FLAGS, DEBUG_FLAGS],
        other => {
            log::warn!("Unknown settings schema channel '{other}', defaulting to dev");
            &[RELEASE_FLAGS, PREVIEW_FLAGS, DOGFOOD_FLAGS, DEBUG_FLAGS]
        }
    };

    for list in flag_lists {
        flags.extend(*list);
    }

    flags
}

fn ensure_hierarchy<'a>(
    root_properties: &'a mut Map<String, Value>,
    hierarchy: &str,
) -> &'a mut Map<String, Value> {
    let mut current = root_properties;

    for segment in hierarchy.split('.') {
        let entry = current.entry(segment.to_string()).or_insert_with(|| {
            Value::Object({
                let mut map = Map::new();
                map.insert("type".to_string(), Value::String("object".to_string()));
                map.insert("properties".to_string(), Value::Object(Map::new()));
                map
            })
        });

        current = entry
            .as_object_mut()
            .expect("hierarchy node should be an object")
            .entry("properties")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .expect("properties should be an object");
    }

    current
}

/// Generates the user-facing JSON schema for Galaxy's TOML settings.
pub fn generate_settings_schema(channel: &str) -> (String, usize) {
    let active_flags = active_flags_for_channel(channel);
    let mut generator = SchemaGenerator::default();
    let mut root_properties = Map::new();
    let mut entry_count = 0;

    for entry in inventory::iter::<SettingSchemaEntry> {
        if entry.is_private {
            continue;
        }

        if let Some(flag) = entry.feature_flag {
            if !active_flags.contains(&flag) {
                continue;
            }
        }

        let type_schema = (entry.schema_fn)(&mut generator);
        let mut schema_value: Value = type_schema.to_value();
        let default_json = (entry.file_default_value_fn)();

        if let Ok(default_value) = serde_json::from_str::<Value>(&default_json) {
            if let Some(object) = schema_value.as_object_mut() {
                object.insert("default".to_string(), default_value);
            }
        }

        if !entry.description.is_empty() {
            if let Some(object) = schema_value.as_object_mut() {
                object.insert(
                    "description".to_string(),
                    Value::String(entry.description.to_string()),
                );
            }
        }

        let target = if let Some(hierarchy) = entry.hierarchy {
            ensure_hierarchy(&mut root_properties, hierarchy)
        } else {
            &mut root_properties
        };

        target.insert(entry.storage_key.to_string(), schema_value);
        entry_count += 1;
    }

    let definitions = generator.take_definitions(true);
    let mut root = Map::new();
    root.insert(
        "$schema".to_string(),
        Value::String("https://json-schema.org/draft/2020-12/schema".to_string()),
    );
    root.insert(
        "title".to_string(),
        Value::String("Galaxy Settings".to_string()),
    );
    root.insert(
        "description".to_string(),
        Value::String(format!(
            "JSON Schema for Galaxy settings ({channel} channel, {entry_count} settings)"
        )),
    );
    root.insert("type".to_string(), Value::String("object".to_string()));
    root.insert("properties".to_string(), Value::Object(root_properties));

    if !definitions.is_empty() {
        root.insert("$defs".to_string(), Value::Object(definitions));
    }

    let mut root_value = Value::Object(root);
    strip_numeric_metadata(&mut root_value);
    strip_empty_enum_entries(&mut root_value);

    (
        serde_json::to_string_pretty(&root_value).expect("settings schema should serialize"),
        entry_count,
    )
}

fn runtime_schema_channel() -> &'static str {
    match ChannelState::channel() {
        Channel::Stable => "stable",
        Channel::Preview => "preview",
        Channel::Dev | Channel::Local | Channel::Oss | Channel::Integration => "dev",
    }
}

/// Returns the bundled settings schema, or generates a current local copy for development runs.
pub fn ensure_runtime_settings_schema() -> anyhow::Result<PathBuf> {
    if let Some(schema_path) = galaxy_core::paths::bundled_resources_dir()
        .map(|resources| resources.join("settings_schema.json"))
        .filter(|path| path.is_file())
    {
        return Ok(schema_path);
    }

    let schema_path = galaxy_core::paths::config_local_dir().join("settings_schema.json");
    let (schema, _) = generate_settings_schema(runtime_schema_channel());
    let existing_schema = std::fs::read_to_string(&schema_path).ok();

    if existing_schema.as_deref() != Some(schema.as_str()) {
        if let Some(parent) = schema_path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create settings schema directory {}",
                    parent.display()
                )
            })?;
        }
        std::fs::write(&schema_path, schema).with_context(|| {
            format!(
                "Failed to write Galaxy settings schema to {}",
                schema_path.display()
            )
        })?;
    }

    Ok(schema_path)
}

//! Composition of built-in adapters. Generic configuration loading receives this registry.
use super::{Result, claude_code_hub::ClaudeCodeHub, models_dev::ModelsDev, registry::Registry};

pub fn registry() -> Result<Registry> {
    let mut registry = Registry::new();
    registry.register(ClaudeCodeHub::TYPE, ClaudeCodeHub::parse_config)?;
    registry.register(ModelsDev::TYPE, ModelsDev::parse_config)?;
    Ok(registry)
}

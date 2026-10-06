//! Format-specific expression validation, scalar extraction and success conditions.
use crate::dest::Result;
use serde_json::Value as Json;
use std::collections::BTreeMap;
use sxd_xpath::{Context, Factory, Value};

#[derive(Clone, Debug, PartialEq)]
pub enum Scalar {
    Number(f64),
    Text(String),
    Bool(bool),
    Null,
}

impl Scalar {
    pub fn from_json(value: &Json) -> Result<Self> {
        match value {
            Json::Number(n) => n
                .as_f64()
                .map(Self::Number)
                .ok_or_else(|| "number out of range".into()),
            Json::String(s) => Ok(Self::Text(s.clone())),
            Json::Bool(b) => Ok(Self::Bool(*b)),
            Json::Null => Ok(Self::Null),
            _ => Err("expression must select a scalar".into()),
        }
    }
}

pub fn validate_pointer(pointer: &str) -> Result<()> {
    if !pointer.is_empty() && !pointer.starts_with('/') {
        return Err("JSON Pointer must be empty or start with '/'".into());
    }
    let mut chars = pointer.chars();
    while let Some(c) = chars.next() {
        if c == '~' && !matches!(chars.next(), Some('0' | '1')) {
            return Err("JSON Pointer has an invalid '~' escape".into());
        }
    }
    Ok(())
}

pub fn validate_xpath(expression: &str) -> Result<()> {
    Factory::new()
        .build(expression)
        .map_err(|e| format!("invalid XPath: {e}"))?
        .ok_or("XPath must not be empty")?;
    Ok(())
}

#[derive(Clone, Debug)]
pub struct Condition {
    pub expression: String,
    pub equals: Scalar,
}

pub trait Extractor: Send + Sync {
    fn extract(&self, body: &str) -> Result<Scalar>;
}

pub struct JsonExtractor {
    pub pointer: String,
    pub expect: Option<Condition>,
}

impl Extractor for JsonExtractor {
    fn extract(&self, body: &str) -> Result<Scalar> {
        let document: Json =
            serde_json::from_str(body).map_err(|_| "invalid JSON source response")?;
        let select = |pointer: &str| {
            Scalar::from_json(
                document
                    .pointer(pointer)
                    .ok_or_else(|| format!("JSON Pointer '{pointer}' matched no value"))?,
            )
        };
        if let Some(expect) = &self.expect
            && select(&expect.expression)? != expect.equals
        {
            return Err("source success condition failed".into());
        }
        select(&self.pointer)
    }
}

pub struct XmlExtractor {
    pub xpath: String,
    pub namespaces: BTreeMap<String, String>,
    pub expect: Option<Condition>,
}

impl Extractor for XmlExtractor {
    fn extract(&self, body: &str) -> Result<Scalar> {
        let package =
            sxd_document::parser::parse(body).map_err(|_| "invalid XML source response")?;
        let document = package.as_document();
        let mut context = Context::new();
        for (prefix, uri) in &self.namespaces {
            context.set_namespace(prefix, uri);
        }
        let select = |expression: &str| -> Result<Scalar> {
            let xpath = Factory::new()
                .build(expression)
                .map_err(|e| format!("invalid XPath: {e}"))?
                .ok_or("XPath must not be empty")?;
            match xpath
                .evaluate(&context, document.root())
                .map_err(|e| format!("XPath evaluation failed: {e}"))?
            {
                Value::Number(n) => Ok(Scalar::Number(n)),
                Value::String(s) => Ok(Scalar::Text(s)),
                Value::Boolean(b) => Ok(Scalar::Bool(b)),
                Value::Nodeset(nodes) => {
                    if nodes.size() != 1 {
                        return Err(format!(
                            "XPath must match exactly one node, matched {}",
                            nodes.size()
                        ));
                    }
                    Ok(Scalar::Text(
                        nodes
                            .document_order_first()
                            .expect("one node")
                            .string_value(),
                    ))
                }
            }
        };
        if let Some(expect) = &self.expect
            && select(&expect.expression)? != expect.equals
        {
            return Err("source success condition failed".into());
        }
        select(&self.xpath)
    }
}

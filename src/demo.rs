//! Fixture backends for Lapui's built-in demonstrations, not filesystem tools.

use crate::action::{ActionError, ActionInfo, ActionKind, ActionRegistry};
use crate::action_catalog::ActionOptions;
use serde_json::{json, Value};
use std::time::Duration;

fn info(id: &str, description: &str, input_schema: Value, output_schema: Value) -> ActionInfo {
    ActionInfo {
        id: id.into(),
        description: description.into(),
        input_schema,
        output_schema,
        kind: ActionKind::Write,
    }
}

fn rename_target(state: &Value, args: &Value) -> Result<usize, ActionError> {
    let id = args["fileId"].as_str().unwrap();
    let name = args["name"].as_str().unwrap();
    if name.trim() != name
        || matches!(name, "." | "..")
        || name.contains(['/', '\\'])
        || name.chars().any(char::is_control)
    {
        return Err(ActionError::new("invalid_name", "name must be a single nonempty filename without surrounding whitespace or control characters"));
    }
    let files = state["files"].as_array().unwrap();
    let index = files
        .iter()
        .position(|file| file["id"] == id)
        .ok_or_else(|| ActionError::new("target_missing", "file does not exist"))?;
    let expected = args["expectedFileVersion"].as_u64().unwrap();
    let current = files[index]["version"].as_u64().unwrap();
    if current != expected {
        return Err(ActionError::new("stale_entity", format!("expected file version {expected}, current version is {current}; refresh before retry")));
    }
    if files
        .iter()
        .any(|file| file["id"] != id && file["name"] == name)
    {
        return Err(ActionError::new(
            "name_conflict",
            "another fixture file already has that name",
        ));
    }
    Ok(index)
}

pub fn files() -> Result<ActionRegistry, ActionError> {
    let registry = ActionRegistry::new(json!({"files":[
        {"id":"file-1","name":"设计说明.md","version":1,"size":2048},
        {"id":"file-2","name":"project.rs","version":1,"size":4096},
        {"id":"file-3","name":"photo.png","version":1,"size":8192}
    ]}))?;
    let file_schema = json!({"type":"object","required":["id","name","version","size"],"additionalProperties":false,"properties":{
        "id":{"type":"string"},"name":{"type":"string"},"version":{"type":"integer","minimum":1},"size":{"type":"integer","minimum":0}
    }});
    registry.register_query(info("files.query", "Search fixture files with bounded pagination; no state mutation",
        json!({"type":"object","additionalProperties":false,"properties":{"query":{"type":"string","maxLength":256},"offset":{"type":"integer","minimum":0,"maximum":1000},"limit":{"type":"integer","minimum":1,"maximum":50}}}),
        json!({"type":"object","required":["items","total","offset","hasMore"],"additionalProperties":false,"properties":{"items":{"type":"array","items":file_schema.clone()},"total":{"type":"integer","minimum":0},"offset":{"type":"integer","minimum":0},"hasMore":{"type":"boolean"}}})),
        |state, args| {
            let query = args.get("query").and_then(Value::as_str).unwrap_or("").to_lowercase();
            let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
            let matches: Vec<_> = state["files"].as_array().unwrap().iter().filter(|file| file["name"].as_str().unwrap().to_lowercase().contains(&query)).collect();
            let items: Vec<_> = matches.iter().skip(offset).take(limit).copied().cloned().collect();
            Ok(json!({"total":matches.len(),"offset":offset,"hasMore":offset.saturating_add(items.len()) < matches.len(),"items":items}))
        })?;
    registry.register_with_options(info("files.rename", "Rename an in-memory fixture file; requires its current entity version",
        json!({"type":"object","required":["fileId","name","expectedFileVersion"],"additionalProperties":false,"properties":{"fileId":{"type":"string","maxLength":128},"name":{"type":"string","minLength":1,"maxLength":128},"expectedFileVersion":{"type":"integer","minimum":1}}}), file_schema.clone()),
        ActionOptions::default().with_availability(|state, args| rename_target(state,args).map(|_| ())),
        |state, args| {
            let index = rename_target(state, args)?;
            let name = args["name"].as_str().unwrap();
            let files = state["files"].as_array_mut().unwrap();
            let current = files[index]["version"].as_u64().unwrap();
            let version = current.checked_add(1).ok_or_else(|| ActionError::new("version_overflow", "file version overflow"))?;
            files[index]["name"] = json!(name); files[index]["version"] = json!(version);
            Ok(files[index].clone())
        })?;
    registry.register_operation(info("files.scan", "Scan an immutable snapshot of the in-memory fixture catalog; reports progress and supports cooperative cancellation",
        json!({"type":"object","additionalProperties":false}),
        json!({"type":"object","required":["files","total"],"additionalProperties":false,"properties":{"files":{"type":"array","items":file_schema},"total":{"type":"integer","minimum":0}}})),
        |state, _, context| {
            let files = state["files"].as_array().unwrap();
            for (index, file) in files.iter().enumerate() { context.checkpoint()?; context.delay(Duration::from_millis(40))?; context.report((index + 1) as f64 / files.len() as f64, file["name"].as_str().unwrap())?; }
            Ok(json!({"files":files,"total":files.len()}))
        })?;
    Ok(registry)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixture_queries_paginate_and_entity_versions_prevent_overwrites() {
        let registry = files().unwrap();
        let result = registry.invoke("files.query", &json!({"limit":1})).unwrap();
        assert_eq!(result.version, 0);
        assert_eq!(result.result.as_ref().unwrap()["hasMore"], true);
        let rename = json!({"fileId":"file-1","name":"改名.md","expectedFileVersion":1});
        let result = registry
            .invoke_with_request_id("files.rename", &rename, Some("rename-1"))
            .unwrap();
        assert_eq!(result.result.as_ref().unwrap()["version"], 2);
        assert_eq!(
            registry
                .invoke_with_request_id("files.rename", &rename, Some("rename-1"))
                .unwrap()
                .version,
            1
        );
        assert_eq!(
            registry.invoke("files.rename", &rename).unwrap_err().code,
            "stale_entity"
        );
        assert_eq!(
            registry
                .invoke(
                    "files.rename",
                    &json!({"fileId":"file-2","name":"改名.md","expectedFileVersion":1})
                )
                .unwrap_err()
                .code,
            "name_conflict"
        );
        assert_eq!(
            registry
                .invoke(
                    "files.rename",
                    &json!({"fileId":"file-2","name":"other.rs","expectedFileVersion":1})
                )
                .unwrap()
                .version,
            2
        );
        let query = registry
            .invoke("files.query", &json!({"query":"改名"}))
            .unwrap();
        assert_eq!(query.version, 2);
        assert_eq!(query.result.unwrap()["items"].as_array().unwrap().len(), 1);
        let query = registry
            .invoke("files.query", &json!({"offset":1000}))
            .unwrap();
        assert!(query.result.unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(registry.observe().version, 2);
    }
}

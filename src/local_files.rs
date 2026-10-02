//! Read-only directory index with app-owned, process-local metadata.

use crate::action::{ActionError, ActionInfo, ActionKind, ActionRegistry};
use crate::action_catalog::ActionOptions;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

const MAX_FILES: usize = 500;
const MAX_NAME_BYTES: usize = 255;
const MAX_NOTE_CHARS: usize = 2_000;

fn info(id: &str, description: &str, input_schema: Value, output_schema: Value) -> ActionInfo {
    ActionInfo {
        id: id.into(),
        description: description.into(),
        input_schema,
        output_schema,
        kind: ActionKind::Write,
    }
}

fn file_id(name: &str) -> String {
    let mut id = String::with_capacity(5 + name.len() * 2);
    id.push_str("file-");
    for byte in name.as_bytes() {
        use std::fmt::Write as _;
        write!(id, "{byte:02x}").unwrap();
    }
    id
}

fn scan(root: &Path, existing: Option<&Value>) -> Result<Vec<Value>, ActionError> {
    let entries = fs::read_dir(root)
        .map_err(|error| ActionError::new("directory_read_failed", error.to_string()))?;
    let mut files = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| ActionError::new("directory_read_failed", error.to_string()))?;
        // symlink_metadata inspects the directory entry itself; it does not
        // follow a link if the entry changed after read_dir returned it.
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| ActionError::new("file_read_failed", error.to_string()))?;
        if !metadata.file_type().is_file() {
            // Never follow symlinks or recurse into directories.
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name.len() > MAX_NAME_BYTES || name.chars().any(char::is_control) {
            continue;
        }
        let id = file_id(&name);
        let old = existing
            .and_then(|state| state["files"].as_array())
            .and_then(|files| files.iter().find(|file| file["id"] == id));
        let version = old.and_then(|file| file["version"].as_u64()).unwrap_or(1);
        let app_metadata = old
            .and_then(|file| file.get("metadata"))
            .cloned()
            .unwrap_or_else(|| json!({"note":""}));
        files.push(json!({
            "id": id,
            "name": name,
            "size": metadata.len(),
            "version": version,
            "metadata": app_metadata
        }));
    }
    files.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    if files.len() > MAX_FILES {
        return Err(ActionError::new(
            "directory_too_large",
            format!("directory contains more than {MAX_FILES} indexable files"),
        ));
    }
    Ok(files)
}

/// Create actions for one explicitly selected directory. File content and paths
/// are never returned, and mutations are limited to process-local app metadata.
pub fn actions(directory: impl AsRef<Path>) -> Result<ActionRegistry, ActionError> {
    let root: PathBuf = directory
        .as_ref()
        .canonicalize()
        .map_err(|error| ActionError::new("directory_unavailable", error.to_string()))?;
    if !root.is_dir() {
        return Err(ActionError::new(
            "not_a_directory",
            "selected path is not a directory",
        ));
    }
    let files = scan(&root, None)?;
    let registry = ActionRegistry::new(
        json!({"files":files,"directoryName":root.file_name().and_then(|name| name.to_str()).unwrap_or("directory")}),
    )?;
    let file_schema = json!({"type":"object","required":["id","name","size","version","metadata"],"additionalProperties":false,"properties":{
        "id":{"type":"string"},"name":{"type":"string"},"size":{"type":"integer","minimum":0},"version":{"type":"integer","minimum":1},
        "metadata":{"type":"object","required":["note"],"additionalProperties":false,"properties":{"note":{"type":"string","maxLength":2000}}}
    }});
    registry.register_query(
        info("local_files.query", "Search the selected directory's top-level file index; file contents are not read",
            json!({"type":"object","additionalProperties":false,"properties":{"query":{"type":"string","maxLength":256},"offset":{"type":"integer","minimum":0,"maximum":500},"limit":{"type":"integer","minimum":1,"maximum":50}}}),
            json!({"type":"object","required":["items","total","offset","hasMore"],"additionalProperties":false,"properties":{"items":{"type":"array","items":file_schema.clone()},"total":{"type":"integer","minimum":0},"offset":{"type":"integer","minimum":0},"hasMore":{"type":"boolean"}}})),
        |state, args| {
            let query = args.get("query").and_then(Value::as_str).unwrap_or("").to_lowercase();
            let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
            let matched: Vec<_> = state["files"].as_array().unwrap().iter()
                .filter(|file| file["name"].as_str().unwrap().to_lowercase().contains(&query)).collect();
            let items: Vec<_> = matched.iter().skip(offset).take(limit).map(|file| (*file).clone()).collect();
            Ok(json!({"items":items,"total":matched.len(),"offset":offset,"hasMore":offset.saturating_add(items.len()) < matched.len()}))
        })?;
    registry.register_with_options(
        info("local_files.metadata.update", "Update app-owned metadata for an indexed file; does not change the filesystem",
            json!({"type":"object","required":["fileId","note","expectedFileVersion"],"additionalProperties":false,"properties":{"fileId":{"type":"string","maxLength":1024},"note":{"type":"string","maxLength":2000},"expectedFileVersion":{"type":"integer","minimum":1}}}), file_schema.clone()),
        ActionOptions::default().with_availability(|state, args| metadata_target(state,args).map(|_| ())),
        |state, args| {
            let index = metadata_target(state, args)?;
            let files = state["files"].as_array_mut().unwrap();
            let old = files[index]["version"].as_u64().unwrap();
            let version = old.checked_add(1).ok_or_else(|| ActionError::new("version_overflow", "file version overflow"))?;
            files[index]["metadata"]["note"] = args["note"].clone();
            files[index]["version"] = json!(version);
            Ok(files[index].clone())
        })?;
    let refresh_root = root.clone();
    registry.register(
        info("local_files.refresh", "Re-read the selected directory's top-level file names and sizes without opening file contents",
            json!({"type":"object","additionalProperties":false}),
            json!({"type":"object","required":["total"],"additionalProperties":false,"properties":{"total":{"type":"integer","minimum":0}}})),
        move |state, _| {
            let fresh = scan(&refresh_root, Some(state))?;
            let total = fresh.len();
            state["files"] = json!(fresh);
            Ok(json!({"total":total}))
        })?;
    Ok(registry)
}

fn metadata_target(state: &Value, args: &Value) -> Result<usize, ActionError> {
    let file_id = args.get("fileId").and_then(Value::as_str).unwrap_or("");
    let note = args.get("note").and_then(Value::as_str).unwrap_or("");
    if note.chars().count() > MAX_NOTE_CHARS {
        return Err(ActionError::new(
            "invalid_metadata",
            "note exceeds 2000 characters",
        ));
    }
    let files = state["files"].as_array().unwrap();
    let index = files
        .iter()
        .position(|file| file["id"] == file_id)
        .ok_or_else(|| {
            ActionError::new(
                "target_missing",
                "indexed file does not exist; refresh first",
            )
        })?;
    let expected = args
        .get("expectedFileVersion")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let current = files[index]["version"].as_u64().unwrap();
    if current != expected {
        return Err(ActionError::new("stale_entity", format!("expected file version {expected}, current version is {current}; refresh before retry")));
    }
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!("lapui-local-files-{unique}"));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn indexes_names_and_sizes_and_only_mutates_app_metadata() {
        let temp = TempDir::new();
        fs::write(temp.0.join("中文.txt"), b"secret contents").unwrap();
        fs::create_dir(temp.0.join("nested")).unwrap();
        let registry = actions(&temp.0).unwrap();
        let observed = registry.observe();
        let file = &observed.state["files"][0];
        assert_eq!(file["name"], "中文.txt");
        assert_eq!(file["size"], 15);
        assert!(!observed.state.to_string().contains("secret contents"));
        assert!(!observed
            .state
            .to_string()
            .contains(temp.0.to_string_lossy().as_ref()));
        let id = file["id"].as_str().unwrap().to_owned();
        registry
            .invoke(
                "local_files.metadata.update",
                &json!({"fileId":id,"note":"说明","expectedFileVersion":1}),
            )
            .unwrap();
        assert!(temp.0.join("中文.txt").exists());
        assert_eq!(registry.observe().state["files"][0]["name"], "中文.txt");
        assert_eq!(
            registry.observe().state["files"][0]["metadata"]["note"],
            "说明"
        );
        assert_eq!(
            registry
                .invoke(
                    "local_files.metadata.update",
                    &json!({"fileId":id,"note":"过期","expectedFileVersion":1})
                )
                .unwrap_err()
                .code,
            "stale_entity"
        );
        fs::write(temp.0.join("new.txt"), b"new").unwrap();
        let refreshed = registry.invoke("local_files.refresh", &json!({})).unwrap();
        assert_eq!(refreshed.result.unwrap()["total"], 2);
        let files = registry.observe().state["files"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(files[0]["name"], "new.txt");
        assert_eq!(files[1]["metadata"]["note"], "说明");
    }
}

//! # `cap.fs` — governed file read/write (Wave 2).
//!
//! A capability handler for `exec.local`. Two ops:
//! - `read`:  args `{ path }` → returns file contents (string).
//! - `write`: args `{ path, content }` → writes the file, returns bytes written.
//!
//! This is the governed stand-in for raw shell file ops: the model invokes it
//! through the pipeline (so `govern` can gate it in Wave 4), and it never shells
//! out. Reads/writes are scoped to whatever path the model names — Wave 4's
//! `gov.policy` is where you'd restrict to an allowed root.

use crate::pipeline::ExecError;
use crate::schema::Value;

/// `cap.fs` handler. Returns a JSON result value.
pub fn handle_fs(args: &Value) -> Result<Value, ExecError> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ExecError("cap.fs requires a `path` string".into()))?;
    let op = args
        .get("op")
        .and_then(|v| v.as_str())
        .unwrap_or("read");

    match op {
        "read" => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| ExecError(format!("cap.fs read {path}: {e}")))?;
            Ok(serde_json::json!({ "path": path, "content": text }))
        }
        "write" => {
            let content = args
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ExecError("cap.fs write requires `content`".into()))?;
            std::fs::write(path, content)
                .map_err(|e| ExecError(format!("cap.fs write {path}: {e}")))?;
            Ok(serde_json::json!({ "path": path, "bytes": content.len() }))
        }
        other => Err(ExecError(format!(
            "cap.fs unknown op `{other}` (expected read|write)"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;


    fn tmp(name: &str) -> String {
        let p = std::env::temp_dir().join(name);
        p.to_string_lossy().to_string()
    }

    #[test]
    fn write_then_read_roundtrip() {
        let p = tmp("pan_capfs_test.txt");
        let _ = std::fs::remove_file(&p);
        let w = handle_fs(&serde_json::json!({ "path": p, "op": "write", "content": "hello fs" })).unwrap();
        assert_eq!(w["bytes"], 8);
        let r = handle_fs(&serde_json::json!({ "path": p, "op": "read" })).unwrap();
        assert_eq!(r["content"], "hello fs");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn read_missing_file_errors() {
        let p = tmp("pan_capfs_missing_xyz.txt");
        let _ = std::fs::remove_file(&p);
        assert!(handle_fs(&serde_json::json!({ "path": p })).is_err());
    }

    #[test]
    fn unknown_op_errors() {
        assert!(handle_fs(&serde_json::json!({ "path": "/tmp/x", "op": "delete" })).is_err());
    }
}

use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;

use crate::domain::Selection;
use crate::error::Result;
use crate::error::SwitcherError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TransformOutcome {
    pub request: Value,
    pub changed: bool,
}

pub fn transform_app_server_request(
    mut request: Value,
    selection: &Selection,
) -> Result<TransformOutcome> {
    validate_selection(selection)?;
    let object = request.as_object_mut().ok_or_else(|| {
        SwitcherError::Validation("App Server request must be a JSON object".to_string())
    })?;
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return Ok(TransformOutcome {
            request,
            changed: false,
        });
    };

    let changed = match method {
        "model/list" => {
            params_mut(object)?.insert("includeHidden".into(), Value::Bool(true));
            true
        }
        "thread/list" => {
            params_mut(object)?.insert("modelProviders".into(), Value::Array(Vec::new()));
            true
        }
        "thread/start" => {
            let params = params_mut(object)?;
            params.insert("model".into(), Value::String(selection.model_id.clone()));
            params.insert(
                "modelProvider".into(),
                Value::String(selection.provider_id.clone()),
            );
            true
        }
        _ => false,
    };

    Ok(TransformOutcome { request, changed })
}

fn params_mut(object: &mut Map<String, Value>) -> Result<&mut Map<String, Value>> {
    if !object.contains_key("params") {
        object.insert("params".to_string(), Value::Object(Map::new()));
    }
    object
        .get_mut("params")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| {
            SwitcherError::Validation("App Server request params must be an object".to_string())
        })
}

fn validate_selection(selection: &Selection) -> Result<()> {
    for (label, value) in [
        ("provider id", selection.provider_id.as_str()),
        ("model id", selection.model_id.as_str()),
    ] {
        if value.is_empty()
            || value.len() > 128
            || value.chars().any(|character| character.is_control())
        {
            return Err(SwitcherError::Validation(format!(
                "invalid selection {label}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn selection() -> Selection {
        Selection {
            provider_id: "acme".into(),
            model_id: "acme-code".into(),
        }
    }

    #[test]
    fn sets_provider_only_for_new_threads() {
        let started = transform_app_server_request(
            json!({"id": 1, "method": "thread/start", "params": {"cwd": "/repo"}}),
            &selection(),
        )
        .unwrap();
        assert_eq!(started.request["params"]["model"], "acme-code");
        assert_eq!(started.request["params"]["modelProvider"], "acme");

        let resumed = transform_app_server_request(
            json!({"id": 2, "method": "thread/resume", "params": {"threadId": "x"}}),
            &selection(),
        )
        .unwrap();
        assert!(!resumed.changed);
        assert!(resumed.request["params"].get("modelProvider").is_none());
    }

    #[test]
    fn requests_all_provider_models() {
        let models =
            transform_app_server_request(json!({"method": "model/list"}), &selection()).unwrap();
        assert_eq!(models.request["params"]["includeHidden"], true);

        let threads =
            transform_app_server_request(json!({"method": "thread/list"}), &selection()).unwrap();
        assert_eq!(threads.request["params"]["modelProviders"], json!([]));
    }

    #[test]
    fn passes_unknown_methods_through() {
        let original = json!({"method": "turn/start", "params": {"threadId": "x"}});
        let output = transform_app_server_request(original.clone(), &selection()).unwrap();
        assert!(!output.changed);
        assert_eq!(output.request, original);
    }
}

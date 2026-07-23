use board_core::launch::{ExecutionSpec, RunLaunchSpec};
use serde_json::json;

fn execution() -> ExecutionSpec {
    ExecutionSpec {
        argv: vec!["agent".into(), "arg\nwith\0bytes  ".into()],
        env: vec![("EXACT".into(), "value\n\0  ".into())],
        agent_kind: Some("pi".into()),
        initial_prompt: Some("task\n  ".into()),
        system_prompt: None,
    }
}

#[test]
fn v1_launch_spec_has_explicit_tag_and_roundtrips_exactly() {
    let spec = RunLaunchSpec::v1(execution());
    let encoded = serde_json::to_value(&spec).unwrap();
    assert_eq!(encoded["version"], json!(1));
    assert_eq!(
        serde_json::from_value::<RunLaunchSpec>(encoded).unwrap(),
        spec
    );
    assert_eq!(spec.execution(), &execution());
}

#[test]
fn unsupported_launch_spec_version_is_rejected() {
    let error = serde_json::from_value::<RunLaunchSpec>(json!({
        "version": 2,
        "execution": {
            "argv": [], "env": [], "agent_kind": null,
            "initial_prompt": null, "system_prompt": null
        }
    }))
    .unwrap_err();
    assert!(error.to_string().contains("version"), "{error}");
}

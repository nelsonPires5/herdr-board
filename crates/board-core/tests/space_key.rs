use std::collections::HashSet;

use board_core::model::SpaceKey;
use board_core::protocol::SpaceKind;

#[test]
fn typed_space_key_preserves_session_kind_default_and_null_identity() {
    let default_null = SpaceKey {
        session: None,
        kind: SpaceKind::Workspace,
        reference: None,
    };
    let same = default_null.clone();
    let explicit_empty_session = SpaceKey {
        session: Some(String::new()),
        ..default_null.clone()
    };
    let explicit_empty_ref = SpaceKey {
        reference: Some(String::new()),
        ..default_null.clone()
    };
    let named_session = SpaceKey {
        session: Some("other".into()),
        ..default_null.clone()
    };
    let new_workspace = SpaceKey {
        kind: SpaceKind::NewWorkspace,
        ..default_null.clone()
    };

    let keys = HashSet::from([
        default_null,
        same,
        explicit_empty_session,
        explicit_empty_ref,
        named_session,
        new_workspace,
    ]);
    assert_eq!(keys.len(), 5);
}

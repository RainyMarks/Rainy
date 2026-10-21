//! Replies captured from a real Host parse into the protocol types.

#[test]
fn parses_models_status() {
    let status: rainy_protocol::ModelsStatus = serde_json::from_str(include_str!("models-status.json")).unwrap();
    assert_eq!(status.selected.unwrap().provider, "qa");
    assert_eq!(status.presets.len(), 3);
}

use utoipa::OpenApi;

#[test]
fn checked_in_contract_matches_server() {
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("../api/openapi.json")).unwrap();
    let actual = serde_json::to_value(flare::api::ApiDoc::openapi()).unwrap();
    assert_eq!(
        actual, expected,
        "Regenerate with cargo run --example export_openapi > api/openapi.json"
    );
}

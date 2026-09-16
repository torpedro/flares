use utoipa::OpenApi;

fn main() {
    println!(
        "{}",
        flare::api::ApiDoc::openapi().to_pretty_json().unwrap()
    );
}

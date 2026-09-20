use utoipa::OpenApi;

fn main() {
    println!(
        "{}",
        flares::api::ApiDoc::openapi().to_pretty_json().unwrap()
    );
}

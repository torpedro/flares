use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Path, Query, Request, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use subtle::ConstantTimeEq;
use utoipa::{
    Modify, OpenApi,
    openapi::security::{Http, HttpAuthScheme, SecurityScheme},
};

use crate::{
    config::Secret,
    models::*,
    notifications::Notifier,
    store::{Store, StoreError},
};

#[derive(Clone)]
struct AppState {
    store: Store,
    notifier: Option<Arc<dyn Notifier>>,
    token: Secret,
}

pub struct ApiError(StatusCode, &'static str);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (
            self.0,
            Json(ErrorBody {
                detail: self.1.into(),
            }),
        )
            .into_response();
        if self.0 == StatusCode::UNAUTHORIZED {
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                header::HeaderValue::from_static("Bearer"),
            );
        }
        response
    }
}

impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::NotFound => Self(StatusCode::NOT_FOUND, "Issue not found"),
            _ => {
                tracing::error!("Issue database operation failed");
                Self(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Issue storage is unavailable",
                )
            }
        }
    }
}

fn invalid(detail: &'static str) -> ApiError {
    ApiError(StatusCode::UNPROCESSABLE_ENTITY, detail)
}

fn json_body<T>(value: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    value
        .map(|Json(value)| value)
        .map_err(|error| ApiError(error.status(), "Invalid JSON body: check fields and types"))
}

async fn authenticate(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|header| header.to_str().ok())
        .and_then(|text| text.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, value)| value);
    if !token
        .is_some_and(|token| bool::from(token.as_bytes().ct_eq(state.token.expose().as_bytes())))
    {
        return ApiError(StatusCode::UNAUTHORIZED, "Invalid or missing bearer token")
            .into_response();
    }
    next.run(request).await
}

pub fn router(store: Store, notifier: Option<Arc<dyn Notifier>>, token: Secret) -> Router {
    let state = AppState {
        store,
        notifier,
        token,
    };
    let protected = Router::new()
        .route("/v1/issues/open", post(open_issue))
        .route("/v1/issues/close", post(close_issue))
        .route("/v1/issues", get(list_issues))
        // An encoded slash remains part of an opaque ID. Static action paths also support lookup.
        .route("/v1/issues/open", get(get_open_id))
        .route("/v1/issues/close", get(get_close_id))
        .route("/v1/issues/{*id}", get(get_issue))
        .route("/v1/issue", get(lookup_issue))
        .route_layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .with_state(state);
    Router::new()
        .merge(protected)
        .route("/openapi.json", get(|| async { Json(ApiDoc::openapi()) }))
        .fallback(|| async { ApiError(StatusCode::NOT_FOUND, "Endpoint not found") })
        .layer(DefaultBodyLimit::max(32 * 1024))
}

#[utoipa::path(post, path = "/v1/issues/open", request_body = OpenIssue,
    responses((status = 200, body = MutationResult), (status = 401, body = ErrorBody), (status = 422, body = ErrorBody)),
    security(("bearer_token" = [])))]
async fn open_issue(
    State(state): State<AppState>,
    body: Result<Json<OpenIssue>, JsonRejection>,
) -> Result<Json<MutationResult>, ApiError> {
    let request = json_body(body)?;
    request.validate().map_err(invalid)?;
    // Complete the committed operation even if the HTTP client disconnects while waiting.
    tokio::spawn(async move {
        let (mut issue, changed) = state.store.open(request, state.notifier.is_some()).await?;
        let mut outcome = Notification::not_attempted();
        if let Some(notifier) = state.notifier.filter(|_| changed) {
            outcome = notifier.send(&issue.title, &issue.message).await;
            state
                .store
                .record_notification(issue.id.clone(), issue.opening_count, outcome.clone())
                .await?;
            issue.notification = outcome.clone();
            if outcome.status == NotificationStatus::Failed {
                tracing::warn!("Opening saved, but its notification attempt failed");
            }
        }
        Ok(Json(MutationResult {
            issue,
            changed,
            notification: outcome,
        }))
    })
    .await
    .map_err(|_| {
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Opening interrupted; check issue state",
        )
    })?
}

#[utoipa::path(post, path = "/v1/issues/close", request_body = CloseIssue,
    responses((status = 200, body = MutationResult), (status = 401, body = ErrorBody), (status = 404, body = ErrorBody), (status = 422, body = ErrorBody)),
    security(("bearer_token" = [])))]
async fn close_issue(
    State(state): State<AppState>,
    body: Result<Json<CloseIssue>, JsonRejection>,
) -> Result<Json<MutationResult>, ApiError> {
    let request = json_body(body)?;
    validate_id(&request.id).map_err(invalid)?;
    let (issue, changed) = state.store.close(request.id).await?;
    Ok(Json(MutationResult {
        issue,
        changed,
        notification: Notification::not_attempted(),
    }))
}

#[utoipa::path(get, path = "/v1/issues/{id}", params(("id" = String, Path, description = "URL-encoded issue ID")),
    responses((status = 200, body = Issue), (status = 401, body = ErrorBody), (status = 404, body = ErrorBody)),
    security(("bearer_token" = [])))]
async fn get_issue(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Issue>, ApiError> {
    validate_id(&id).map_err(invalid)?;
    Ok(Json(state.store.get(id).await?))
}

async fn get_open_id(state: State<AppState>) -> Result<Json<Issue>, ApiError> {
    get_issue(state, Path("open".into())).await
}
async fn get_close_id(state: State<AppState>) -> Result<Json<Issue>, ApiError> {
    get_issue(state, Path("close".into())).await
}

#[derive(Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
struct LookupQuery {
    id: String,
}

#[utoipa::path(get, path = "/v1/issue", params(LookupQuery),
    responses((status = 200, body = Issue), (status = 401, body = ErrorBody), (status = 404, body = ErrorBody)),
    security(("bearer_token" = [])))]
async fn lookup_issue(
    state: State<AppState>,
    query: Result<Query<LookupQuery>, QueryRejection>,
) -> Result<Json<Issue>, ApiError> {
    let Query(query) = query.map_err(|_| invalid("Expected an id query parameter"))?;
    get_issue(state, Path(query.id)).await
}

#[utoipa::path(get, path = "/v1/issues", params(ListQuery),
    responses((status = 200, body = IssueList), (status = 401, body = ErrorBody), (status = 422, body = ErrorBody)),
    security(("bearer_token" = [])))]
async fn list_issues(
    State(state): State<AppState>,
    query: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Json<IssueList>, ApiError> {
    let Query(query) = query.map_err(|_| invalid("Invalid status, limit, or offset"))?;
    if query.limit == 0 || query.limit > 1000 {
        return Err(invalid("limit must be between 1 and 1000"));
    }
    Ok(Json(state.store.list(query).await?))
}

#[derive(OpenApi)]
#[openapi(info(title = "Happer", version = "0.1.0"),
    paths(open_issue, close_issue, get_issue, lookup_issue, list_issues),
    components(schemas(OpenIssue, CloseIssue, Issue, IssueList, MutationResult, Notification, NotificationStatus, IssueStatus, ErrorBody)),
    modifiers(&Security))]
pub struct ApiDoc;

struct Security;
impl Modify for Security {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "bearer_token",
                SecurityScheme::Http(Http::new(HttpAuthScheme::Bearer)),
            );
        }
    }
}

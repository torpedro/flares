use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Path, Query, Request, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::{HeaderMap, StatusCode, header},
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
    delivery: crate::delivery::DeliveryService,
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
            StoreError::NotFound => Self(StatusCode::NOT_FOUND, "Resource not found"),
            StoreError::Conflict => Self(
                StatusCode::CONFLICT,
                "Idempotency key already used for a different request",
            ),
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
    router_with_delivery(
        crate::delivery::DeliveryService::new(store, notifier),
        token,
    )
}

pub fn router_with_delivery(delivery: crate::delivery::DeliveryService, token: Secret) -> Router {
    let state = AppState {
        store: delivery.store.clone(),
        delivery,
        token,
    };
    let protected = Router::new()
        .route("/v1/alerts", post(send_alert))
        .route("/v1/deliveries/{id}", get(get_delivery))
        .route(
            "/v1/heartbeats",
            get(list_heartbeats).post(register_heartbeat),
        )
        .route("/v1/heartbeats/check-in", post(check_in))
        .route("/v1/heartbeat", axum::routing::delete(delete_heartbeat))
        .route("/metrics", get(metrics))
        .route("/v1/issues/open", post(open_issue))
        .route("/v1/issues/close", post(close_issue))
        .route("/v1/issues", get(list_issues))
        // An encoded slash remains part of an opaque ID. Static action paths also support lookup.
        .route("/v1/issues/open", get(get_open_id))
        .route("/v1/issues/close", get(get_close_id))
        .route("/v1/issues/{*id}", get(get_issue))
        .route("/v1/issue", get(lookup_issue))
        .route_layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .with_state(state.clone());
    Router::new()
        .merge(protected)
        .route("/healthz", get(health))
        .route("/readyz", get(readiness).with_state(state))
        .route("/openapi.json", get(|| async { Json(ApiDoc::openapi()) }))
        .fallback(|| async { ApiError(StatusCode::NOT_FOUND, "Endpoint not found") })
        .layer(DefaultBodyLimit::max(32 * 1024))
}

/// Send a one-shot notification without creating or updating an issue.
#[utoipa::path(post, path = "/v1/alerts", request_body = Alert,
    params(("Idempotency-Key" = Option<String>, Header, description = "Retry key; reuse with the same alert body")),
    responses((status = 409, body = ErrorBody), (status = 200, body = AlertResult), (status = 401, body = ErrorBody), (status = 422, body = ErrorBody)),
    security(("bearer_token" = [])))]
async fn send_alert(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Alert>, JsonRejection>,
) -> Result<Json<AlertResult>, ApiError> {
    let request = json_body(body)?;
    request.validate().map_err(invalid)?;
    let key = headers
        .get("idempotency-key")
        .map(|value| {
            let key = value
                .to_str()
                .map_err(|_| invalid("Invalid Idempotency-Key header"))?;
            if key.is_empty() || key.len() > 200 || !key.bytes().all(|b| b.is_ascii_graphic()) {
                return Err(invalid(
                    "Idempotency-Key must contain 1–200 printable ASCII characters",
                ));
            }
            Ok(key.to_owned())
        })
        .transpose()?;
    let job = tokio::spawn(async move { state.delivery.alert(request, key).await })
        .await
        .map_err(|_| {
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Alert interrupted; use the same idempotency key when retrying",
            )
        })??;
    Ok(Json(AlertResult {
        delivery_id: job.id,
        notification: job.notification,
    }))
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
        let plan = state.delivery.plan(request.severity);
        let (mut issue, changed, delivery_id) = state
            .store
            .open_planned(request, !plan.destinations.is_empty(), Some(plan))
            .await?;
        let mut outcome = Notification::not_attempted();
        if let Some(id) = delivery_id {
            outcome = state.delivery.process(id).await?.notification;
            issue.notification = outcome.clone();
        }
        Ok(Json(MutationResult {
            delivery_id,
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
    tokio::spawn(async move {
        let (issue, changed, delivery_id) = state
            .store
            .close_planned(request.id, Some(state.delivery.clone()))
            .await?;
        let notification = if let Some(id) = delivery_id {
            state.delivery.process(id).await?.notification
        } else {
            Notification::not_attempted()
        };
        Ok(Json(MutationResult {
            delivery_id,
            issue,
            changed,
            notification,
        }))
    })
    .await
    .map_err(|_| {
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Closing interrupted; check issue state",
        )
    })?
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
#[openapi(info(title = "Flare"),
    paths(health, readiness, metrics, get_delivery, register_heartbeat, list_heartbeats, check_in, delete_heartbeat, send_alert, open_issue, close_issue, get_issue, lookup_issue, list_issues),
    components(schemas(Delivery, DestinationOutcome, Severity, Heartbeat, HeartbeatInput, Alert, AlertResult, OpenIssue, CloseIssue, Issue, IssueList, MutationResult, Notification, NotificationStatus, IssueStatus, ErrorBody)),
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

#[utoipa::path(get,path="/healthz",responses((status=200,description="Process is running")))]
async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status":"ok"}))
}

#[utoipa::path(get,path="/readyz",responses((status=200,description="Database is accessible"),(status=503,body=ErrorBody)))]
async fn readiness(State(state): State<AppState>) -> Result<Json<serde_json::Value>, ApiError> {
    state
        .store
        .run(|db| {
            db.query_row("SELECT COUNT(*) FROM delivery_metrics", [], |r| {
                r.get::<_, i64>(0)
            })?;
            Ok(())
        })
        .await?;
    Ok(Json(serde_json::json!({"status":"ready"})))
}
#[utoipa::path(get,path="/metrics",responses((status=200,body=String,content_type="text/plain"),(status=401,body=ErrorBody)),security(("bearer_token"=[])))]
async fn metrics(State(state): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    Ok((
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        state.delivery.metrics().await?,
    ))
}
#[utoipa::path(get,path="/v1/deliveries/{id}",params(("id"=i64,Path)),responses((status=200,body=Delivery),(status=404,body=ErrorBody)),security(("bearer_token"=[])))]
async fn get_delivery(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Delivery>, ApiError> {
    Ok(Json(state.delivery.get(id).await?))
}
#[utoipa::path(post,path="/v1/heartbeats",request_body=HeartbeatInput,responses((status=200,body=Heartbeat),(status=422,body=ErrorBody)),security(("bearer_token"=[])))]
async fn register_heartbeat(
    State(state): State<AppState>,
    body: Result<Json<HeartbeatInput>, JsonRejection>,
) -> Result<Json<Heartbeat>, ApiError> {
    let request = json_body(body)?;
    request.validate().map_err(invalid)?;
    Ok(Json(state.delivery.register_heartbeat(request).await?))
}
#[utoipa::path(get,path="/v1/heartbeats",responses((status=200,body=Vec<Heartbeat>)),security(("bearer_token"=[])))]
async fn list_heartbeats(State(state): State<AppState>) -> Result<Json<Vec<Heartbeat>>, ApiError> {
    Ok(Json(state.delivery.heartbeats().await?))
}
#[utoipa::path(post,path="/v1/heartbeats/check-in",request_body=CloseIssue,responses((status=200,body=Heartbeat),(status=404,body=ErrorBody)),security(("bearer_token"=[])))]
async fn check_in(
    State(state): State<AppState>,
    body: Result<Json<CloseIssue>, JsonRejection>,
) -> Result<Json<Heartbeat>, ApiError> {
    let request = json_body(body)?;
    validate_id(&request.id).map_err(invalid)?;
    Ok(Json(state.delivery.check_in(request.id).await?))
}
#[utoipa::path(delete,path="/v1/heartbeat",params(LookupQuery),responses((status=200,description="Monitor deleted"),(status=404,body=ErrorBody)),security(("bearer_token"=[])))]
async fn delete_heartbeat(
    State(state): State<AppState>,
    query: Result<Query<LookupQuery>, QueryRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Query(query) = query.map_err(|_| invalid("Expected an id query parameter"))?;
    validate_id(&query.id).map_err(invalid)?;
    state.delivery.delete_heartbeat(query.id).await?;
    Ok(Json(serde_json::json!({"deleted":true})))
}

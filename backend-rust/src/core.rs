use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{SecondsFormat, Utc};
use futures::TryStreamExt;
use mongodb::bson::{doc, Document};
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc};

pub type Result<T, E = ApiError> = std::result::Result<T, E>;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}
impl ApiError {
    pub fn bad(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.into(),
        }
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: message.into(),
        }
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: message.into(),
        }
    }
}
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for ApiError {}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if self.status.is_server_error() {
            tracing::error!(status=%self.status,error=%self.message,"request failed");
        }
        let message = if self.status == StatusCode::INTERNAL_SERVER_ERROR {
            "Internal server error".to_string()
        } else {
            self.message
        };
        let code = if self.status == StatusCode::UNAUTHORIZED {
            401
        } else if self.status == StatusCode::FORBIDDEN {
            103
        } else {
            self.status.as_u16()
        };
        (
            self.status,
            Json(json!({"code":code,"error":"ApiError","message":message})),
        )
            .into_response()
    }
}
impl From<mongodb::error::Error> for ApiError {
    fn from(error: mongodb::error::Error) -> Self {
        if error.to_string().contains("E11000") {
            Self::bad("A record with this identity already exists")
        } else if error.contains_label("TransientTransactionError") {
            Self {
                status: StatusCode::CONFLICT,
                message: "A concurrent operation changed this record; retry the operation".into(),
            }
        } else {
            Self::internal(error.to_string())
        }
    }
}
impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        Self::internal(e.to_string())
    }
}
impl From<reqwest::Error> for ApiError {
    fn from(e: reqwest::Error) -> Self {
        Self::internal(e.without_url().to_string())
    }
}
impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self::internal(e.to_string())
    }
}
impl From<mongodb::bson::ser::Error> for ApiError {
    fn from(e: mongodb::bson::ser::Error) -> Self {
        Self::internal(e.to_string())
    }
}
impl From<mongodb::bson::de::Error> for ApiError {
    fn from(e: mongodb::bson::de::Error) -> Self {
        Self::internal(e.to_string())
    }
}
impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        Self::internal(e.to_string())
    }
}

pub struct AppState {
    pub mongo: mongodb::Client,
    pub db: mongodb::Database,
    pub http: reqwest::Client,
    pub cos: crate::storage::Cos,
}
impl AppState {
    pub async fn get(&self, collection: &str, id: &str) -> Result<Value> {
        let d = self
            .db
            .collection::<Document>(collection)
            .find_one(doc! {"id":id})
            .await?
            .ok_or_else(|| ApiError::not_found(format!("{collection} record not found")))?;
        to_value(d)
    }
    pub async fn find(&self, collection: &str, filter: Document) -> Result<Vec<Value>> {
        let documents = self
            .db
            .collection::<Document>(collection)
            .find(filter)
            .await?
            .try_collect::<Vec<_>>()
            .await?;
        documents.into_iter().map(to_value).collect()
    }
    pub async fn insert(&self, collection: &str, mut value: Value) -> Result<Value> {
        let object = value
            .as_object_mut()
            .ok_or_else(|| ApiError::bad("Expected an object"))?;
        if !object.contains_key("id") {
            object.insert("id".into(), json!(id()));
        }
        if !object.contains_key("created_at") {
            object.insert("created_at".into(), json!(now()));
        }
        let mut d = mongodb::bson::to_document(&value)?;
        d.insert("_id", field(&value, "id"));
        self.db
            .collection::<Document>(collection)
            .insert_one(d)
            .await?;
        Ok(value)
    }
    pub async fn patch(&self, collection: &str, id: &str, mut fields: Value) -> Result<Value> {
        let object = fields
            .as_object_mut()
            .ok_or_else(|| ApiError::bad("Expected an object"))?;
        object.remove("id");
        object.remove("_id");
        object.remove("created_at");
        if object.keys().any(|k| k.starts_with('$') || k.contains('.')) {
            return Err(ApiError::bad("Invalid field name"));
        }
        object.insert("updated_at".into(), json!(now()));
        let update = mongodb::bson::to_document(&fields)?;
        let d = self
            .db
            .collection::<Document>(collection)
            .find_one_and_update(doc! {"id":id}, doc! {"$set":update})
            .return_document(mongodb::options::ReturnDocument::After)
            .await?
            .ok_or_else(|| ApiError::not_found(format!("{collection} record not found")))?;
        to_value(d)
    }
    pub async fn remove(&self, collection: &str, id: &str) -> Result<()> {
        self.db
            .collection::<Document>(collection)
            .delete_one(doc! {"id":id})
            .await?;
        Ok(())
    }
}
pub fn to_value(mut d: Document) -> Result<Value> {
    d.remove("_id");
    Ok(mongodb::bson::from_document(d)?)
}
pub fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}
pub fn epoch() -> i64 {
    Utc::now().timestamp()
}
pub fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
pub fn env(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_owned())
}
pub fn field<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}
pub fn required(v: &Value, key: &str) -> Result<String> {
    let text = field(v, key).trim();
    if text.is_empty() || text.len() > 4096 {
        return Err(ApiError::bad(format!(
            "{key} is required and must be at most 4096 bytes"
        )));
    }
    Ok(text.to_owned())
}
pub fn public_user(v: &Value) -> Value {
    let mut result = serde_json::Map::new();
    for k in [
        "id",
        "name",
        "email",
        "signature",
        "avatar",
        "admin",
        "banned",
        "disabled",
        "locale",
        "timezone",
        "created_at",
        "create_time",
    ] {
        if let Some(value) = v.get(k) {
            result.insert(k.into(), value.clone());
        }
    }
    result.insert(
        "has_avatar".into(),
        json!(v
            .get("avatar_resource_id")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())),
    );
    Value::Object(result)
}
pub struct Request {
    pub state: Arc<AppState>,
    pub user: Option<Value>,
    pub method: Method,
    pub path: Vec<String>,
    pub query: HashMap<String, String>,
    pub body: Value,
}
impl Request {
    pub fn uid(&self) -> Result<String> {
        self.user
            .as_ref()
            .map(|u| field(u, "id").to_owned())
            .filter(|id| !id.is_empty())
            .ok_or_else(|| ApiError::unauthorized("Authentication required"))
    }
    pub fn is_admin(&self) -> bool {
        self.user
            .as_ref()
            .and_then(|u| u.get("admin"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
    pub fn q(&self, key: &str) -> &str {
        self.query.get(key).map(String::as_str).unwrap_or("")
    }
}
pub struct Reply {
    pub data: Value,
    pub count: Option<u64>,
}
impl From<Value> for Reply {
    fn from(data: Value) -> Self {
        Self { data, count: None }
    }
}
impl Reply {
    pub fn list(values: Vec<Value>, r: &Request) -> Self {
        let count = values.len() as u64;
        if r.q("paging") == "false" {
            return Self {
                data: Value::Array(values),
                count: Some(count),
            };
        }
        let page = r.q("page").parse::<usize>().unwrap_or(1).max(1);
        let limit = r.q("limit").parse::<usize>().unwrap_or(30).clamp(1, 100);
        let start = page.saturating_sub(1).saturating_mul(limit);
        Self {
            data: Value::Array(values.into_iter().skip(start).take(limit).collect()),
            count: Some(count),
        }
    }
}
impl IntoResponse for Reply {
    fn into_response(self) -> Response {
        let mut response = Json(self.data).into_response();
        if let Some(count) = self.count {
            if let Ok(value) = count.to_string().parse() {
                response.headers_mut().insert("x-pagination-count", value);
            }
        }
        response
    }
}

pub async fn indexes(state: &AppState) -> Result<()> {
    for collection in [
        "users",
        "sessions",
        "verification",
        "captchas",
        "site_settings",
        "languages",
        "teams",
        "projects",
        "project_sets",
        "memberships",
        "invitations",
        "applications",
        "term_banks",
        "terms",
        "files",
        "targets",
        "sources",
        "translations",
        "tips",
        "uploads",
        "upload_sessions",
        "upload_guards",
        "resources",
        "resource_refs",
        "quota_accounts",
        "jobs",
        "outputs",
        "roles",
    ] {
        state
            .db
            .collection::<Document>(collection)
            .create_index(
                mongodb::IndexModel::builder()
                    .keys(doc! {"id":1})
                    .options(
                        mongodb::options::IndexOptions::builder()
                            .unique(true)
                            .build(),
                    )
                    .build(),
            )
            .await?;
    }
    for (collection, keys, unique) in [
        ("users", doc! {"email":1}, true),
        ("users", doc! {"name":1}, true),
        (
            "memberships",
            doc! {"group_type":1,"group_id":1,"user_id":1},
            true,
        ),
        ("jobs", doc! {"status":1,"available_at":1}, false),
        ("jobs", doc! {"status":1,"lease_until":1}, false),
        ("jobs", doc! {"kind":1,"dedup_key":1}, true),
        ("resources", doc! {"state":1,"expires_at":1}, false),
        (
            "resources",
            doc! {"owner_id":1,"state":1,"purpose":1,"expires_at":1},
            false,
        ),
        (
            "resources",
            doc! {"team_id":1,"state":1,"purpose":1,"expires_at":1},
            false,
        ),
        (
            "files",
            doc! {"project_id":1,"parent_id":1,"activated":1},
            false,
        ),
        ("sources", doc! {"file_id":1,"rank":1}, false),
        (
            "translations",
            doc! {"source_id":1,"target_id":1,"user_id":1},
            true,
        ),
        ("resource_refs", doc! {"resource_id":1}, false),
    ] {
        state
            .db
            .collection::<Document>(collection)
            .create_index(
                mongodb::IndexModel::builder()
                    .keys(keys)
                    .options(
                        mongodb::options::IndexOptions::builder()
                            .unique(unique)
                            .build(),
                    )
                    .build(),
            )
            .await?;
    }
    state
        .db
        .collection::<Document>("files")
        .create_index(
            mongodb::IndexModel::builder()
                .keys(doc! {"project_id":1,"parent_id":1,"name_key":1})
                .options(
                    mongodb::options::IndexOptions::builder()
                        .unique(true)
                        .partial_filter_expression(doc! {"activated":true,"deleted_at":null})
                        .build(),
                )
                .build(),
        )
        .await?;
    state
        .db
        .collection::<Document>("targets")
        .create_index(
            mongodb::IndexModel::builder()
                .keys(doc! {"project_id":1,"language_id":1})
                .options(
                    mongodb::options::IndexOptions::builder()
                        .name("active_target_language".to_owned())
                        .unique(true)
                        .partial_filter_expression(doc! {"deleted_at":null})
                        .build(),
                )
                .build(),
        )
        .await?;
    for collection in ["invitations", "applications"] {
        state
            .db
            .collection::<Document>(collection)
            .create_index(
                mongodb::IndexModel::builder()
                    .keys(doc! {"group_type":1,"group_id":1,"user_id":1})
                    .options(
                        mongodb::options::IndexOptions::builder()
                            .unique(true)
                            .partial_filter_expression(doc! {"status":0})
                            .build(),
                    )
                    .build(),
            )
            .await?;
    }
    Ok(())
}

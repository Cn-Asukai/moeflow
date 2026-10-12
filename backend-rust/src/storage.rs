use crate::core::{self, ApiError, AppState, Reply, Request, Result};
use futures::TryStreamExt;
use hmac::{Hmac, Mac};
use mongodb::bson::{doc, Document};
use reqwest::Method;
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::collections::BTreeMap;
use std::future::Future;

/// Tencent COS v5 signing, not AWS SigV4. Algorithm matches Tencent's official
/// cos-nodejs-sdk-v5/sdk/util.js getAuth implementation.
#[derive(Clone)]
pub struct Cos {
    endpoint: String,
    bucket: String,
    region: String,
    secret_id: String,
    secret_key: String,
}
pub struct ObjectInfo {
    pub size: u64,
    pub etag: String,
    pub content_type: String,
}
fn encode(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
fn hmac(key: &[u8], value: &str) -> String {
    let mut mac = Hmac::<Sha1>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(value.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}
impl Cos {
    pub fn new() -> Result<Self> {
        let bucket = core::env("COS_BUCKET", "");
        let region = core::env("COS_REGION", "");
        let secret_id = core::env("COS_SECRET_ID", "");
        let secret_key = core::env("COS_SECRET_KEY", "");
        let custom_endpoint = core::env("COS_ENDPOINT", "");
        if [&bucket, &region, &secret_id, &secret_key, &custom_endpoint]
            .iter()
            .all(|value| value.is_empty())
        {
            return Ok(Self {
                endpoint: String::new(),
                bucket,
                region,
                secret_id,
                secret_key,
            });
        }
        if [&bucket, &region, &secret_id, &secret_key]
            .iter()
            .any(|value| value.is_empty())
        {
            return Err(ApiError::bad("COS_REGION, COS_BUCKET, COS_SECRET_ID and COS_SECRET_KEY must all be set, or all COS configuration omitted"));
        }
        let endpoint = if custom_endpoint.is_empty() {
            format!("https://{bucket}.cos.{region}.myqcloud.com")
        } else {
            custom_endpoint
        };
        let url =
            reqwest::Url::parse(&endpoint).map_err(|_| ApiError::bad("invalid COS_ENDPOINT"))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(ApiError::bad(
                "COS_ENDPOINT must be a bucket origin without credentials/path/query",
            ));
        }
        Ok(Self {
            endpoint: endpoint.trim_end_matches('/').to_owned(),
            bucket,
            region,
            secret_id,
            secret_key,
        })
    }
    pub fn configured(&self) -> bool {
        !self.bucket.is_empty()
            && !self.region.is_empty()
            && !self.secret_id.is_empty()
            && !self.secret_key.is_empty()
    }
    pub fn presign(
        &self,
        method: &str,
        key: &str,
        ttl: u64,
        headers: &[(String, String)],
    ) -> Result<String> {
        self.presign_query(method, key, ttl, headers, &[])
    }
    pub fn presign_query(
        &self,
        method: &str,
        key: &str,
        ttl: u64,
        headers: &[(String, String)],
        query: &[(String, String)],
    ) -> Result<String> {
        if !self.configured() {
            return Err(ApiError::unavailable("COS is not configured"));
        }
        if key.starts_with('/') || key.contains(['?', '#', '\r', '\n']) {
            return Err(ApiError::bad("invalid object key"));
        }
        let mut signed = BTreeMap::new();
        let url = reqwest::Url::parse(&self.endpoint)
            .map_err(|_| ApiError::internal("invalid COS origin"))?;
        let host = match url.port() {
            Some(port) => format!("{}:{port}", url.host_str().unwrap()),
            None => url.host_str().unwrap().to_owned(),
        };
        signed.insert("host".to_owned(), host);
        for (k, v) in headers {
            signed.insert(k.to_ascii_lowercase(), v.clone());
        }
        let list = signed
            .keys()
            .map(|s| encode(s).to_ascii_lowercase())
            .collect::<Vec<_>>()
            .join(";");
        let header_string = signed
            .iter()
            .map(|(k, v)| format!("{}={}", encode(k).to_ascii_lowercase(), encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let mut params = BTreeMap::new();
        for (k, v) in query {
            params.insert(encode(k).to_ascii_lowercase(), encode(v));
        }
        let query_string = params
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        let query_list = params.keys().cloned().collect::<Vec<_>>().join(";");
        let time = format!(
            "{};{}",
            core::epoch() - 1,
            core::epoch() + ttl.min(900) as i64
        );
        let sign_key = hmac(self.secret_key.as_bytes(), &time);
        let canonical = format!(
            "{}\n/{}\n{}\n{}\n",
            method.to_ascii_lowercase(),
            key,
            query_string,
            header_string
        );
        let to_sign = format!(
            "sha1\n{}\n{}\n",
            time,
            hex::encode(Sha1::digest(canonical.as_bytes()))
        );
        let signature = hmac(sign_key.as_bytes(), &to_sign);
        let path = key.split('/').map(encode).collect::<Vec<_>>().join("/");
        let suffix = query
            .iter()
            .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        Ok(format!("{}/{path}?q-sign-algorithm=sha1&q-ak={}&q-sign-time={}&q-key-time={}&q-header-list={}&q-url-param-list={}&q-signature={signature}{}",self.endpoint,encode(&self.secret_id),encode(&time),encode(&time),encode(&list),encode(&query_list),if suffix.is_empty(){String::new()}else{format!("&{suffix}")}))
    }
    async fn send(
        &self,
        http: &reqwest::Client,
        method: Method,
        key: &str,
        headers: Vec<(String, String)>,
        body: Option<Vec<u8>>,
    ) -> Result<reqwest::Response> {
        let url = self.presign(method.as_str(), key, 300, &headers)?;
        let mut req = http.request(method.clone(), url);
        req = req.timeout(std::time::Duration::from_secs(60));
        for (k, v) in headers {
            req = req.header(k, v);
        }
        if let Some(bytes) = body {
            req = req.body(bytes);
        }
        let res = req.send().await?;
        if res.status().is_success() || (method == Method::DELETE && res.status().as_u16() == 404) {
            Ok(res)
        } else {
            Err(ApiError::internal(format!(
                "COS {} failed with HTTP {}",
                method,
                res.status().as_u16()
            )))
        }
    }
    pub async fn get_bounded(
        &self,
        http: &reqwest::Client,
        key: &str,
        max_bytes: u64,
    ) -> Result<Vec<u8>> {
        let response = self.send(http, Method::GET, key, vec![], None).await?;
        let expected = response.content_length();
        if expected.is_some_and(|size| size > max_bytes) {
            return Err(ApiError::bad("COS object exceeds permitted download size"));
        }
        let mut data = Vec::new();
        if let Some(size) = expected {
            let capacity = usize::try_from(size)
                .map_err(|_| ApiError::bad("COS object is too large for this platform"))?;
            data.try_reserve_exact(capacity)
                .map_err(|_| ApiError::internal("insufficient memory for bounded COS download"))?;
        }
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.try_next().await? {
            let next = (data.len() as u64)
                .checked_add(chunk.len() as u64)
                .ok_or_else(|| ApiError::bad("COS download size overflow"))?;
            if next > max_bytes {
                return Err(ApiError::bad("COS object exceeds permitted download size"));
            }
            if chunk.len() > data.capacity() - data.len() {
                data.try_reserve(chunk.len()).map_err(|_| {
                    ApiError::internal("insufficient memory for bounded COS download")
                })?;
            }
            data.extend_from_slice(&chunk);
        }
        Ok(data)
    }
    pub async fn put(
        &self,
        http: &reqwest::Client,
        key: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> Result<()> {
        self.send(
            http,
            Method::PUT,
            key,
            vec![("content-type".into(), content_type.into())],
            Some(bytes),
        )
        .await?;
        Ok(())
    }
    pub async fn delete(&self, http: &reqwest::Client, key: &str) -> Result<()> {
        self.send(http, Method::DELETE, key, vec![], None).await?;
        Ok(())
    }
    pub async fn head(&self, http: &reqwest::Client, key: &str) -> Result<ObjectInfo> {
        let res = self.send(http, Method::HEAD, key, vec![], None).await?;
        let header = |name: &str| {
            res.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_owned()
        };
        Ok(ObjectInfo {
            size: header("content-length")
                .parse()
                .map_err(|_| ApiError::internal("COS HEAD omitted size"))?,
            etag: header("etag"),
            content_type: header("content-type"),
        })
    }
    async fn copy_snapshot(
        &self,
        http: &reqwest::Client,
        source: &str,
        dest: &str,
        etag: &str,
    ) -> Result<String> {
        if etag.is_empty() {
            return Err(ApiError::bad("copy requires source ETag"));
        }
        let source_path = source.split('/').map(encode).collect::<Vec<_>>().join("/");
        let headers = vec![
            (
                "x-cos-copy-source".into(),
                format!(
                    "{}.cos.{}.myqcloud.com/{source_path}",
                    self.bucket, self.region
                ),
            ),
            ("x-cos-copy-source-if-match".into(), etag.into()),
        ];
        let response = self.send(http, Method::PUT, dest, headers, None).await?;
        let body = response.text().await?;
        // A copy can return 200 with an embedded XML error after streaming starts.
        if body.contains("<Error>") || !body.contains("<CopyObjectResult") {
            return Err(ApiError::internal("COS conditional copy failed"));
        }
        let result: CopyResult = xml(&body)?;
        if result.etag.is_empty() {
            return Err(ApiError::internal("COS copy omitted ETag"));
        }
        Ok(result.etag)
    }
    async fn multipart_request(
        &self,
        http: &reqwest::Client,
        method: Method,
        key: &str,
        query: Vec<(String, String)>,
        headers: Vec<(String, String)>,
        body: Option<String>,
    ) -> Result<reqwest::Response> {
        let mut request = http.request(
            method.clone(),
            self.presign_query(method.as_str(), key, 300, &headers, &query)?,
        );
        request = request.timeout(std::time::Duration::from_secs(60));
        for (k, v) in headers {
            request = request.header(k, v);
        }
        if let Some(body) = body {
            request = request.body(body);
        }
        let response = request.send().await?;
        if response.status().is_success() || response.status().as_u16() == 404 {
            Ok(response)
        } else {
            Err(ApiError::internal(format!(
                "COS multipart {} failed HTTP {}",
                method,
                response.status().as_u16()
            )))
        }
    }
    async fn multipart_init(
        &self,
        http: &reqwest::Client,
        key: &str,
        mime: &str,
    ) -> Result<String> {
        let response = self
            .multipart_request(
                http,
                Method::POST,
                key,
                vec![("uploads".into(), String::new())],
                vec![("content-type".into(), mime.into())],
                None,
            )
            .await?;
        let value: InitResult = xml(&response.text().await?)?;
        if value.upload_id.is_empty() {
            return Err(ApiError::internal("COS omitted multipart UploadId"));
        }
        Ok(value.upload_id)
    }
    async fn multipart_parts(
        &self,
        http: &reqwest::Client,
        key: &str,
        upload_id: &str,
    ) -> Result<Option<Vec<Part>>> {
        let mut all = Vec::new();
        let mut marker = 0_u32;
        loop {
            let response = self
                .multipart_request(
                    http,
                    Method::GET,
                    key,
                    vec![
                        ("uploadId".into(), upload_id.into()),
                        ("part-number-marker".into(), marker.to_string()),
                        ("max-parts".into(), "1000".into()),
                    ],
                    vec![],
                    None,
                )
                .await?;
            if response.status().as_u16() == 404 {
                return Ok(None);
            }
            let page: PartsResult = xml(&response.text().await?)?;
            all.extend(page.parts);
            if !page.truncated {
                break;
            }
            if page.next_marker <= marker || all.len() > 10000 {
                return Err(ApiError::internal("invalid COS part pagination"));
            }
            marker = page.next_marker;
        }
        all.sort_by_key(|part| part.part_number);
        Ok(Some(all))
    }
    async fn multipart_finish(
        &self,
        http: &reqwest::Client,
        key: &str,
        upload_id: &str,
        parts: &[Part],
    ) -> Result<()> {
        let mut body = String::from("<CompleteMultipartUpload>");
        for part in parts {
            body.push_str(&format!(
                "<Part><PartNumber>{}</PartNumber><ETag>{}</ETag></Part>",
                part.part_number,
                quick_xml::escape::escape(&part.etag)
            ));
        }
        body.push_str("</CompleteMultipartUpload>");
        let response = self
            .multipart_request(
                http,
                Method::POST,
                key,
                vec![("uploadId".into(), upload_id.into())],
                vec![("content-type".into(), "application/xml".into())],
                Some(body),
            )
            .await?;
        let text = response.text().await?;
        if !text.contains("<CompleteMultipartUploadResult") || text.contains("<Error>") {
            return Err(ApiError::internal("COS multipart completion failed"));
        }
        Ok(())
    }
    pub async fn multipart_abort(
        &self,
        http: &reqwest::Client,
        key: &str,
        upload_id: &str,
    ) -> Result<()> {
        self.multipart_request(
            http,
            Method::DELETE,
            key,
            vec![("uploadId".into(), upload_id.into())],
            vec![],
            None,
        )
        .await?;
        Ok(())
    }
    async fn abort_key_uploads(&self, http: &reqwest::Client, key: &str) -> Result<()> {
        let mut key_marker = String::new();
        let mut upload_marker = String::new();
        loop {
            let response = self
                .multipart_request(
                    http,
                    Method::GET,
                    "",
                    vec![
                        ("uploads".into(), String::new()),
                        ("prefix".into(), key.into()),
                        ("key-marker".into(), key_marker.clone()),
                        ("upload-id-marker".into(), upload_marker.clone()),
                    ],
                    vec![],
                    None,
                )
                .await?;
            let page: UploadsResult = xml(&response.text().await?)?;
            for upload in page.uploads {
                if upload.key == key {
                    self.multipart_abort(http, key, &upload.upload_id).await?;
                }
            }
            if !page.truncated {
                break;
            }
            if page.next_key == key_marker && page.next_upload == upload_marker {
                return Err(ApiError::internal("invalid COS upload pagination"));
            }
            key_marker = page.next_key;
            upload_marker = page.next_upload;
        }
        Ok(())
    }
}
fn xml<T: serde::de::DeserializeOwned>(body: &str) -> Result<T> {
    if body.contains("<Error") {
        return Err(ApiError::internal("COS returned an XML operation error"));
    }
    quick_xml::de::from_str(body).map_err(|_| ApiError::internal("invalid COS XML response"))
}
#[derive(serde::Deserialize)]
struct CopyResult {
    #[serde(rename = "ETag")]
    etag: String,
}
#[derive(serde::Deserialize)]
struct InitResult {
    #[serde(rename = "UploadId")]
    upload_id: String,
}
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Part {
    #[serde(rename(deserialize = "PartNumber", serialize = "part_number"))]
    pub part_number: u32,
    #[serde(rename(deserialize = "ETag", serialize = "etag"))]
    pub etag: String,
    #[serde(rename(deserialize = "Size", serialize = "size"))]
    pub size: u64,
}
#[derive(serde::Deserialize)]
struct PartsResult {
    #[serde(rename = "Part", default)]
    parts: Vec<Part>,
    #[serde(rename = "IsTruncated", default)]
    truncated: bool,
    #[serde(rename = "NextPartNumberMarker", default)]
    next_marker: u32,
}
#[derive(serde::Deserialize)]
struct MultipartEntry {
    #[serde(rename = "Key")]
    key: String,
    #[serde(rename = "UploadId")]
    upload_id: String,
}
#[derive(serde::Deserialize)]
struct UploadsResult {
    #[serde(rename = "Upload", default)]
    uploads: Vec<MultipartEntry>,
    #[serde(rename = "IsTruncated", default)]
    truncated: bool,
    #[serde(rename = "NextKeyMarker", default)]
    next_key: String,
    #[serde(rename = "NextUploadIdMarker", default)]
    next_upload: String,
}
fn document(v: &Value) -> Result<Document> {
    mongodb::bson::to_document(v).map_err(|e| ApiError::internal(e.to_string()))
}
fn quota_limit(team: bool) -> i64 {
    core::env(
        if team {
            "TEAM_STORAGE_QUOTA_BYTES"
        } else {
            "USER_STORAGE_QUOTA_BYTES"
        },
        if team { "107374182400" } else { "1073741824" },
    )
    .parse()
    .unwrap_or(0)
}

async fn retry_conflicts<T, F, Fut>(mut operation: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    // Conflict means MongoDB explicitly aborted the transaction. Callbacks
    // perform no COS operation and retain durable binding idempotency markers.
    let mut attempt = 0_u32;
    loop {
        match operation().await {
            Err(error) if error.status == axum::http::StatusCode::CONFLICT && attempt < 7 => {
                let delay =
                    10_u64 * (1_u64 << attempt.min(5)) + u64::from(rand::random::<u8>() % 20);
                attempt += 1;
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
            result => return result,
        }
    }
}

async fn register(
    state: &AppState,
    owner: &str,
    team: Option<&str>,
    name: &str,
    size: i64,
    content_type: &str,
    purpose: &str,
) -> Result<Value> {
    retry_conflicts(|| reserve_once(state, owner, team, name, size, content_type, purpose)).await
}

/// Register the unique formal key and reserve capacity together. If COS fails,
/// the registered object remains eligible for durable cleanup, never an orphan.
async fn reserve_once(
    state: &AppState,
    owner: &str,
    team: Option<&str>,
    name: &str,
    size: i64,
    content_type: &str,
    purpose: &str,
) -> Result<Value> {
    let rid = core::id();
    let account = match team {
        Some(t) => format!("team:{t}"),
        None => format!("user:{owner}"),
    };
    let resource = json!({"id":rid,"owner_id":owner,"team_id":team,"key":format!("formal/{rid}"),"name":name,"size":size,"content_type":content_type,"purpose":purpose,"state":"uploading","expires_at":core::epoch()+86400,"created_at":core::now(),"quota_account":account,"quota_released":false});
    let mut session = state.mongo.start_session().await?;
    session.start_transaction().await?;
    let client_upload = matches!(purpose, "file" | "user_avatar" | "team_avatar" | "import");
    if client_upload {
        // First write to one stable owner row, even when the owner's parallel
        // requests charge different team accounts. Team serialization follows
        // through the quota-account write below; counts need no release counter.
        state
            .db
            .collection::<Document>("upload_guards")
            .update_one(
                doc! {"_id":owner},
                doc! {"$setOnInsert":{"id":owner},"$inc":{"sequence":1_i64}},
            )
            .upsert(true)
            .session(&mut session)
            .await?;
    }
    let quotas = state.db.collection::<Document>("quota_accounts");
    quotas.update_one(doc!{"id":&account},doc!{"$setOnInsert":{"id":&account,"used_bytes":0_i64},"$set":{"limit_bytes":quota_limit(team.is_some())}}).upsert(true).session(&mut session).await?;
    let reserved = quotas
        .update_one(
            doc! {"id":&account,"$expr":{"$lte":[{"$add":["$used_bytes",size]},"$limit_bytes"]}},
            doc! {"$inc":{"used_bytes":size}},
        )
        .session(&mut session)
        .await?;
    if reserved.modified_count != 1 {
        session.abort_transaction().await?;
        return Err(ApiError::bad("storage quota exceeded"));
    }
    if client_upload {
        let resources = state.db.collection::<Document>("resources");
        let active = doc! {"purpose":{"$in":["file","user_avatar","team_avatar","import"]},"state":{"$in":["uploading","copying","binding"]},"expires_at":{"$gt":core::epoch()}};
        let user_limit = core::env("MAX_ACTIVE_UPLOADS_PER_USER", "8")
            .parse::<u64>()
            .unwrap_or(8);
        let mut owned = active.clone();
        owned.insert("owner_id", owner);
        if resources
            .count_documents(owned)
            .session(&mut session)
            .await?
            >= user_limit
        {
            session.abort_transaction().await?;
            return Err(ApiError::bad("active upload limit exceeded for user"));
        }
        if let Some(team_id) = team {
            let team_limit = core::env("MAX_ACTIVE_UPLOADS_PER_TEAM", "32")
                .parse::<u64>()
                .unwrap_or(32);
            let mut grouped = active;
            grouped.insert("team_id", team_id);
            if resources
                .count_documents(grouped)
                .session(&mut session)
                .await?
                >= team_limit
            {
                session.abort_transaction().await?;
                return Err(ApiError::bad("active upload limit exceeded for team"));
            }
        }
    }
    state
        .db
        .collection::<Document>("resources")
        .insert_one(document(&resource)?)
        .session(&mut session)
        .await?;
    session.commit_transaction().await?;
    Ok(resource)
}
pub async fn create_object(
    state: &AppState,
    user: &Value,
    team_id: Option<&str>,
    name: &str,
    bytes: Vec<u8>,
    content_type: &str,
) -> Result<Value> {
    let resource = register(
        state,
        core::field(user, "id"),
        team_id,
        name,
        bytes.len() as i64,
        content_type,
        "generated",
    )
    .await?;
    state
        .cos
        .put(
            &state.http,
            core::field(&resource, "key"),
            bytes,
            content_type,
        )
        .await?;
    state
        .patch(
            "resources",
            core::field(&resource, "id"),
            json!({"state":"unbound","expires_at":core::epoch()+86400}),
        )
        .await
}
pub async fn attach(
    state: &AppState,
    resource_id: &str,
    kind: &str,
    target_id: &str,
) -> Result<()> {
    let mut session = state.mongo.start_session().await?;
    session.start_transaction().await?;
    attach_in_session(state, resource_id, kind, target_id, &mut session).await?;
    session.commit_transaction().await?;
    Ok(())
}
pub async fn attach_in_session(
    state: &AppState,
    resource_id: &str,
    kind: &str,
    target_id: &str,
    session: &mut mongodb::ClientSession,
) -> Result<()> {
    let mut fields = doc! {"state":"bound","updated_at":core::now()};
    if kind == "file" {
        fields.insert("bound_file_id", target_id);
    }
    let changed=state.db.collection::<Document>("resources").update_one(doc!{"id":resource_id,"state":{"$in":["unbound","binding","bound"]},"$or":[{"state":{"$in":["binding","bound"]}},{"expires_at":{"$gt":core::epoch()}}]},doc!{"$set":fields}).session(&mut *session).await?;
    if changed.matched_count != 1 {
        return Err(ApiError::bad("resource expired or entered deletion"));
    }
    let ref_id = format!("{resource_id}:{kind}:{target_id}");
    state.db.collection::<Document>("resource_refs").update_one(doc!{"id":&ref_id},doc!{"$setOnInsert":{"id":ref_id,"resource_id":resource_id,"kind":kind,"target_id":target_id,"created_at":core::now()}}).upsert(true).session(&mut *session).await?;
    Ok(())
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RetirementPolicy {
    RecoveryGrace,
    LifetimeExpired,
}
impl RetirementPolicy {
    fn expires_at(self) -> i64 {
        core::epoch()
            + match self {
                Self::RecoveryGrace => 604800,
                Self::LifetimeExpired => 0,
            }
    }
}
pub async fn retire(
    state: &AppState,
    resource_id: &str,
    kind: &str,
    target_id: &str,
    policy: RetirementPolicy,
) -> Result<()> {
    let mut session = state.mongo.start_session().await?;
    session.start_transaction().await?;
    let refs = state.db.collection::<Document>("resource_refs");
    refs.delete_one(doc! {"id":format!("{resource_id}:{kind}:{target_id}")})
        .session(&mut session)
        .await?;
    if refs
        .count_documents(doc! {"resource_id":resource_id})
        .session(&mut session)
        .await?
        == 0
    {
        let resources = state.db.collection::<Document>("resources");
        let mut expires_at = policy.expires_at();
        if kind == "job" {
            if let Some(resource) = resources
                .find_one(doc! {"id":resource_id})
                .session(&mut session)
                .await?
            {
                let raw_import = resource.get_str("purpose").ok() == Some("import");
                let expired_output = resource.get_str("purpose").ok() == Some("generated")
                    && resource
                        .get_i64("expires_at")
                        .is_ok_and(|deadline| deadline <= core::epoch());
                if (raw_import || expired_output)
                    && !resource.contains_key("bound_file_id")
                    && !resource.contains_key("bound_avatar_id")
                {
                    expires_at = core::epoch();
                }
            }
        }
        let filter = match policy {
            RetirementPolicy::RecoveryGrace => doc! {"id":resource_id,"state":"bound"},
            RetirementPolicy::LifetimeExpired => {
                doc! {"id":resource_id,"state":{"$in":["bound","unbound"]}}
            }
        };
        resources
            .update_one(
                filter,
                doc! {"$set":{"state":"unbound","expires_at":expires_at}},
            )
            .session(&mut session)
            .await?;
    }
    session.commit_transaction().await?;
    Ok(())
}
pub async fn release_quota(state: &AppState, resource: &Value) -> Result<()> {
    let account = core::field(resource, "quota_account");
    if account.is_empty() {
        return Ok(());
    }
    let mut session = state.mongo.start_session().await?;
    session.start_transaction().await?;
    let changed = state
        .db
        .collection::<Document>("resources")
        .update_one(
            doc! {"id":core::field(resource,"id"),"state":"deleted","quota_released":false},
            doc! {"$set":{"quota_released":true}},
        )
        .session(&mut session)
        .await?;
    if changed.modified_count == 1 {
        state
            .db
            .collection::<Document>("quota_accounts")
            .update_one(
                doc! {"id":account},
                doc! {"$inc":{"used_bytes":-resource["size"].as_i64().unwrap_or(0)}},
            )
            .session(&mut session)
            .await?;
    }
    session.commit_transaction().await?;
    Ok(())
}
async fn authorize(r: &Request, v: &Value) -> Result<Option<String>> {
    match core::field(v, "purpose") {
        "file" => {
            let p =
                crate::groups::require(r, "project", core::field(v, "project_id"), 1020).await?;
            Ok(Some(core::field(&p, "team_id").to_owned()))
        }
        "team_avatar" => {
            crate::groups::require(r, "team", core::field(v, "team_id"), 10).await?;
            Ok(Some(core::field(v, "team_id").to_owned()))
        }
        "import" => {
            crate::groups::require(r, "team", core::field(v, "team_id"), 1090).await?;
            Ok(Some(core::field(v, "team_id").to_owned()))
        }
        "user_avatar" => {
            r.uid()?;
            Ok(None)
        }
        _ => Err(ApiError::bad("invalid upload purpose")),
    }
}
async fn request_upload(r: &Request) -> Result<Value> {
    let uid = r.uid()?;
    let team = authorize(r, &r.body).await?;
    if !r.state.cos.configured() {
        return Err(ApiError::unavailable("COS is not configured"));
    }
    let name = core::required(&r.body, "name")?;
    if name.contains(['/', '\\', '\0']) || name.len() > 255 {
        return Err(ApiError::bad("invalid file name"));
    }
    let size = r.body["size"]
        .as_i64()
        .ok_or_else(|| ApiError::bad("size must be integer bytes"))?;
    let max = core::env("MAX_UPLOAD_BYTES", "5368709120")
        .parse::<i64>()
        .unwrap_or(0)
        .min(5368709120);
    if size <= 0 || size > max {
        return Err(ApiError::bad("file size exceeds single PUT limit"));
    }
    let content_type = core::required(&r.body, "content_type")?.to_ascii_lowercase();
    let purpose = core::field(&r.body, "purpose");
    let permitted = match purpose {
        "user_avatar" | "team_avatar" => matches!(
            content_type.as_str(),
            "image/png" | "image/jpeg" | "image/webp"
        ),
        "file" => matches!(
            content_type.as_str(),
            "image/png" | "image/jpeg" | "image/webp" | "image/gif" | "text/plain"
        ),
        "import" => matches!(
            content_type.as_str(),
            "application/zip"
                | "application/x-zip-compressed"
                | "text/plain"
                | "application/octet-stream"
        ),
        _ => false,
    };
    if !permitted {
        return Err(ApiError::bad("unsupported content type for purpose"));
    }
    if purpose.ends_with("avatar") && size > 10485760 {
        return Err(ApiError::bad("avatar exceeds 10 MiB"));
    }
    if content_type.starts_with("image/") && size > 33554432 {
        return Err(ApiError::bad("image exceeds 32 MiB"));
    }
    if content_type == "text/plain" && size > 16777216 {
        return Err(ApiError::bad("text exceeds 16 MiB"));
    }
    if purpose == "import" && content_type != "text/plain" && size > 536870912 {
        return Err(ApiError::bad("compressed import exceeds 512 MiB"));
    }
    let resource = register(
        &r.state,
        &uid,
        team.as_deref(),
        &name,
        size,
        &content_type,
        purpose,
    )
    .await?;
    let sid = core::id();
    let staging_id = core::id();
    let key = format!("staging/{sid}");
    r.state.insert("resources",json!({"id":staging_id,"owner_id":uid,"key":key,"size":size,"content_type":content_type,"name":name,"purpose":"staging","state":"uploading","expires_at":core::epoch()+86400,"session_id":sid})).await?;
    let multipart = size > 33554432;
    let mut upload=r.state.insert("upload_sessions",json!({"id":sid,"owner_id":uid,"purpose":purpose,"resource_id":resource["id"],"staging_resource_id":staging_id,"key":key,"name":name,"size":size,"content_type":content_type,"project_id":r.body["project_id"],"team_id":team,"parent_id":r.body["parent_id"],"status":if multipart{"initializing"}else{"uploading"},"upload_mode":if multipart{"multipart"}else{"single"},"part_size":8388608_u64,"expires_at":core::epoch()+86400})).await?;
    if multipart {
        let multipart_id = r
            .state
            .cos
            .multipart_init(&r.state.http, &key, &content_type)
            .await?;
        upload = r
            .state
            .patch(
                "upload_sessions",
                &sid,
                json!({"multipart_id":multipart_id,"status":"uploading"}),
            )
            .await?;
        r.state
            .patch(
                "resources",
                &staging_id,
                json!({"multipart_id":upload["multipart_id"]}),
            )
            .await?;
    }
    signed_upload(r, &upload)
}
fn signed_upload(r: &Request, upload: &Value) -> Result<Value> {
    if upload["expires_at"].as_i64().unwrap_or(0) <= core::epoch()
        || core::field(upload, "status") != "uploading"
    {
        return Err(ApiError::bad("upload session expired or completed"));
    }
    let ttl = (upload["expires_at"].as_i64().unwrap() - core::epoch()).min(900) as u64;
    if core::field(upload, "upload_mode") == "multipart" {
        return Ok(
            json!({"id":upload["id"],"upload_mode":"multipart","multipart_id":upload["multipart_id"],"part_size":upload["part_size"],"expires_at":upload["expires_at"],"session_expires_at":upload["expires_at"]}),
        );
    }
    // Browsers set Content-Length from the Blob automatically; do not expose
    // that forbidden request header for JavaScript to set explicitly.
    let headers = vec![
        (
            "content-type".to_owned(),
            core::field(upload, "content_type").to_owned(),
        ),
        (
            "content-length".to_owned(),
            upload["size"].as_i64().unwrap_or(0).to_string(),
        ),
    ];
    Ok(
        json!({"id":upload["id"],"upload_mode":"single","upload_url":r.state.cos.presign("PUT",core::field(upload,"key"),ttl,&headers)?,"headers":{"Content-Type":upload["content_type"]},"expires_at":core::epoch()+ttl as i64,"session_expires_at":upload["expires_at"]}),
    )
}
fn active_multipart(upload: &Value) -> Result<()> {
    if core::field(upload, "upload_mode") != "multipart"
        || core::field(upload, "multipart_id").is_empty()
    {
        return Err(ApiError::bad("not a multipart upload"));
    }
    if core::field(upload, "status") != "uploading"
        || upload["expires_at"].as_i64().unwrap_or(0) <= core::epoch()
    {
        return Err(ApiError::bad("multipart session expired or completed"));
    }
    Ok(())
}
async fn sign_part(r: &Request, upload: &Value) -> Result<Value> {
    authorize(r, upload).await?;
    active_multipart(upload)?;
    let number = r.body["part_number"]
        .as_u64()
        .ok_or_else(|| ApiError::bad("part_number must be integer"))?;
    let size = upload["size"].as_u64().unwrap_or(0);
    let part_size = upload["part_size"].as_u64().unwrap_or(8388608);
    if number == 0 || number > size.div_ceil(part_size) {
        return Err(ApiError::bad("part number outside reserved file"));
    }
    let actual_size = (size - (number - 1) * part_size).min(part_size);
    let ttl = (upload["expires_at"].as_i64().unwrap() - core::epoch()).min(900) as u64;
    let headers = vec![("content-length".into(), actual_size.to_string())];
    let url = r.state.cos.presign_query(
        "PUT",
        core::field(upload, "key"),
        ttl,
        &headers,
        &[
            (
                "uploadId".into(),
                core::field(upload, "multipart_id").into(),
            ),
            ("partNumber".into(), number.to_string()),
        ],
    )?;
    Ok(
        json!({"upload_url":url,"headers":{},"expires_at":core::epoch()+ttl as i64,"part_number":number,"size":actual_size}),
    )
}
async fn list_parts(r: &Request, upload: &Value) -> Result<Value> {
    authorize(r, upload).await?;
    if matches!(core::field(upload, "status"), "completed" | "bound")
        || upload["multipart_completed"].as_bool() == Some(true)
    {
        return Ok(
            json!({"parts":upload["completed_parts"],"part_size":upload["part_size"],"status":upload["status"]}),
        );
    }
    active_multipart(upload)?;
    let parts = r
        .state
        .cos
        .multipart_parts(
            &r.state.http,
            core::field(upload, "key"),
            core::field(upload, "multipart_id"),
        )
        .await?
        .ok_or_else(|| ApiError::bad("multipart upload no longer exists"))?;
    Ok(json!({"parts":parts,"part_size":upload["part_size"],"status":upload["status"]}))
}
async fn finish_multipart(r: &Request, upload: &Value) -> Result<()> {
    if core::field(upload, "upload_mode") != "multipart"
        || upload["multipart_completed"].as_bool() == Some(true)
    {
        return Ok(());
    }
    let parts = r
        .state
        .cos
        .multipart_parts(
            &r.state.http,
            core::field(upload, "key"),
            core::field(upload, "multipart_id"),
        )
        .await?;
    if let Some(parts) = parts {
        let size = upload["size"].as_u64().unwrap_or(0);
        let part_size = upload["part_size"].as_u64().unwrap_or(8388608);
        if parts.len() as u64 != size.div_ceil(part_size) {
            return Err(ApiError::bad("multipart upload has missing parts"));
        }
        for (index, part) in parts.iter().enumerate() {
            let number = index as u64 + 1;
            let expected = (size - (number - 1) * part_size).min(part_size);
            if u64::from(part.part_number) != number
                || part.size != expected
                || part.etag.is_empty()
            {
                return Err(ApiError::bad(
                    "multipart part size/order does not match reservation",
                ));
            }
        }
        r.state
            .patch(
                "upload_sessions",
                core::field(upload, "id"),
                json!({"completed_parts":parts}),
            )
            .await?;
        r.state
            .cos
            .multipart_finish(
                &r.state.http,
                core::field(upload, "key"),
                core::field(upload, "multipart_id"),
                &parts,
            )
            .await?;
    }
    // Recovery of a lost CompleteMultipartUpload response is safe: clients
    // never received a plain PUT grant for this key, only this upload's parts.
    let head = r
        .state
        .cos
        .head(&r.state.http, core::field(upload, "key"))
        .await?;
    if head.size != upload["size"].as_u64().unwrap_or(0)
        || head.content_type != core::field(upload, "content_type")
    {
        return Err(ApiError::bad(
            "completed multipart object does not match reservation",
        ));
    }
    r.state
        .patch(
            "upload_sessions",
            core::field(upload, "id"),
            json!({"multipart_completed":true}),
        )
        .await?;
    Ok(())
}
async fn cancel_upload(r: &Request, upload: &Value) -> Result<Value> {
    authorize(r, upload).await?;
    let resource = r
        .state
        .get("resources", core::field(upload, "resource_id"))
        .await?;
    if resource.get("bound_file_id").is_some() || resource.get("bound_avatar_id").is_some() {
        return Err(ApiError::bad("cannot cancel an already bound upload"));
    }
    let changed=r.state.db.collection::<Document>("upload_sessions").update_one(doc!{"id":core::field(upload,"id"),"status":{"$in":["uploading","initializing","cancelled"]}},doc!{"$set":{"status":"cancelled","expires_at":core::epoch()}}).await?;
    if changed.matched_count != 1 {
        return Err(ApiError::bad("cannot cancel a completing or bound upload"));
    }
    close_session(&r.state, upload).await?;
    Ok(json!({"id":upload["id"],"status":"cancelled"}))
}
async fn close_session(state: &AppState, upload: &Value) -> Result<()> {
    if core::field(upload, "upload_mode") == "multipart" {
        let upload_id = core::field(upload, "multipart_id");
        if upload_id.is_empty() {
            state
                .cos
                .abort_key_uploads(&state.http, core::field(upload, "key"))
                .await?;
        } else {
            state
                .cos
                .multipart_abort(&state.http, core::field(upload, "key"), upload_id)
                .await?;
        }
    }
    for (rid, expires_at) in [
        (core::field(upload, "resource_id"), core::epoch()),
        (
            core::field(upload, "staging_resource_id"),
            core::epoch() + 900,
        ),
    ] {
        state
            .db
            .collection::<Document>("resources")
            .update_one(
                doc! {"id":rid,"state":"uploading"},
                doc! {"$set":{"expires_at":expires_at}},
            )
            .await?;
    }
    state
        .patch(
            "upload_sessions",
            core::field(upload, "id"),
            json!({"multipart_aborted":true,"multipart_aborted_at":core::epoch()}),
        )
        .await?;
    Ok(())
}
pub async fn expire_sessions(state: &AppState) -> Result<()> {
    // Repeat multipart abort reconciliation hourly: in-flight initiation/parts
    // may outlive the first abort. Keep locators; never TTL the session away.
    let candidates=state.find("upload_sessions",doc!{"expires_at":{"$lte":core::epoch()},"status":{"$in":["uploading","initializing","completing","cancelled","expired"]},"$or":[{"multipart_aborted":{"$ne":true}},{"upload_mode":"multipart","multipart_aborted_at":{"$lte":core::epoch()-3600}}]}).await?;
    for candidate in candidates {
        let resource = state
            .get("resources", core::field(&candidate, "resource_id"))
            .await?;
        if resource.get("completed_at").is_some()
            && !matches!(core::field(&candidate, "status"), "cancelled" | "expired")
        {
            state
                .patch(
                    "upload_sessions",
                    core::field(&candidate, "id"),
                    json!({"status":"completed"}),
                )
                .await?;
            continue;
        }
        if matches!(core::field(&resource, "state"), "copying" | "binding")
            && resource["lease_until"].as_i64().unwrap_or(0) > core::epoch()
        {
            continue;
        }
        let changed=state.db.collection::<Document>("upload_sessions").update_one(doc!{"id":core::field(&candidate,"id"),"expires_at":{"$lte":core::epoch()},"status":{"$in":["uploading","initializing","completing","cancelled","expired"]}},doc!{"$set":{"status":"expired"}}).await?;
        if changed.matched_count == 1 {
            close_session(state, &candidate).await?;
        }
    }
    Ok(())
}
async fn complete(r: &Request, upload: &Value) -> Result<Value> {
    authorize(r, upload).await?;
    if matches!(
        core::field(upload, "status"),
        "cancelled" | "expired" | "initializing"
    ) {
        return Err(ApiError::bad("upload session unavailable"));
    }
    let resource = r
        .state
        .get("resources", core::field(upload, "resource_id"))
        .await?;
    if core::field(&resource, "state") == "unbound"
        && resource["expires_at"].as_i64().unwrap_or(0) <= core::epoch()
    {
        return Err(ApiError::bad("resource expired"));
    }
    if matches!(
        core::field(&resource, "state"),
        "unbound" | "binding" | "bound"
    ) {
        return Ok(
            json!({"resource_id":resource["id"],"name":resource["name"],"size":resource["size"]}),
        );
    }
    if upload["expires_at"].as_i64().unwrap_or(0) <= core::epoch() {
        return Err(ApiError::bad("upload session expired"));
    }
    let token = core::id();
    let claimed=r.state.db.collection::<Document>("resources").update_one(doc!{"id":core::field(&resource,"id"),"$or":[{"state":"uploading"},{"state":"copying","lease_until":{"$lt":core::epoch()}}]},doc!{"$set":{"state":"copying","claim_token":&token,"lease_until":core::epoch()+1800}}).await?;
    if claimed.modified_count != 1 {
        return Err(ApiError::bad(
            "completion already running or resource deleted",
        ));
    }
    let session_claim = r
        .state
        .db
        .collection::<Document>("upload_sessions")
        .update_one(
            doc! {"id":core::field(upload,"id"),"status":{"$in":["uploading","completing"]}},
            doc! {"$set":{"status":"completing"}},
        )
        .await?;
    if session_claim.matched_count != 1 {
        r.state
            .db
            .collection::<Document>("resources")
            .update_one(
                doc! {"id":core::field(&resource,"id"),"state":"copying","claim_token":&token},
                doc! {"$set":{"state":"uploading"},"$unset":{"claim_token":"","lease_until":""}},
            )
            .await?;
        return Err(ApiError::bad("upload session was cancelled or expired"));
    }
    let outcome = async {
        finish_multipart(r, upload).await?;
        copy_validated(r, upload, &resource).await
    }
    .await;
    match outcome {
        Ok(info) => {
            let updated=r.state.db.collection::<Document>("resources").update_one(doc!{"id":core::field(&resource,"id"),"state":"copying","claim_token":&token},doc!{"$set":{"state":"unbound","etag":info.etag,"expires_at":core::epoch()+86400,"completed_at":core::now()},"$unset":{"claim_token":"","lease_until":""}}).await?;
            if updated.modified_count != 1 {
                return Err(ApiError::bad("completion claim lost"));
            }
            r.state
                .patch(
                    "upload_sessions",
                    core::field(upload, "id"),
                    json!({"status":"completed"}),
                )
                .await?;
            r.state
                .db
                .collection::<Document>("resources")
                .update_one(
                    doc! {"id":core::field(upload,"staging_resource_id"),"state":"uploading"},
                    doc! {"$set":{"state":"unbound","expires_at":core::epoch()+900}},
                )
                .await?;
            Ok(
                json!({"resource_id":resource["id"],"name":resource["name"],"size":resource["size"]}),
            )
        }
        Err(error) => {
            r.state.db.collection::<Document>("resources").update_one(doc!{"id":core::field(&resource,"id"),"claim_token":&token,"state":"copying"},doc!{"$set":{"state":"uploading"},"$unset":{"claim_token":"","lease_until":""}}).await?;
            r.state
                .db
                .collection::<Document>("upload_sessions")
                .update_one(
                    doc! {"id":core::field(upload,"id"),"status":"completing"},
                    doc! {"$set":{"status":"uploading"}},
                )
                .await?;
            Err(error)
        }
    }
}
async fn copy_validated(r: &Request, upload: &Value, resource: &Value) -> Result<ObjectInfo> {
    // A persisted snapshot prevents any retry from overwriting the formal key
    // with different staging bytes after an ambiguous network response.
    let mut snapshot = core::field(resource, "source_etag").to_owned();
    let source = r
        .state
        .cos
        .head(&r.state.http, core::field(upload, "key"))
        .await?;
    if source.size != upload["size"].as_u64().unwrap_or(0)
        || source.content_type.to_ascii_lowercase() != core::field(upload, "content_type")
        || source.etag.is_empty()
    {
        return Err(ApiError::bad(
            "uploaded size/type does not match reservation",
        ));
    }
    if snapshot.is_empty() {
        r.state
            .db
            .collection::<Document>("resources")
            .update_one(
                doc! {"id":core::field(resource,"id"),"source_etag":{"$exists":false}},
                doc! {"$set":{"source_etag":&source.etag}},
            )
            .await?;
        snapshot = core::field(
            &r.state
                .get("resources", core::field(resource, "id"))
                .await?,
            "source_etag",
        )
        .to_owned();
    }
    if snapshot != source.etag {
        return Err(ApiError::bad("staging object changed after confirmation"));
    }
    let copy_etag = r
        .state
        .cos
        .copy_snapshot(
            &r.state.http,
            core::field(upload, "key"),
            core::field(resource, "key"),
            &snapshot,
        )
        .await?;
    let formal = r
        .state
        .cos
        .head(&r.state.http, core::field(resource, "key"))
        .await?;
    if formal.size != source.size
        || formal.etag != copy_etag
        || formal.content_type != source.content_type
    {
        return Err(ApiError::bad("copied object failed snapshot validation"));
    }
    validate_content(r, upload, resource).await?;
    Ok(formal)
}
/// Validate business file/avatar bytes on a blocking thread. Imports validate
/// their ZIP envelope separately, then call this for every extracted file.
pub fn validate_bytes(_name: &str, data: &[u8], content_type: &str, purpose: &str) -> Result<()> {
    let avatar = matches!(purpose, "user_avatar" | "team_avatar");
    if avatar && data.len() > 10485760 {
        return Err(ApiError::bad("avatar exceeds 10 MiB"));
    }
    if content_type == "text/plain" && !avatar {
        if data.len() > 16777216 {
            return Err(ApiError::bad("text exceeds 16 MiB"));
        }
        std::str::from_utf8(data).map_err(|_| ApiError::bad("text must be UTF-8"))?;
        return Ok(());
    }
    let expected = match content_type {
        "image/png" => image::ImageFormat::Png,
        "image/jpeg" => image::ImageFormat::Jpeg,
        "image/webp" => image::ImageFormat::WebP,
        "image/gif" if !avatar => image::ImageFormat::Gif,
        _ => return Err(ApiError::bad("unsupported business file content type")),
    };
    if data.len() > 33554432 {
        return Err(ApiError::bad("image exceeds 32 MiB"));
    }
    let format = image::guess_format(data).map_err(|_| ApiError::bad("invalid image content"))?;
    if format != expected {
        return Err(ApiError::bad("image content does not match declared type"));
    }
    let (width, height) = image::ImageReader::with_format(std::io::Cursor::new(data), format)
        .into_dimensions()
        .map_err(|_| ApiError::bad("invalid image dimensions"))?;
    if width > 16384 || height > 16384 || u64::from(width) * u64::from(height) > 40000000 {
        return Err(ApiError::bad(
            "image dimensions or 40 million pixel limit exceeded",
        ));
    }
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(data), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(268435456);
    reader.limits(limits);
    reader
        .decode()
        .map_err(|_| ApiError::bad("invalid image or image limits exceeded"))?;
    Ok(())
}
async fn validate_content(r: &Request, upload: &Value, resource: &Value) -> Result<()> {
    let mime = core::field(upload, "content_type").to_owned();
    if mime.starts_with("image/") || mime == "text/plain" {
        static VALIDATORS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
        let permit = VALIDATORS
            .acquire()
            .await
            .map_err(|_| ApiError::internal("validation unavailable"))?;
        let declared = upload["size"]
            .as_u64()
            .ok_or_else(|| ApiError::bad("invalid upload size"))?;
        let limit = if mime == "text/plain" {
            16777216
        } else if matches!(
            core::field(upload, "purpose"),
            "user_avatar" | "team_avatar"
        ) {
            10485760
        } else {
            33554432
        };
        let bytes = r
            .state
            .cos
            .get_bounded(
                &r.state.http,
                core::field(resource, "key"),
                declared.min(limit),
            )
            .await?;
        if bytes.len() as u64 != upload["size"].as_u64().unwrap_or(0) {
            return Err(ApiError::bad("content length changed"));
        }
        let purpose = core::field(upload, "purpose").to_owned();
        let name = core::field(upload, "name").to_owned();
        return tokio::task::spawn_blocking(move || -> Result<()> {
            let _permit = permit;
            validate_bytes(&name, &bytes, &mime, &purpose)
        })
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    }
    // ZIP decoding, path and expansion limits are enforced by the import job
    // before business objects are published. Reject disguised packages here.
    let res = r
        .state
        .cos
        .send(
            &r.state.http,
            Method::GET,
            core::field(resource, "key"),
            vec![("range".into(), "bytes=0-3".into())],
            None,
        )
        .await?;
    if res.status().as_u16() != 206 {
        return Err(ApiError::internal("COS did not honor validation Range"));
    }
    let prefix = res.bytes().await?;
    if prefix.as_ref() != b"PK\x03\x04" && prefix.as_ref() != b"PK\x05\x06" {
        return Err(ApiError::bad("import package is not ZIP"));
    }
    Ok(())
}
async fn bind(r: &Request, upload: &Value) -> Result<Value> {
    authorize(r, upload).await?;
    if matches!(
        core::field(upload, "status"),
        "cancelled" | "expired" | "initializing"
    ) {
        return Err(ApiError::bad("upload session unavailable"));
    }
    if let Some(result) = upload.get("bind_result") {
        return binding_response(r, upload, core::field(result, "id")).await;
    }
    let rid = core::field(upload, "resource_id");
    let resource = r.state.get("resources", rid).await?;
    if resource.get("completed_at").is_none() {
        return Err(ApiError::bad("upload has not passed completion validation"));
    }
    let bound_target = core::field(
        &resource,
        if core::field(upload, "purpose") == "file" {
            "bound_file_id"
        } else {
            "bound_avatar_id"
        },
    );
    if !bound_target.is_empty() {
        r.state
            .patch(
                "upload_sessions",
                core::field(upload, "id"),
                json!({"status":"bound","bind_result":{"id":bound_target}}),
            )
            .await?;
        return binding_response(r, upload, bound_target).await;
    }
    if core::field(upload, "purpose") == "import" {
        if !matches!(core::field(&resource, "state"), "unbound" | "bound")
            || (core::field(&resource, "state") == "unbound"
                && resource["expires_at"].as_i64().unwrap_or(0) <= core::epoch())
        {
            return Err(ApiError::bad("import resource expired or unavailable"));
        }
        return Ok(json!({"resource_id":rid}));
    }
    let token = core::id();
    let claimed=r.state.db.collection::<Document>("upload_sessions").update_one(doc!{"id":core::field(upload,"id"),"bind_result":{"$exists":false},"$or":[{"bind_lease":{"$exists":false}},{"bind_lease":{"$lt":core::epoch()}}]},doc!{"$set":{"bind_token":&token,"bind_lease":core::epoch()+300}}).await?;
    if claimed.modified_count != 1 {
        return Err(ApiError::bad("binding already running"));
    }
    let guarded=r.state.db.collection::<Document>("resources").update_one(doc!{"id":rid,"$or":[{"state":"unbound","expires_at":{"$gt":core::epoch()}},{"state":{"$in":["binding","bound"]}}]},doc!{"$set":{"state":"binding","bind_session_id":core::field(upload,"id"),"lease_until":core::epoch()+300}}).await?;
    if guarded.matched_count != 1 {
        return Err(ApiError::bad("resource expired or deleting"));
    }
    let user = r
        .user
        .as_ref()
        .ok_or_else(|| ApiError::unauthorized("login required"))?;
    let result = match core::field(upload, "purpose") {
        "file" => {
            retry_conflicts(|| {
                crate::content::bind_file(
                    &r.state,
                    user,
                    core::field(upload, "project_id"),
                    &resource,
                    upload["parent_id"].as_str(),
                )
            })
            .await
        }
        "user_avatar" | "team_avatar" => {
            retry_conflicts(|| bind_avatar(r, upload, &resource)).await
        }
        _ => Err(ApiError::bad("invalid purpose")),
    };
    match result {
        Ok(value) => {
            r.state.db.collection::<Document>("upload_sessions").update_one(doc!{"id":core::field(upload,"id"),"bind_token":&token},doc!{"$set":{"status":"bound","bind_result":{"id":core::field(&value,"id")}},"$unset":{"bind_token":"","bind_lease":""}}).await?;
            Ok(value)
        }
        Err(error) => {
            r.state
                .db
                .collection::<Document>("upload_sessions")
                .update_one(
                    doc! {"id":core::field(upload,"id"),"bind_token":&token},
                    doc! {"$unset":{"bind_token":"","bind_lease":""}},
                )
                .await?;
            r.state
                .db
                .collection::<Document>("resources")
                .update_one(
                    doc! {"id":rid,"state":"binding"},
                    doc! {"$set":{"state":"unbound"}},
                )
                .await?;
            Err(error)
        }
    }
}
async fn binding_response(r: &Request, upload: &Value, target: &str) -> Result<Value> {
    match core::field(upload, "purpose") {
        "file" => {
            crate::content::decorate_file(&r.state, &r.state.get("files", target).await?).await
        }
        "team_avatar" => {
            crate::groups::decorate_team(
                &r.state,
                &r.state.get("teams", target).await?,
                r.user.as_ref(),
            )
            .await
        }
        "user_avatar" => avatar_user(&r.state, &r.state.get("users", target).await?).await,
        _ => Err(ApiError::bad("invalid completed binding purpose")),
    }
}
async fn bind_avatar(r: &Request, upload: &Value, resource: &Value) -> Result<Value> {
    let team = core::field(upload, "purpose") == "team_avatar";
    let collection = if team { "teams" } else { "users" };
    let target = if team {
        core::field(upload, "team_id").to_owned()
    } else {
        r.uid()?
    };
    let rid = core::field(resource, "id");
    let mut session = r.state.mongo.start_session().await?;
    session.start_transaction().await?;
    let business = r.state.db.collection::<Document>(collection);
    let current = business
        .find_one(doc! {"id":&target})
        .session(&mut session)
        .await?
        .ok_or_else(|| ApiError::not_found("avatar target missing"))?;
    if current
        .get("deleted_at")
        .is_some_and(|value| !matches!(value, mongodb::bson::Bson::Null))
    {
        session.abort_transaction().await?;
        return Err(ApiError::bad("avatar target deleted"));
    }
    let old = current
        .get_str("avatar_resource_id")
        .unwrap_or("")
        .to_owned();
    let resources = r.state.db.collection::<Document>("resources");
    let refs = r.state.db.collection::<Document>("resource_refs");
    let changed = resources
        .update_one(
            doc! {"id":rid,"state":{"$in":["binding","bound"]}},
            doc! {"$set":{"state":"bound","bound_avatar_id":&target}},
        )
        .session(&mut session)
        .await?;
    if changed.matched_count != 1 {
        session.abort_transaction().await?;
        return Err(ApiError::bad("avatar resource unavailable"));
    }
    let ref_id = format!("{rid}:avatar:{target}");
    refs.update_one(doc!{"id":&ref_id},doc!{"$setOnInsert":{"id":ref_id,"resource_id":rid,"kind":"avatar","target_id":&target,"created_at":core::now()}}).upsert(true).session(&mut session).await?;
    business
        .update_one(
            doc! {"id":&target},
            doc! {"$set":{"avatar_resource_id":rid,"has_avatar":true,"updated_at":core::now()}},
        )
        .session(&mut session)
        .await?;
    if !old.is_empty() && old != rid {
        refs.delete_one(doc! {"id":format!("{old}:avatar:{target}")})
            .session(&mut session)
            .await?;
        if refs
            .count_documents(doc! {"resource_id":&old})
            .session(&mut session)
            .await?
            == 0
        {
            resources.update_one(doc!{"id":&old,"state":"bound"},doc!{"$set":{"state":"unbound","expires_at":RetirementPolicy::RecoveryGrace.expires_at()}}).session(&mut session).await?;
        }
    }
    session.commit_transaction().await?;
    let updated = r.state.get(collection, &target).await?;
    if team {
        crate::groups::decorate_team(&r.state, &updated, r.user.as_ref()).await
    } else {
        avatar_user(&r.state, &updated).await
    }
}
async fn avatar_user(state: &AppState, user: &Value) -> Result<Value> {
    let mut public = core::public_user(user);
    let rid = core::field(user, "avatar_resource_id");
    if !rid.is_empty() {
        let resource = state.get("resources", rid).await?;
        if core::field(&resource, "state") == "bound" {
            public["avatar"] =
                json!(state
                    .cos
                    .presign("GET", core::field(&resource, "key"), 300, &[])?);
        }
    }
    Ok(public)
}
async fn download(r: &Request, rid: &str) -> Result<Value> {
    r.uid()?;
    let resource = r.state.get("resources", rid).await?;
    if !matches!(core::field(&resource, "state"), "bound" | "unbound")
        || (core::field(&resource, "state") == "unbound"
            && resource["expires_at"].as_i64().unwrap_or(0) <= core::epoch())
    {
        return Err(ApiError::not_found("resource unavailable"));
    }
    let refs = r
        .state
        .find("resource_refs", doc! {"resource_id":rid})
        .await?;
    let mut allowed = false;
    if core::field(&resource, "state") == "unbound"
        && core::field(&resource, "owner_id") == r.uid()?
    {
        let sessions = r
            .state
            .find("upload_sessions", doc! {"resource_id":rid})
            .await?;
        if let Some(session) = sessions.first() {
            authorize(r, session).await?;
        } else {
            let team = core::field(&resource, "team_id");
            if !team.is_empty() {
                crate::groups::require(r, "team", team, 1).await?;
            }
        }
        allowed = true;
    }
    for reference in refs {
        let target = core::field(&reference, "target_id");
        match core::field(&reference, "kind") {
            "file" => {
                let file = r.state.get("files", target).await?;
                if file.get("deleted_at").is_none_or(Value::is_null)
                    && !file["blocked"].as_bool().unwrap_or(false)
                    && crate::groups::require(r, "project", core::field(&file, "project_id"), 1)
                        .await
                        .is_ok()
                {
                    allowed = true;
                }
            }
            "avatar" => {
                if core::field(&resource, "purpose") == "user_avatar" {
                    allowed = true;
                } else if crate::groups::require(r, "team", target, 1).await.is_ok() {
                    allowed = true;
                }
            }
            "output" => {
                let output = r.state.get("outputs", target).await?;
                if output["expires_at"].as_i64().unwrap_or(i64::MAX) > core::epoch()
                    && crate::groups::require(r, "project", core::field(&output, "project_id"), 1)
                        .await
                        .is_ok()
                {
                    allowed = true;
                }
            }
            _ => {}
        }
    }
    if !allowed {
        return Err(ApiError::forbidden("resource access denied"));
    }
    let headers = vec![];
    Ok(
        json!({"url":r.state.cos.presign("GET",core::field(&resource,"key"),300,&headers)?,"expires_at":core::epoch()+300}),
    )
}
/// Reconcile interrupted commits only after the promised resource lifetime.
/// GC must invoke this before atomically claiming ordinary expiry candidates.
pub async fn recover_expired(state: &AppState) -> Result<()> {
    // Image replacement commits a new pointer before retiring the old ref.
    // Recover that durable intent even when its caller never retries.
    let mut references = state
        .db
        .collection::<Document>("resource_refs")
        .find(doc! {"kind":"file"})
        .await?;
    while let Some(reference) = references.try_next().await? {
        let rid = reference.get_str("resource_id").unwrap_or("");
        let target = reference.get_str("target_id").unwrap_or("");
        let mut session = state.mongo.start_session().await?;
        session.start_transaction().await?;
        let file = state
            .db
            .collection::<Document>("files")
            .find_one(doc! {"id":target})
            .session(&mut session)
            .await?;
        if let Some(file) = file {
            let current = file.get_str("resource_id").unwrap_or("");
            if current != rid {
                let active = state
                    .db
                    .collection::<Document>("resources")
                    .count_documents(
                        doc! {"id":{"$in":[rid,current]},"lease_until":{"$gt":core::epoch()}},
                    )
                    .session(&mut session)
                    .await?;
                if active == 0 {
                    let refs = state.db.collection::<Document>("resource_refs");
                    refs.delete_one(doc! {"id":reference.get_str("id").unwrap_or("")})
                        .session(&mut session)
                        .await?;
                    if refs
                        .count_documents(doc! {"resource_id":rid})
                        .session(&mut session)
                        .await?
                        == 0
                    {
                        state.db.collection::<Document>("resources").update_one(doc!{"id":rid,"state":"bound"},doc!{"$set":{"state":"unbound","expires_at":RetirementPolicy::RecoveryGrace.expires_at()}}).session(&mut session).await?;
                    }
                }
            }
        }
        session.commit_transaction().await?;
    }
    let candidates=state.find("resources",doc!{"state":{"$in":["copying","binding"]},"lease_until":{"$lte":core::epoch()},"expires_at":{"$lte":core::epoch()}}).await?;
    for candidate in candidates {
        let rid = core::field(&candidate, "id");
        let mut session = state.mongo.start_session().await?;
        session.start_transaction().await?;
        let refs = state.db.collection::<Document>("resource_refs");
        for (collection, field, kind) in [
            ("files", "resource_id", "file"),
            ("users", "avatar_resource_id", "avatar"),
            ("teams", "avatar_resource_id", "avatar"),
            ("outputs", "resource_id", "output"),
        ] {
            let mut filter = Document::new();
            filter.insert(field, rid);
            if let Some(business) = state
                .db
                .collection::<Document>(collection)
                .find_one(filter)
                .session(&mut session)
                .await?
            {
                if let Ok(target) = business.get_str("id") {
                    let ref_id = format!("{rid}:{kind}:{target}");
                    refs.update_one(doc!{"id":&ref_id},doc!{"$setOnInsert":{"id":ref_id,"resource_id":rid,"kind":kind,"target_id":target,"created_at":core::now()}}).upsert(true).session(&mut session).await?;
                }
            }
        }
        let referenced = refs
            .count_documents(doc! {"resource_id":rid})
            .session(&mut session)
            .await?
            > 0;
        state.db.collection::<Document>("resources").update_one(doc!{"id":rid,"state":core::field(&candidate,"state"),"lease_until":{"$lte":core::epoch()},"expires_at":{"$lte":core::epoch()}},doc!{"$set":{"state":if referenced{"bound"}else{"unbound"}},"$unset":{"lease_until":"","claim_token":""}}).session(&mut session).await?;
        session.commit_transaction().await?;
    }
    Ok(())
}
pub async fn handle(r: &Request) -> Result<Option<Reply>> {
    let p = &r.path;
    let method = r.method.as_str();
    if p.first().map(String::as_str) == Some("uploads") {
        r.uid()?;
        let value = if p.len() == 1 && method == "POST" {
            request_upload(r).await?
        } else if p.len() == 3 && matches!(method, "POST" | "GET") {
            let upload = r.state.get("upload_sessions", &p[1]).await?;
            if core::field(&upload, "owner_id") != r.uid()? {
                return Err(ApiError::forbidden("upload belongs to another user"));
            }
            match (method, p[2].as_str()) {
                ("POST", "complete") => complete(r, &upload).await?,
                ("POST", "bind") => bind(r, &upload).await?,
                ("POST", "sign") => {
                    authorize(r, &upload).await?;
                    signed_upload(r, &upload)?
                }
                ("POST", "parts") => sign_part(r, &upload).await?,
                ("GET", "parts") => list_parts(r, &upload).await?,
                ("POST", "cancel") => cancel_upload(r, &upload).await?,
                _ => return Err(ApiError::not_found("unknown upload operation")),
            }
        } else {
            return Err(ApiError::bad("unsupported upload operation"));
        };
        return Ok(Some(value.into()));
    }
    if p.first().map(String::as_str) == Some("resources") {
        if p.len() == 3 && p[2] == "download" && matches!(method, "POST" | "GET") {
            return Ok(Some(download(r, &p[1]).await?.into()));
        }
        return Err(ApiError::bad("unsupported resource operation"));
    }
    Ok(None)
}

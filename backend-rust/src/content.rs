use crate::core::{
    epoch, field, id, now, public_user, required, ApiError, AppState, Reply, Request, Result,
};
use axum::http::Method;
use mongodb::bson::{doc, Document};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Arc;

fn live(v: &Value) -> bool {
    v.get("deleted_at").is_none_or(Value::is_null)
}
fn active(v: &Value) -> bool {
    live(v) && v["activated"].as_bool().unwrap_or(true)
}
fn number(v: &Value, k: &str) -> i64 {
    v[k].as_i64().unwrap_or(0)
}
fn name(v: &Value) -> Result<String> {
    let s = required(v, "name")?;
    if s.len() > 255
        || s == "."
        || s == ".."
        || s.chars()
            .any(|c| c.is_control() || "/\\:*?\"<>|".contains(c))
    {
        return Err(ApiError::bad("Invalid file name"));
    }
    Ok(s)
}
fn text(v: &Value, k: &str, empty: bool) -> Result<String> {
    let s = v[k]
        .as_str()
        .ok_or_else(|| ApiError::bad(format!("{k} must be a string")))?;
    if s.len() > 1_000_000 || (!empty && s.trim().is_empty()) {
        return Err(ApiError::bad(format!(
            "{k} must contain text and be at most 1000000 bytes"
        )));
    }
    Ok(s.to_owned())
}
fn coordinates(body: &Value, out: &mut Value) -> Result<()> {
    for key in ["x", "y"] {
        if let Some(v) = body.get(key) {
            let n = v
                .as_f64()
                .ok_or_else(|| ApiError::bad("Coordinates must be numbers"))?;
            if !n.is_finite() || !(0.0..=1.0).contains(&n) {
                return Err(ApiError::bad("Coordinates must be between 0 and 1"));
            }
            out[key] = json!(n);
        }
    }
    if let Some(v) = body.get("position_type") {
        let n = v
            .as_i64()
            .ok_or_else(|| ApiError::bad("position_type must be an integer"))?;
        if !(1..=2).contains(&n) {
            return Err(ApiError::bad("Invalid position_type"));
        }
        out["position_type"] = json!(n);
    }
    if let Some(v) = body.get("vertices") {
        let a = v
            .as_array()
            .ok_or_else(|| ApiError::bad("vertices must be coordinate pairs"))?;
        if a.len() < 6
            || a.len() > 200
            || a.len() % 2 != 0
            || a.iter().any(|v| {
                v.as_f64()
                    .is_none_or(|n| !n.is_finite() || !(0.0..=1.0).contains(&n))
            })
        {
            return Err(ApiError::bad(
                "vertices must contain 3 to 100 normalized coordinate pairs",
            ));
        }
        out["vertices"] = v.clone();
    }
    Ok(())
}
async fn project(r: &Request, pid: &str, permission: i64) -> Result<Value> {
    crate::groups::require(r, "project", pid, permission).await
}
async fn file_access(r: &Request, fid: &str, permission: i64, historical: bool) -> Result<Value> {
    let f = r.state.get("files", fid).await?;
    project(r, field(&f, "project_id"), permission).await?;
    if !live(&f) || (!historical && !active(&f)) {
        return Err(ApiError::not_found(
            "File is recycled or an inactive revision",
        ));
    }
    if permission != 1 && number(&f, "safe_status") == 5 {
        return Err(ApiError::forbidden("File is blocked by moderation"));
    }
    Ok(f)
}
async fn source_access(r: &Request, sid: &str, permission: i64) -> Result<(Value, Value)> {
    let s = r.state.get("sources", sid).await?;
    if !live(&s) {
        return Err(ApiError::not_found("Source deleted"));
    }
    let f = file_access(r, field(&s, "file_id"), permission, permission == 1).await?;
    Ok((s, f))
}
async fn parent_check(state: &AppState, pid: &str, parent: Option<&str>) -> Result<()> {
    if let Some(p) = parent {
        let f = state.get("files", p).await?;
        if field(&f, "project_id") != pid || number(&f, "type") != 1 || !active(&f) {
            return Err(ApiError::bad(
                "Parent must be an active folder in this project",
            ));
        }
    }
    Ok(())
}
async fn siblings(state: &AppState, f: &Value) -> Result<Vec<Value>> {
    let mut values = state.find("files",doc! {"project_id":field(f,"project_id"),"parent_id":mongodb::bson::to_bson(&f["parent_id"]).map_err(|e|ApiError::internal(e.to_string()))?}).await?;
    values.retain(active);
    values.sort_by(|a, b| {
        field(a, "name")
            .cmp(field(b, "name"))
            .then_with(|| field(a, "id").cmp(field(b, "id")))
    });
    Ok(values)
}
async fn sources_for(state: &AppState, fid: &str) -> Result<Vec<Value>> {
    let mut s = state.find("sources", doc! {"file_id":fid}).await?;
    s.retain(live);
    s.sort_by(|a, b| {
        number(a, "rank")
            .cmp(&number(b, "rank"))
            .then_with(|| field(a, "id").cmp(field(b, "id")))
    });
    Ok(s)
}
async fn counts(state: &AppState, files: &[Value], target: &str) -> Result<Value> {
    if files.is_empty() {
        return Ok(json!({"source_count":0,"translated_source_count":0,"checked_source_count":0}));
    }
    let file_ids: Vec<_> = files.iter().map(|f| field(f, "id")).collect();
    let text_files: HashSet<_> = files
        .iter()
        .filter(|f| number(f, "type") == 3)
        .map(|f| field(f, "id"))
        .collect();
    let sources = state
        .find("sources", doc! {"file_id":{"$in":file_ids}})
        .await?;
    let source_ids: HashSet<String> = sources
        .into_iter()
        .filter(|s| {
            live(s)
                && (!text_files.contains(field(s, "file_id"))
                    || !field(s, "content").trim().is_empty())
        })
        .map(|s| field(&s, "id").to_owned())
        .collect();
    let targets: HashSet<String> = state
        .find("targets", doc! {"project_id":field(&files[0],"project_id")})
        .await?
        .into_iter()
        .filter(live)
        .map(|t| field(&t, "id").to_owned())
        .collect();
    let mut translated = HashSet::new();
    let mut checked = HashSet::new();
    if !source_ids.is_empty() {
        let filter_ids: Vec<_> = source_ids.iter().map(String::as_str).collect();
        for t in state
            .find("translations", doc! {"source_id":{"$in":filter_ids}})
            .await?
        {
            if live(&t)
                && targets.contains(field(&t, "target_id"))
                && (target.is_empty() || field(&t, "target_id") == target)
                && (!field(&t, "content").is_empty() || !field(&t, "proofread_content").is_empty())
            {
                translated.insert(field(&t, "source_id").to_owned());
                if t["selected"].as_bool() == Some(true) {
                    checked.insert(field(&t, "source_id").to_owned());
                }
            }
        }
    }
    Ok(
        json!({"source_count":source_ids.len(),"translated_source_count":translated.len(),"checked_source_count":checked.len()}),
    )
}
fn tree_members(all: Vec<Value>, root: &Value) -> Vec<Value> {
    let mut ids = HashSet::from([field(root, "id").to_owned()]);
    let mut families = HashSet::from([field(root, "revision_family_id").to_owned()]);
    loop {
        let before = ids.len();
        for f in &all {
            if ids.contains(field(f, "parent_id"))
                || (!field(f, "revision_family_id").is_empty()
                    && families.contains(field(f, "revision_family_id")))
            {
                ids.insert(field(f, "id").to_owned());
                families.insert(field(f, "revision_family_id").to_owned());
            }
        }
        if before == ids.len() {
            break;
        }
    }
    all.into_iter()
        .filter(|f| ids.contains(field(f, "id")))
        .collect()
}
async fn descendants(state: &AppState, root: &Value) -> Result<Vec<Value>> {
    Ok(tree_members(
        state
            .find("files", doc! {"project_id":field(root,"project_id")})
            .await?,
        root,
    ))
}
async fn find_in_session(
    state: &AppState,
    collection: &str,
    filter: Document,
    session: &mut mongodb::ClientSession,
) -> Result<Vec<Value>> {
    let mut cursor = state
        .db
        .collection::<Document>(collection)
        .find(filter)
        .session(&mut *session)
        .await?;
    let mut values = Vec::new();
    while let Some(document) = cursor.next(&mut *session).await {
        values.push(crate::core::to_value(document?)?);
    }
    Ok(values)
}
async fn descendants_in_session(
    state: &AppState,
    root: &Value,
    session: &mut mongodb::ClientSession,
) -> Result<Vec<Value>> {
    Ok(tree_members(
        find_in_session(
            state,
            "files",
            doc! {"project_id":field(root,"project_id")},
            session,
        )
        .await?,
        root,
    ))
}
async fn file_metadata(state: &AppState, f: &Value, target: &str) -> Result<Value> {
    let mut out = f.clone();
    let files = if number(f, "type") == 1 {
        descendants(state, f)
            .await?
            .into_iter()
            .filter(active)
            .filter(|v| number(v, "type") != 1)
            .collect()
    } else {
        vec![f.clone()]
    };
    let c = counts(state, &files, target).await?;
    for k in [
        "source_count",
        "translated_source_count",
        "checked_source_count",
    ] {
        out[k] = c[k].clone();
    }
    out["file_target_cache"] = c;
    out["create_time"] = f["created_at"].clone();
    out["edit_time"] = f.get("updated_at").unwrap_or(&f["created_at"]).clone();
    out["parse_status"] = json!(f["parse_status"]
        .as_i64()
        .unwrap_or(if number(f, "type") == 3 { 0 } else { 4 }));
    out["parse_status_detail_name"] = json!(match number(&out, "parse_status") {
        1 => "Queued",
        2 => "Parsing",
        3 => "Failed",
        4 => "Succeeded",
        _ => "Not started",
    });
    out["parse_error_type_detail_name"] = f.get("parse_error").cloned().unwrap_or(json!(""));
    out["image_ocr_percent"] = f.get("image_ocr_percent").cloned().unwrap_or(json!(0));
    out["image_ocr_percent_detail_name"] = json!("");
    out["safe_status"] = json!(f["safe_status"].as_i64().unwrap_or(3));
    out["file_not_exist_reason"] = json!(if number(f, "safe_status") == 5 {
        4
    } else if field(f, "resource_id").is_empty() && number(f, "type") != 1 {
        1
    } else {
        0
    });
    out["save_name"] = json!("");
    out["url"] = json!("");
    out["cover_url"] = json!("");
    out["safe_check_url"] = json!("");
    if !field(f, "resource_id").is_empty() && number(f, "safe_status") != 5 {
        let resource = state.get("resources", field(f, "resource_id")).await?;
        if matches!(field(&resource, "state"), "deleted" | "deleting") {
            out["file_not_exist_reason"] = json!(1);
        } else {
            let url = state
                .cos
                .presign("GET", field(&resource, "key"), 300, &[])?;
            out["url"] = json!(url);
            out["cover_url"] = out["url"].clone();
            out["size"] = resource["size"].clone();
        }
    }
    out.as_object_mut().unwrap().remove("resource_key");
    Ok(out)
}
pub async fn decorate_file(state: &AppState, file: &Value) -> Result<Value> {
    file_metadata(state, file, "").await
}
async fn lock_project_for_file(
    state: &AppState,
    pid: &str,
    parent: Option<&str>,
    session: &mut mongodb::ClientSession,
) -> Result<()> {
    let changed = state
        .db
        .collection::<Document>("projects")
        .update_one(
            doc! {"id":pid,"deleted_at":null,"status":0},
            doc! {"$inc":{"metadata_version":1_i64}},
        )
        .session(&mut *session)
        .await?;
    if changed.matched_count != 1 {
        return Err(ApiError::forbidden("Project is recycled or read-only"));
    }
    if let Some(parent) = parent {
        let changed = state
            .db
            .collection::<Document>("files")
            .update_one(
                doc! {"id":parent,"project_id":pid,"type":1,"activated":true,"deleted_at":null},
                doc! {"$inc":{"metadata_version":1_i64}},
            )
            .session(&mut *session)
            .await?;
        if changed.matched_count != 1 {
            return Err(ApiError::bad("Parent folder is no longer active"));
        }
    }
    Ok(())
}

pub async fn bind_file(
    state: &AppState,
    user: &Value,
    project_id: &str,
    resource: &Value,
    parent_id: Option<&str>,
) -> Result<Value> {
    let req = Request {
        state: Arc::new(AppState {
            db: state.db.clone(),
            mongo: state.mongo.clone(),
            http: state.http.clone(),
            cos: state.cos.clone(),
        }),
        user: Some(user.clone()),
        method: Method::POST,
        path: vec![],
        query: Default::default(),
        body: json!({}),
    };
    project(&req, project_id, 1020).await?;
    let rid = required(resource, "id")?;
    if field(resource, "owner_id") != field(user, "id") {
        return Err(ApiError::forbidden("Resource belongs to another user"));
    }
    let current_resource = state.get("resources", &rid).await?;
    if !field(&current_resource, "bound_file_id").is_empty() {
        let existing = state
            .get("files", field(&current_resource, "bound_file_id"))
            .await?;
        if field(&existing, "project_id") != project_id {
            return Err(ApiError::bad("Resource already bound to another project"));
        }
        if number(&existing, "type") == 3 && number(&existing, "parse_status") != 4 {
            let fid = field(&existing, "id");
            crate::jobs::enqueue(state,"text_parse",json!({"file_id":fid,"resource_id":rid,"project_id":project_id,"owner_id":field(user,"id")}),Some(&format!("text_parse:{fid}"))).await?;
        } else if !field(&existing, "previous_resource_id").is_empty() {
            crate::storage::retire(
                state,
                field(&existing, "previous_resource_id"),
                "file",
                field(&existing, "id"),
                crate::storage::RetirementPolicy::RecoveryGrace,
            )
            .await?;
        }
        return decorate_file(state, &existing).await;
    }
    parent_check(state, project_id, parent_id).await?;
    if let Some(existing) = state
        .find("files", doc! {"resource_id":&rid})
        .await?
        .into_iter()
        .next()
    {
        if field(&existing, "project_id") != project_id || existing["parent_id"] != json!(parent_id)
        {
            return Err(ApiError::bad("Resource already bound elsewhere"));
        }
        if number(&existing, "type") == 3 && number(&existing, "parse_status") != 4 {
            let fid = field(&existing, "id");
            crate::jobs::enqueue(state,"text_parse",json!({"file_id":fid,"resource_id":rid,"project_id":project_id,"owner_id":field(user,"id")}),Some(&format!("text_parse:{fid}"))).await?;
        } else if !field(&existing, "previous_resource_id").is_empty() {
            crate::storage::retire(
                state,
                field(&existing, "previous_resource_id"),
                "file",
                field(&existing, "id"),
                crate::storage::RetirementPolicy::RecoveryGrace,
            )
            .await?;
        }
        return decorate_file(state, &existing).await;
    }
    if !matches!(field(resource, "state"), "binding" | "bound" | "unbound") {
        return Err(ApiError::bad("Resource unavailable"));
    }
    let filename = name(resource)?;
    let mime = field(resource, "content_type");
    let lower = filename.to_lowercase();
    let kind = if mime.starts_with("image/") {
        2
    } else if mime.starts_with("text/")
        || lower.ends_with(".txt")
        || lower.ends_with(".md")
        || lower.ends_with(".srt")
    {
        3
    } else {
        return Err(ApiError::bad(
            "Only images and UTF-8 text files can be bound",
        ));
    };
    let old=state.find("files",doc!{"project_id":project_id,"parent_id":parent_id,"name_key":filename.to_lowercase(),"activated":true,"deleted_at":null}).await?.into_iter().next();
    if old.as_ref().is_some_and(|f| number(f, "type") != kind) {
        return Err(ApiError::bad("File name already used by another type"));
    }
    if kind == 2 {
        if let Some(prior) = &old {
            let fid = field(prior, "id");
            let mut session = state.mongo.start_session().await?;
            session.start_transaction().await?;
            lock_project_for_file(state, project_id, parent_id, &mut session).await?;
            crate::storage::attach_in_session(state, &rid, "file", fid, &mut session).await?;
            let changed=state.db.collection::<Document>("files").update_one(doc!{"id":fid,"resource_id":field(prior,"resource_id"),"name_key":filename.to_lowercase(),"parent_id":parent_id,"activated":true,"deleted_at":null},doc!{"$set":{"resource_id":&rid,"size":number(resource,"size"),"safe_status":3,"revision":number(prior,"revision")+1,"previous_resource_id":field(prior,"resource_id"),"updated_at":now()}}).session(&mut session).await?;
            if changed.matched_count != 1 {
                return Err(ApiError::bad("File changed concurrently; retry binding"));
            }
            state.db.collection::<Document>("jobs").update_many(doc!{"kind":"ai","payload.file_id":fid,"payload.resource_id":{"$ne":&rid},"status":{"$in":["preparing","queued","running"]}},doc!{"$set":{"status":"cancelled","cancel_reason":"resource_replaced","finished_epoch":epoch(),"updated_at":now()}}).session(&mut session).await?;
            session.commit_transaction().await?;
            let replaced = state.get("files", fid).await?;
            if !field(prior, "resource_id").is_empty() {
                crate::storage::retire(
                    state,
                    field(prior, "resource_id"),
                    "file",
                    fid,
                    crate::storage::RetirementPolicy::RecoveryGrace,
                )
                .await?;
            }
            return decorate_file(state, &replaced).await;
        }
    }
    let fid = rid.clone();
    let family = old
        .as_ref()
        .map(|f| field(f, "revision_family_id").to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| fid.clone());
    let revision = state
        .find("files", doc! {"revision_family_id":&family})
        .await?
        .iter()
        .map(|f| number(f, "revision"))
        .max()
        .unwrap_or(0)
        + 1;
    let previous_revision_id = if kind == 3 {
        old.as_ref().map(|prior| field(prior, "id"))
    } else {
        None
    };
    let value = json!({"id":fid,"project_id":project_id,"parent_id":parent_id,"name":filename,"name_key":filename.to_lowercase(),"type":kind,"resource_id":rid,"size":resource["size"],"revision":revision,"revision_family_id":family,"previous_revision_id":previous_revision_id,"activated":true,"deleted_at":null,"safe_status":3,"parse_status":if kind==3{1}else{4},"created_at":now(),"owner_id":field(user,"id")});
    let mut session = state.mongo.start_session().await?;
    session.start_transaction().await?;
    lock_project_for_file(state, project_id, parent_id, &mut session).await?;
    crate::storage::attach_in_session(state, &rid, "file", &fid, &mut session).await?;
    let coll = state.db.collection::<Document>("files");
    if let Some(prior) = &old {
        let changed=coll.update_one(doc!{"id":field(prior,"id"),"name_key":filename.to_lowercase(),"parent_id":parent_id,"activated":true,"deleted_at":null},doc!{"$set":{"activated":false,"updated_at":now()}}).session(&mut session).await?;
        if changed.matched_count != 1 {
            return Err(ApiError::bad(
                "Prior revision was recycled, moved, or changed concurrently",
            ));
        }
    }
    let document =
        mongodb::bson::to_document(&value).map_err(|e| ApiError::internal(e.to_string()))?;
    coll.insert_one(document).session(&mut session).await?;
    session.commit_transaction().await?;
    // Text revision resources remain referenced so history never destroys translations.
    if kind == 3 {
        crate::jobs::enqueue(state,"text_parse",json!({"file_id":fid,"resource_id":rid,"project_id":project_id,"owner_id":field(user,"id")}),Some(&format!("text_parse:{fid}"))).await?;
    }
    decorate_file(state, &value).await
}
async fn user_value(state: &AppState, uid: &str) -> Result<Value> {
    if uid.is_empty() {
        return Ok(Value::Null);
    }
    let user = match state.get("users", uid).await {
        Ok(v) => v,
        Err(e) if e.status == axum::http::StatusCode::NOT_FOUND => return Ok(Value::Null),
        Err(e) => return Err(e),
    };
    let mut public = public_user(&user);
    if let Some(object) = public.as_object_mut() {
        object.remove("email");
    }
    public["avatar_url"] = json!("");
    public["avatar"] = json!("");
    public["create_time"] = user["created_at"].clone();
    if !field(&user, "avatar_resource_id").is_empty() {
        let resource = state
            .get("resources", field(&user, "avatar_resource_id"))
            .await?;
        if !matches!(field(&resource, "state"), "deleted" | "deleting") {
            let url = state
                .cos
                .presign("GET", field(&resource, "key"), 300, &[])?;
            public["avatar_url"] = json!(url);
            public["avatar"] = public["avatar_url"].clone();
        }
    }
    Ok(public)
}
async fn target_value(state: &AppState, t: &Value) -> Result<Value> {
    let mut o = t.clone();
    o["language"] = state.get("languages", field(t, "language_id")).await?;
    o["create_time"] = t["created_at"].clone();
    o["edit_time"] = t.get("updated_at").unwrap_or(&t["created_at"]).clone();
    Ok(o)
}
async fn translation_value(state: &AppState, t: &Value) -> Result<Value> {
    let mut o = t.clone();
    for (k, uid) in [
        ("user", "user_id"),
        ("proofreader", "proofreader_id"),
        ("selector", "selector_id"),
    ] {
        o[k] = user_value(state, field(t, uid)).await?;
    }
    o["target"] = target_value(state, &state.get("targets", field(t, "target_id")).await?).await?;
    o["create_time"] = t["created_at"].clone();
    o["edit_time"] = t.get("updated_at").unwrap_or(&t["created_at"]).clone();
    Ok(o)
}
async fn tip_value(state: &AppState, t: &Value) -> Result<Value> {
    let mut o = t.clone();
    o["user"] = user_value(state, field(t, "user_id")).await?;
    o["create_time"] = t["created_at"].clone();
    o["edit_time"] = t.get("updated_at").unwrap_or(&t["created_at"]).clone();
    Ok(o)
}
async fn source_value(r: &Request, s: &Value, target: &str) -> Result<Value> {
    let mut o = s.clone();
    let ts = r
        .state
        .find("translations", doc! {"source_id":field(s,"id")})
        .await?;
    let target_ids: Vec<_> = ts.iter().map(|t| field(t, "target_id")).collect();
    let live_targets: HashSet<String> = r
        .state
        .find("targets", doc! {"id":{"$in":target_ids},"deleted_at":null})
        .await?
        .into_iter()
        .map(|t| field(&t, "id").to_owned())
        .collect();
    o["has_other_language_translation"] = json!(ts.iter().any(|t| live(t)
        && live_targets.contains(field(t, "target_id"))
        && field(t, "target_id") != target));
    let mut filtered: Vec<_> = ts
        .into_iter()
        .filter(|t| {
            live(t)
                && live_targets.contains(field(t, "target_id"))
                && (target.is_empty() || field(t, "target_id") == target)
        })
        .collect();
    filtered.sort_by(|a, b| {
        b["selected"]
            .as_bool()
            .cmp(&a["selected"].as_bool())
            .then_with(|| field(b, "updated_at").cmp(field(a, "updated_at")))
            .then_with(|| field(a, "id").cmp(field(b, "id")))
    });
    let mut other = Vec::new();
    o["my_translation"] = Value::Null;
    for t in filtered {
        let v = translation_value(&r.state, &t).await?;
        if r.user
            .as_ref()
            .is_some_and(|u| field(u, "id") == field(&t, "user_id"))
            && !t["mt"].as_bool().unwrap_or(false)
        {
            o["my_translation"] = v;
        } else {
            other.push(v);
        }
    }
    o["translations"] = json!(other);
    let mut tips = Vec::new();
    for t in r
        .state
        .find("tips", doc! {"source_id":field(s,"id")})
        .await?
    {
        if live(&t)
            && (field(&t, "target_id").is_empty()
                || target.is_empty()
                || field(&t, "target_id") == target)
        {
            tips.push(tip_value(&r.state, &t).await?);
        }
    }
    o["tips"] = json!(tips);
    Ok(o)
}
async fn target_check(r: &Request, tid: &str, pid: &str) -> Result<Value> {
    let t = r.state.get("targets", tid).await?;
    if field(&t, "project_id") != pid || !live(&t) {
        return Err(ApiError::bad("Target does not belong to project"));
    }
    Ok(t)
}
async fn select(r: &Request, translations: &[(Value, bool)]) -> Result<()> {
    let uid = r.uid()?;
    let mut session = r.state.mongo.start_session().await?;
    session.start_transaction().await?;
    let coll = r.state.db.collection::<Document>("translations");
    for (t, selected) in translations {
        if *selected {
            coll.update_many(
                doc! {"source_id":field(t,"source_id"),"target_id":field(t,"target_id")},
                doc! {"$set":{"selected":false,"selector_id":null,"updated_at":now()}},
            )
            .session(&mut session)
            .await?;
        }
        coll.update_one(doc!{"id":field(t,"id")},doc!{"$set":{"selected":*selected,"selector_id":if *selected{Some(uid.as_str())}else{None},"updated_at":now()}}).session(&mut session).await?;
    }
    session.commit_transaction().await?;
    Ok(())
}
fn response(v: Value) -> Result<Option<Reply>> {
    Ok(Some(v.into()))
}
fn unsupported() -> Result<Option<Reply>> {
    Err(ApiError::bad("Unsupported method or content operation"))
}

pub async fn handle(r: &Request) -> Result<Option<Reply>> {
    let p: Vec<&str> = r.path.iter().map(String::as_str).collect();
    let state = &r.state;
    match p.as_slice() {
        ["projects", pid, "files"] => {
            project(r, pid, if r.method == Method::GET { 1 } else { 1020 }).await?;
            if r.method == Method::GET {
                let mut result = Vec::new();
                let recycle = r.q("recycled") == "true";
                for f in state.find("files", doc! {"project_id":*pid}).await? {
                    if (if recycle { !live(&f) } else { active(&f) })
                        && (r.q("word").is_empty()
                            || field(&f, "name")
                                .to_lowercase()
                                .contains(&r.q("word").to_lowercase()))
                        && (r.q("parent_id").is_empty()
                            || if r.q("parent_id") == "root" {
                                f["parent_id"].is_null()
                            } else {
                                field(&f, "parent_id") == r.q("parent_id")
                            })
                    {
                        result.push(file_metadata(state, &f, r.q("target")).await?);
                    }
                }
                result.sort_by(|a, b| field(a, "name").cmp(field(b, "name")));
                return Ok(Some(Reply::list(result, r)));
            }
            if r.method == Method::POST {
                if r.body.get("type").is_some_and(|v| v.as_i64() != Some(1)) {
                    return Err(ApiError::bad("Use direct upload binding for files"));
                }
                let filename = name(&r.body)?;
                let parent = r.body["parent_id"].as_str().filter(|s| *s != "root");
                parent_check(state, pid, parent).await?;
                if !state.find("files",doc!{"project_id":*pid,"parent_id":parent,"name_key":filename.to_lowercase(),"activated":true,"deleted_at":null}).await?.is_empty(){return Err(ApiError::bad("Name already exists"));}
                let fid = id();
                let f = json!({"id":fid,"project_id":pid,"parent_id":parent,"name":filename,"name_key":filename.to_lowercase(),"type":1,"activated":true,"revision":1,"revision_family_id":fid,"deleted_at":null,"safe_status":4,"parse_status":4,"created_at":now()});
                let mut session = state.mongo.start_session().await?;
                session.start_transaction().await?;
                lock_project_for_file(state, pid, parent, &mut session).await?;
                state
                    .db
                    .collection::<Document>("files")
                    .insert_one(mongodb::bson::to_document(&f)?)
                    .session(&mut session)
                    .await?;
                session.commit_transaction().await?;
                return response(decorate_file(state, &f).await?);
            }
            unsupported()
        }
        ["files", fid] => {
            let f = file_access(
                r,
                fid,
                if r.method == Method::GET {
                    1
                } else if r.method == Method::DELETE {
                    1050
                } else if r.body.get("parent_id").is_some() {
                    1030
                } else {
                    1040
                },
                r.method == Method::GET,
            )
            .await?;
            if r.method == Method::GET {
                let mut o = file_metadata(state, &f, r.q("target")).await?;
                if number(&f, "type") == 2 {
                    let images: Vec<_> = siblings(state, &f)
                        .await?
                        .into_iter()
                        .filter(|v| number(v, "type") == 2)
                        .collect();
                    if let Some(pos) = images.iter().position(|v| field(v, "id") == *fid) {
                        o["prev_image"] = if pos > 0 {
                            file_metadata(state, &images[pos - 1], r.q("target")).await?
                        } else {
                            Value::Null
                        };
                        o["next_image"] = if pos + 1 < images.len() {
                            file_metadata(state, &images[pos + 1], r.q("target")).await?
                        } else {
                            Value::Null
                        };
                    }
                }
                return response(o);
            }
            if r.method == Method::DELETE {
                let deleted_at = epoch();
                let mut session = state.mongo.start_session().await?;
                session.start_transaction().await?;
                lock_project_for_file(state, field(&f, "project_id"), None, &mut session).await?;
                let root = field(&f, "id");
                let current = find_in_session(
                    state,
                    "files",
                    doc! {"id":root,"activated":true,"deleted_at":null},
                    &mut session,
                )
                .await?
                .into_iter()
                .next()
                .ok_or_else(|| ApiError::bad("File changed or was recycled concurrently"))?;
                let children = descendants_in_session(state, &current, &mut session).await?;
                {
                    let file_ids: Vec<_> = children
                        .iter()
                        .filter(|child| live(child))
                        .map(|child| field(child, "id"))
                        .collect();
                    state.db.collection::<Document>("jobs").update_many(doc!{"payload.file_id":{"$in":file_ids},"status":{"$in":["preparing","queued","running"]}},doc!{"$set":{"status":"cancelled","cancel_reason":"file_recycled","finished_epoch":deleted_at,"updated_at":now()}}).session(&mut session).await?;
                }
                for child in children {
                    if live(&child) {
                        let changed=state.db.collection::<Document>("files").update_one(doc!{"id":field(&child,"id"),"deleted_at":null},doc!{"$set":{"deleted_at":deleted_at,"deletion_root_type":"file","deletion_root_id":root,"updated_at":now()}}).session(&mut session).await?;
                        if changed.matched_count != 1 {
                            return Err(ApiError::bad("Descendant changed during recycling"));
                        }
                    }
                }
                session.commit_transaction().await?;
                return response(json!({"id":fid,"deleted_at":deleted_at}));
            }
            if r.method == Method::PUT || r.method == Method::PATCH {
                let mut update = json!({});
                if r.body.get("name").is_some() {
                    project(r, field(&f, "project_id"), 1040).await?;
                    let filename = name(&r.body)?;
                    update["name_key"] = json!(filename.to_lowercase());
                    update["name"] = json!(filename);
                }
                if let Some(parent) = r.body.get("parent_id") {
                    project(r, field(&f, "project_id"), 1030).await?;
                    if !parent.is_null() && !parent.is_string() {
                        return Err(ApiError::bad("parent_id must be a string or null"));
                    }
                    let parent = parent.as_str().filter(|s| *s != "root");
                    if parent.is_some_and(|s| s == *fid) {
                        return Err(ApiError::bad("Cannot move a folder into itself"));
                    }
                    update["parent_id"] = json!(parent);
                }
                if update.as_object().unwrap().is_empty() {
                    return Err(ApiError::bad("Provide name or parent_id"));
                }
                let mut session = state.mongo.start_session().await?;
                session.start_transaction().await?;
                lock_project_for_file(state, field(&f, "project_id"), None, &mut session).await?;
                let f = find_in_session(
                    state,
                    "files",
                    doc! {"id":*fid,"activated":true,"deleted_at":null},
                    &mut session,
                )
                .await?
                .into_iter()
                .next()
                .ok_or_else(|| ApiError::bad("File is no longer active"))?;
                let new_name = update.get("name").unwrap_or(&f["name"]);
                let new_parent = update.get("parent_id").unwrap_or(&f["parent_id"]);
                lock_project_for_file(
                    state,
                    field(&f, "project_id"),
                    new_parent.as_str(),
                    &mut session,
                )
                .await?;
                if let Some(parent) = new_parent.as_str() {
                    if descendants_in_session(state, &f, &mut session)
                        .await?
                        .iter()
                        .any(|v| field(v, "id") == parent)
                    {
                        return Err(ApiError::bad("Cannot move folder into descendant"));
                    }
                }
                for s in find_in_session(state,"files",doc!{"project_id":field(&f,"project_id"),"name_key":new_name.as_str().unwrap_or("").to_lowercase(),"parent_id":mongodb::bson::to_bson(new_parent)?},&mut session).await?{if active(&s)&&field(&s,"id")!=*fid{return Err(ApiError::bad("Name already exists in destination"));}}
                // Apply path/name to the entire revision family so future uploads continue that family.
                update["updated_at"] = json!(now());
                state
                    .db
                    .collection::<Document>("files")
                    .update_many(
                        doc! {"revision_family_id":field(&f,"revision_family_id")},
                        doc! {"$set":mongodb::bson::to_document(&update)?},
                    )
                    .session(&mut session)
                    .await?;
                session.commit_transaction().await?;
                return response(decorate_file(state, &state.get("files", fid).await?).await?);
            }
            unsupported()
        }
        ["files", fid, "history"] => {
            if r.method != Method::GET {
                return unsupported();
            }
            let f = file_access(r, fid, 1, true).await?;
            let mut revisions = state
                .find(
                    "files",
                    doc! {"revision_family_id":field(&f,"revision_family_id")},
                )
                .await?;
            revisions.sort_by_key(|v| std::cmp::Reverse(number(v, "revision")));
            let mut out = Vec::new();
            for v in revisions {
                out.push(decorate_file(state, &v).await?);
            }
            Ok(Some(Reply::list(out, r)))
        }
        ["files", fid, "activate"] => {
            if r.method != Method::POST {
                return unsupported();
            }
            let f = file_access(r, fid, 1020, true).await?;
            let mut session = state.mongo.start_session().await?;
            session.start_transaction().await?;
            lock_project_for_file(
                state,
                field(&f, "project_id"),
                f["parent_id"].as_str(),
                &mut session,
            )
            .await?;
            let coll = state.db.collection::<Document>("files");
            coll.update_many(
                doc! {"revision_family_id":field(&f,"revision_family_id")},
                doc! {"$set":{"activated":false}},
            )
            .session(&mut session)
            .await?;
            let changed = coll
                .update_one(
                    doc! {"id":*fid,"deleted_at":null},
                    doc! {"$set":{"activated":true,"updated_at":now()}},
                )
                .session(&mut session)
                .await?;
            if changed.matched_count != 1 {
                return Err(ApiError::bad("Revision was recycled concurrently"));
            }
            session.commit_transaction().await?;
            response(decorate_file(state, &state.get("files", fid).await?).await?)
        }
        ["files", fid, "restore"] => {
            if r.method != Method::POST {
                return unsupported();
            }
            let f = state.get("files", fid).await?;
            project(r, field(&f, "project_id"), 1050).await?;
            if live(&f) {
                return response(decorate_file(state, &f).await?);
            }
            if number(&f, "deleted_at") < epoch() - 604800 {
                return Err(ApiError::bad("Recycle retention expired"));
            }
            if field(&f, "deletion_root_type") != "file" || field(&f, "deletion_root_id") != *fid {
                return Err(ApiError::bad("Restore the recycled ancestor first"));
            }
            let mut session = state.mongo.start_session().await?;
            session.start_transaction().await?;
            lock_project_for_file(
                state,
                field(&f, "project_id"),
                f["parent_id"].as_str(),
                &mut session,
            )
            .await?;
            for sibling in find_in_session(state,"files",doc!{"project_id":field(&f,"project_id"),"parent_id":mongodb::bson::to_bson(&f["parent_id"])?},&mut session).await?{if active(&sibling)&&field(&sibling,"name_key")==field(&f,"name_key"){return Err(ApiError::bad("Restore destination name occupied"));}}
            let restore: Vec<_> = descendants_in_session(state, &f, &mut session)
                .await?
                .into_iter()
                .filter(|child| {
                    field(child, "deletion_root_type") == "file"
                        && field(child, "deletion_root_id") == *fid
                })
                .collect();
            let root_changed=state.db.collection::<Document>("files").update_one(doc!{"id":*fid,"deleted_at":{"$gte":epoch()-604800},"deletion_state":{"$ne":"deleting"},"deletion_root_type":"file","deletion_root_id":*fid},doc!{"$set":{"deleted_at":null,"deletion_state":null,"deletion_root_type":null,"deletion_root_id":null,"updated_at":now()}}).session(&mut session).await?;
            if root_changed.matched_count != 1 {
                return Err(ApiError::bad(
                    "Recycle root expired or deletion has started",
                ));
            }
            for child in restore {
                if field(&child, "id") != *fid {
                    let changed=state.db.collection::<Document>("files").update_one(doc!{"id":field(&child,"id"),"deletion_state":{"$ne":"deleting"},"deletion_root_type":"file","deletion_root_id":*fid},doc!{"$set":{"deleted_at":null,"deletion_state":null,"deletion_root_type":null,"deletion_root_id":null,"updated_at":now()}}).session(&mut session).await?;
                    if changed.matched_count != 1 {
                        return Err(ApiError::bad("A descendant is already being deleted"));
                    }
                }
                if !field(&child, "resource_id").is_empty() {
                    crate::storage::attach_in_session(
                        state,
                        field(&child, "resource_id"),
                        "file",
                        field(&child, "id"),
                        &mut session,
                    )
                    .await?;
                }
            }
            session.commit_transaction().await?;
            response(decorate_file(state, &state.get("files", fid).await?).await?)
        }
        ["projects", pid, "targets"] => {
            project(r, pid, if r.method == Method::GET { 1 } else { 1150 }).await?;
            if r.method == Method::GET {
                let files: Vec<_> = state
                    .find("files", doc! {"project_id":*pid})
                    .await?
                    .into_iter()
                    .filter(active)
                    .filter(|f| number(f, "type") != 1)
                    .collect();
                let mut out = Vec::new();
                for t in state.find("targets", doc! {"project_id":*pid}).await? {
                    if live(&t) {
                        let mut o = target_value(state, &t).await?;
                        if !r.q("word").is_empty()
                            && !o["language"]
                                .to_string()
                                .to_lowercase()
                                .contains(&r.q("word").to_lowercase())
                        {
                            continue;
                        }
                        let c = counts(state, &files, field(&t, "id")).await?;
                        o["translated_source_count"] = c["translated_source_count"].clone();
                        o["checked_source_count"] = c["checked_source_count"].clone();
                        out.push(o);
                    }
                }
                return Ok(Some(Reply::list(out, r)));
            }
            if r.method == Method::POST {
                let language = r
                    .body
                    .get("language_id")
                    .or_else(|| r.body.get("language"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| ApiError::bad("language is required"))?;
                state.get("languages", language).await?;
                let mut session = state.mongo.start_session().await?;
                session.start_transaction().await?;
                lock_project_for_file(state, pid, None, &mut session).await?;
                let mut existing = find_in_session(
                    state,
                    "targets",
                    doc! {"project_id":*pid,"language_id":language},
                    &mut session,
                )
                .await?;
                if existing.iter().any(live) {
                    return Err(ApiError::bad("Target language already exists"));
                }
                existing.sort_by_key(|t| std::cmp::Reverse(number(t, "deleted_at")));
                let recent = existing.into_iter().find(|t| {
                    number(t, "deleted_at") > epoch() - 604800
                        && field(t, "deletion_root_type") == "target"
                        && field(t, "deletion_root_id") == field(t, "id")
                        && field(t, "deletion_state") != "deleting"
                });
                let t = if let Some(mut t) = recent {
                    let changed=state.db.collection::<Document>("targets").update_one(doc!{"id":field(&t,"id"),"deleted_at":{"$gt":epoch()-604800},"deletion_root_type":"target","deletion_root_id":field(&t,"id"),"deletion_state":{"$ne":"deleting"}},doc!{"$set":{"deleted_at":null,"deletion_root_type":null,"deletion_root_id":null,"deletion_state":null,"updated_at":now()}}).session(&mut session).await?;
                    if changed.matched_count != 1 {
                        return Err(ApiError::bad("Target is expired or deletion has started"));
                    }
                    t["deleted_at"] = Value::Null;
                    t["deletion_root_type"] = Value::Null;
                    t["deletion_root_id"] = Value::Null;
                    t["deletion_state"] = Value::Null;
                    t["updated_at"] = json!(now());
                    t
                } else {
                    let t = json!({"id":id(),"project_id":pid,"language_id":language,"intro":"","deleted_at":null,"created_at":now()});
                    state
                        .db
                        .collection::<Document>("targets")
                        .insert_one(mongodb::bson::to_document(&t)?)
                        .session(&mut session)
                        .await?;
                    t
                };
                session.commit_transaction().await?;
                return response(target_value(state, &t).await?);
            }
            unsupported()
        }
        ["targets", tid] => {
            let t = state.get("targets", tid).await?;
            project(
                r,
                field(&t, "project_id"),
                if r.method == Method::GET {
                    1
                } else if r.method == Method::DELETE {
                    1170
                } else {
                    1160
                },
            )
            .await?;
            if r.method == Method::GET {
                if !live(&t) {
                    return Err(ApiError::not_found("Target recycled"));
                }
                return response(target_value(state, &t).await?);
            }
            if r.method == Method::DELETE {
                let mut session = state.mongo.start_session().await?;
                session.start_transaction().await?;
                lock_project_for_file(state, field(&t, "project_id"), None, &mut session).await?;
                let changed=state.db.collection::<Document>("targets").update_one(doc!{"id":*tid,"deleted_at":null,"deletion_state":{"$ne":"deleting"}},doc!{"$set":{"deleted_at":epoch(),"deletion_root_type":"target","deletion_root_id":*tid,"updated_at":now()}}).session(&mut session).await?;
                if changed.matched_count != 1 {
                    return Err(ApiError::bad("Target is already recycled or deleting"));
                }
                state.db.collection::<Document>("jobs").update_many(doc!{"payload.target_id":*tid,"status":{"$in":["preparing","queued","running"]}},doc!{"$set":{"status":"cancelled","cancel_reason":"target_recycled","finished_epoch":epoch(),"updated_at":now()}}).session(&mut session).await?;
                session.commit_transaction().await?;
                return response(json!({"id":tid}));
            }
            if r.method == Method::PUT {
                let intro = text(&r.body, "intro", true)?;
                let mut session = state.mongo.start_session().await?;
                session.start_transaction().await?;
                lock_project_for_file(state, field(&t, "project_id"), None, &mut session).await?;
                let changed = state
                    .db
                    .collection::<Document>("targets")
                    .update_one(
                        doc! {"id":*tid,"deleted_at":null},
                        doc! {"$set":{"intro":intro,"updated_at":now()}},
                    )
                    .session(&mut session)
                    .await?;
                if changed.matched_count != 1 {
                    return Err(ApiError::not_found("Target recycled"));
                }
                session.commit_transaction().await?;
                return response(target_value(state, &state.get("targets", tid).await?).await?);
            }
            unsupported()
        }
        ["files", fid, "sources"] => {
            let permission = if r.method == Method::GET {
                1
            } else if r.method == Method::PATCH {
                1140
            } else {
                1080
            };
            let f = file_access(r, fid, permission, r.method == Method::GET).await?;
            let target = if !r.q("target_id").is_empty() {
                r.q("target_id")
            } else {
                r.q("target")
            };
            if !target.is_empty() {
                target_check(r, target, field(&f, "project_id")).await?;
            }
            if r.method == Method::GET {
                let mut out = Vec::new();
                for s in sources_for(state, fid).await? {
                    out.push(source_value(r, &s, target).await?);
                }
                return Ok(Some(Reply::list(out, r)));
            }
            if r.method == Method::POST {
                if number(&f, "type") != 2 {
                    return Err(ApiError::bad("Only images support manual source labels"));
                }
                let mut value = json!({"file_id":fid,"content":"","x":0.5,"y":0.5,"rank":sources_for(state,fid).await?.iter().map(|s|number(s,"rank")).max().unwrap_or(0)+1,"position_type":1,"machine":false});
                coordinates(&r.body, &mut value)?;
                if r.body.get("content").is_some() {
                    value["content"] = json!(text(&r.body, "content", true)?);
                }
                let s = state.insert("sources", value).await?;
                return response(source_value(r, &s, target).await?);
            }
            if r.method == Method::PATCH {
                let items = r
                    .body
                    .as_array()
                    .ok_or_else(|| ApiError::bad("Batch selection must be an array"))?;
                if items.len() > 10000 {
                    return Err(ApiError::bad("Too many selections"));
                }
                let mut selections = Vec::new();
                let mut seen = HashSet::new();
                for item in items {
                    let sid = required(item, "source_id")?;
                    let tid = required(item, "translation_id")?;
                    let (s, _) = source_access(r, &sid, 1140).await?;
                    if field(&s, "file_id") != *fid {
                        return Err(ApiError::bad("Source is from another file"));
                    }
                    let t = state.get("translations", &tid).await?;
                    if field(&t, "source_id") != sid || !live(&t) {
                        return Err(ApiError::bad("Translation does not belong to source"));
                    }
                    target_check(r, field(&t, "target_id"), field(&f, "project_id")).await?;
                    if !seen.insert((sid, field(&t, "target_id").to_owned())) {
                        return Err(ApiError::bad("Duplicate source/target in batch"));
                    }
                    selections.push((t, true));
                }
                select(r, &selections).await?;
                let mut out = Vec::new();
                for (t, _) in selections {
                    let s = state.get("sources", field(&t, "source_id")).await?;
                    out.push(source_value(r, &s, field(&t, "target_id")).await?);
                }
                return response(json!(out));
            }
            unsupported()
        }
        ["sources", sid, "rank"] => {
            if r.method != Method::PUT {
                return unsupported();
            }
            let (s, f) = source_access(r, sid, 1080).await?;
            if number(&f, "type") != 2 {
                return Err(ApiError::bad("Only image labels can be reordered"));
            }
            let next = required(&r.body, "next_source_id")?;
            let mut list = sources_for(state, field(&s, "file_id")).await?;
            if next == *sid {
                return response(s);
            }
            list.retain(|v| field(v, "id") != *sid);
            let pos = if next == "end" {
                list.len()
            } else {
                list.iter()
                    .position(|v| field(v, "id") == next)
                    .ok_or_else(|| ApiError::bad("Next source must belong to same file"))?
            };
            list.insert(pos, s);
            let mut session = state.mongo.start_session().await?;
            session.start_transaction().await?;
            for (index, item) in list.iter().enumerate() {
                state
                    .db
                    .collection::<Document>("sources")
                    .update_one(
                        doc! {"id":field(item,"id")},
                        doc! {"$set":{"rank":(index+1) as i64,"updated_at":now()}},
                    )
                    .session(&mut session)
                    .await?;
            }
            session.commit_transaction().await?;
            response(state.get("sources", sid).await?)
        }
        ["sources", sid] => {
            let (s, f) = source_access(
                r,
                sid,
                if r.method == Method::GET {
                    1
                } else if r.method == Method::DELETE {
                    1100
                } else {
                    1090
                },
            )
            .await?;
            if r.method == Method::GET {
                return response(source_value(r, &s, r.q("target_id")).await?);
            }
            if number(&f, "type") != 2 {
                return Err(ApiError::bad(
                    "Text sources are immutable; upload a new revision",
                ));
            }
            if r.method == Method::DELETE {
                state
                    .patch("sources", sid, json!({"deleted_at":epoch()}))
                    .await?;
                return response(json!({"id":sid}));
            }
            if r.method == Method::PUT {
                let mut update = json!({});
                coordinates(&r.body, &mut update)?;
                if r.body.get("content").is_some() {
                    update["content"] = json!(text(&r.body, "content", true)?);
                }
                if update.as_object().unwrap().is_empty() {
                    return Err(ApiError::bad("No editable source fields"));
                }
                let s = state.patch("sources", sid, update).await?;
                return response(source_value(r, &s, r.q("target_id")).await?);
            }
            unsupported()
        }
        ["sources", sid, "translations"] => {
            let (s, f) =
                source_access(r, sid, if r.method == Method::GET { 1 } else { 1110 }).await?;
            if r.method == Method::GET {
                let mut out = Vec::new();
                for t in state.find("translations", doc! {"source_id":*sid}).await? {
                    if live(&t) {
                        out.push(translation_value(state, &t).await?);
                    }
                }
                return Ok(Some(Reply::list(out, r)));
            }
            if r.method == Method::POST {
                let uid = r.uid()?;
                let tid = required(&r.body, "target_id")?;
                target_check(r, &tid, field(&f, "project_id")).await?;
                let content = text(&r.body, "content", true)?;
                let existing = state
                    .find(
                        "translations",
                        doc! {"source_id":field(&s,"id"),"target_id":&tid,"user_id":&uid},
                    )
                    .await?
                    .into_iter()
                    .next();
                let t = if let Some(t) = existing {
                    if content.is_empty() && field(&t, "proofread_content").is_empty() {
                        state.remove("translations", field(&t, "id")).await?;
                        return response(Value::Null);
                    }
                    state
                        .patch(
                            "translations",
                            field(&t, "id"),
                            json!({"content":content,"deleted_at":null}),
                        )
                        .await?
                } else {
                    if content.is_empty() {
                        return response(Value::Null);
                    }
                    state.insert("translations",json!({"source_id":sid,"target_id":tid,"user_id":uid,"content":content,"proofread_content":"","selected":false,"mt":false})).await?
                };
                return response(translation_value(state, &t).await?);
            }
            unsupported()
        }
        ["translations", tid] => {
            let t = state.get("translations", tid).await?;
            if !live(&t) {
                return Err(ApiError::not_found("Translation deleted"));
            }
            let permission = if r.method == Method::GET {
                1
            } else if r.method == Method::DELETE
                && r.user
                    .as_ref()
                    .is_none_or(|u| field(u, "id") != field(&t, "user_id"))
            {
                1120
            } else if r.body.get("content").is_some() {
                1110
            } else if r.body.get("proofread_content").is_some() {
                1130
            } else if r.body.get("selected").is_some() {
                1140
            } else {
                1110
            };
            let (_, f) = source_access(r, field(&t, "source_id"), permission).await?;
            target_check(r, field(&t, "target_id"), field(&f, "project_id")).await?;
            if r.method == Method::GET {
                return response(translation_value(state, &t).await?);
            }
            let uid = r.uid()?;
            if r.method == Method::DELETE {
                state.remove("translations", tid).await?;
                return response(json!({"id":tid}));
            }
            if r.method == Method::PUT || r.method == Method::PATCH {
                let mut update = json!({});
                if r.body.get("content").is_some() {
                    if field(&t, "user_id") != uid {
                        return Err(ApiError::forbidden(
                            "Only the author can edit translation content",
                        ));
                    }
                    update["content"] = json!(text(&r.body, "content", true)?);
                }
                if r.body.get("proofread_content").is_some() {
                    project(r, field(&f, "project_id"), 1130).await?;
                    update["proofread_content"] = json!(text(&r.body, "proofread_content", true)?);
                    update["proofreader_id"] = json!(uid);
                }
                let selection = if let Some(selected) = r.body.get("selected") {
                    project(r, field(&f, "project_id"), 1140).await?;
                    Some(
                        selected
                            .as_bool()
                            .ok_or_else(|| ApiError::bad("selected must be boolean"))?,
                    )
                } else {
                    None
                };
                if update.as_object().unwrap().is_empty() && selection.is_none() {
                    return Err(ApiError::bad("No editable translation fields"));
                }
                let t = if update.as_object().unwrap().is_empty() {
                    t
                } else {
                    state.patch("translations", tid, update).await?
                };
                if field(&t, "content").is_empty() && field(&t, "proofread_content").is_empty() {
                    state.remove("translations", tid).await?;
                    return response(Value::Null);
                }
                if let Some(selected) = selection {
                    select(r, &[(t, selected)]).await?;
                }
                return response(
                    translation_value(state, &state.get("translations", tid).await?).await?,
                );
            }
            unsupported()
        }
        ["sources", sid, "tips"] => {
            let (_, f) =
                source_access(r, sid, if r.method == Method::GET { 1 } else { 1110 }).await?;
            if r.method == Method::GET {
                let mut out = Vec::new();
                for t in state.find("tips", doc! {"source_id":*sid}).await? {
                    if live(&t) {
                        out.push(tip_value(state, &t).await?);
                    }
                }
                return Ok(Some(Reply::list(out, r)));
            }
            if r.method == Method::POST {
                let target = field(&r.body, "target_id");
                if !target.is_empty() {
                    target_check(r, target, field(&f, "project_id")).await?;
                }
                let t=state.insert("tips",json!({"source_id":sid,"user_id":r.uid()?,"target_id":target,"content":text(&r.body,"content",false)?})).await?;
                return response(tip_value(state, &t).await?);
            }
            unsupported()
        }
        ["tips", tid] => {
            let t = state.get("tips", tid).await?;
            let (_, f) = source_access(
                r,
                field(&t, "source_id"),
                if r.method == Method::GET { 1 } else { 1110 },
            )
            .await?;
            if r.method == Method::GET {
                return response(tip_value(state, &t).await?);
            }
            if field(&t, "user_id") != r.uid()? {
                project(r, field(&f, "project_id"), 1120).await?;
            }
            if r.method == Method::DELETE {
                state.remove("tips", tid).await?;
                return response(json!({"id":tid}));
            }
            if r.method == Method::PUT {
                let t = state
                    .patch(
                        "tips",
                        tid,
                        json!({"content":text(&r.body,"content",false)?}),
                    )
                    .await?;
                return response(tip_value(state, &t).await?);
            }
            unsupported()
        }
        ["admin", "files"] => {
            if !r.is_admin() {
                return Err(ApiError::forbidden("Administrator required"));
            }
            if r.method != Method::GET {
                return unsupported();
            }
            let filter = r.q("safe_status");
            let statuses: Vec<i64> = filter.split(',').filter_map(|s| s.parse().ok()).collect();
            let mut out = Vec::new();
            for f in state.find("files", doc! {}).await? {
                if number(&f, "type") != 1
                    && (statuses.is_empty() || statuses.contains(&number(&f, "safe_status")))
                {
                    let mut v = decorate_file(state, &f).await?;
                    if !field(&f, "resource_id").is_empty() {
                        let resource = state.get("resources", field(&f, "resource_id")).await?;
                        if !matches!(field(&resource, "state"), "deleted" | "deleting") {
                            v["safe_check_url"] = json!(state.cos.presign(
                                "GET",
                                field(&resource, "key"),
                                300,
                                &[]
                            )?);
                        }
                    }
                    out.push(v);
                }
            }
            Ok(Some(Reply::list(out, r)))
        }
        ["admin", "files", "safe-status"] => {
            if !r.is_admin() {
                return Err(ApiError::forbidden("Administrator required"));
            }
            if r.method != Method::PUT {
                return unsupported();
            }
            let mut operations = Vec::new();
            let mut seen = HashSet::new();
            for (key, status) in [("safe_files", 4), ("unsafe_files", 5)] {
                let items = r.body[key]
                    .as_array()
                    .ok_or_else(|| ApiError::bad("safe_files and unsafe_files must be arrays"))?;
                for value in items {
                    let fid = value
                        .as_str()
                        .ok_or_else(|| ApiError::bad("File IDs must be strings"))?;
                    if !seen.insert(fid.to_owned()) {
                        return Err(ApiError::bad("Duplicate moderation file"));
                    }
                    state.get("files", fid).await?;
                    operations.push((fid.to_owned(), status));
                }
            }
            for (fid, status) in operations {
                state
                    .patch(
                        "files",
                        &fid,
                        json!({"safe_status":status,"moderator_id":r.uid()?,"moderated_at":now()}),
                    )
                    .await?;
            }
            response(json!({"updated":seen.len()}))
        }
        _ => Ok(None),
    }
}

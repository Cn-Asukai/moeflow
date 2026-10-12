use crate::core::{self, ApiError, AppState, Reply, Request, Result};
use axum::http::Method;
use mongodb::bson::{doc, Document};
use serde_json::{json, Value};
use std::collections::HashSet;

fn collection(kind: &str) -> Result<&'static str> {
    match kind {
        "team" => Ok("teams"),
        "project" => Ok("projects"),
        _ => Err(ApiError::bad("Invalid group type")),
    }
}
fn live(v: &Value) -> bool {
    v.get("deleted_at").map_or(true, Value::is_null)
}
fn permission_name(n: i64, kind: &str) -> &'static str {
    match n {
        1 => "Access",
        5 => "Delete",
        10 => "Settings",
        15 => "Create roles",
        20 => "Delete roles",
        101 => "Review applications",
        105 => "Invite members",
        110 => "Remove members",
        115 => "Change roles",
        120 => "Change remarks",
        1010 if kind == "team" => "Inherit project administration",
        1010 => "Finish/resume",
        1020 if kind == "team" => "Create term bank",
        1030 if kind == "team" => "Read term bank",
        1040 if kind == "team" => "Edit term bank",
        1050 if kind == "team" => "Delete term bank",
        1060 if kind == "team" => "Create term",
        1070 if kind == "team" => "Edit term",
        1080 if kind == "team" => "Delete term",
        1090 if kind == "team" => "Create project",
        1100 if kind == "team" => "Create project set",
        1110 if kind == "team" => "Edit project set",
        1120 if kind == "team" => "Delete project set",
        1130 if kind == "team" => "Use OCR",
        1140 if kind == "team" => "Use translation quota",
        1150 if kind == "team" => "Insight",
        1020 => "Add file",
        1030 => "Move file",
        1040 => "Rename file",
        1050 => "Delete file",
        1060 => "Export",
        1080 => "Add source",
        1090 => "Move source",
        1100 => "Delete source",
        1110 => "Translate",
        1120 => "Delete translation",
        1130 => "Proofread",
        1140 => "Select translation",
        1150 => "Add target",
        1160 => "Change target",
        1170 => "Delete target",
        _ => "Permission",
    }
}
fn codes(kind: &str) -> Vec<i64> {
    let mut p = vec![1, 5, 10, 15, 20, 101, 105, 110, 115, 120];
    p.extend(if kind == "team" {
        (1010..=1150).step_by(10).collect::<Vec<_>>()
    } else {
        vec![
            1010, 1020, 1030, 1040, 1050, 1060, 1080, 1090, 1100, 1110, 1120, 1130, 1140, 1150,
            1160, 1170,
        ]
    });
    p
}
fn role_value(kind: &str, code: &str, level: i64, p: Vec<i64>) -> Value {
    json!({"id":format!("{kind}-{code}"),"name":code,"system":true,"system_code":code,"level":level,"permissions":p.into_iter().map(|id|json!({"id":id,"name":permission_name(id,kind),"intro":""})).collect::<Vec<_>>()})
}
fn presets(kind: &str) -> Vec<Value> {
    let all = codes(kind);
    let mut out = vec![
        role_value(kind, "creator", 500, all.clone()),
        role_value(
            kind,
            "admin",
            400,
            all.into_iter()
                .filter(|n| kind == "project" || *n != 5)
                .collect(),
        ),
    ];
    if kind == "team" {
        out.push(role_value(
            kind,
            "senior",
            300,
            vec![
                1, 1020, 1030, 1040, 1050, 1060, 1070, 1080, 1090, 1100, 1110, 1120, 1130, 1140,
            ],
        ));
        out.push(role_value(
            kind,
            "member",
            200,
            vec![
                1, 1020, 1030, 1040, 1050, 1060, 1070, 1080, 1090, 1100, 1110, 1120,
            ],
        ));
        out.push(role_value(kind, "beginner", 100, vec![1]));
    } else {
        out.push(role_value(
            kind,
            "coordinator",
            300,
            vec![
                1, 1020, 1030, 1040, 1050, 1060, 1080, 1090, 1100, 1110, 1120, 1130, 1140, 101,
                105, 110, 115, 120,
            ],
        ));
        out.push(role_value(
            kind,
            "proofreader",
            200,
            vec![1, 1060, 1080, 1090, 1100, 1110, 1130, 1140],
        ));
        out.push(role_value(
            kind,
            "translator",
            200,
            vec![1, 1060, 1080, 1090, 1100, 1110],
        ));
        out.push(role_value(
            kind,
            "picture_editor",
            200,
            vec![1, 1060, 1080, 1090, 1100],
        ));
        out.push(role_value(kind, "supporter", 100, vec![1, 1110]));
    }
    out
}
fn has(role: &Value, p: i64) -> bool {
    role["permissions"].as_array().map_or(false, |a| {
        a.iter()
            .any(|x| x.as_i64() == Some(p) || x["id"].as_i64() == Some(p))
    })
}
fn level(role: &Value) -> i64 {
    role["level"].as_i64().unwrap_or(0)
}
async fn resolve_role(s: &AppState, kind: &str, gid: &str, rid: &str) -> Result<Value> {
    if let Some(v) = presets(kind)
        .into_iter()
        .find(|v| core::field(v, "id") == rid)
    {
        return Ok(v);
    }
    let v = s.get("roles", rid).await?;
    if core::field(&v, "group_id") != gid || core::field(&v, "group_type") != kind {
        return Err(ApiError::bad("Role belongs to another group"));
    }
    Ok(v)
}
async fn membership(s: &AppState, kind: &str, gid: &str, uid: &str) -> Result<Option<Value>> {
    Ok(s.find(
        "memberships",
        doc! {"group_type":kind,"group_id":gid,"user_id":uid},
    )
    .await?
    .into_iter()
    .next())
}
async fn user_role(s: &AppState, kind: &str, g: &Value, user: Option<&Value>) -> Result<Value> {
    let Some(u) = user else {
        return Ok(Value::Null);
    };
    if u["admin"].as_bool() == Some(true) || core::field(g, "owner_id") == core::field(u, "id") {
        return Ok(presets(kind)[0].clone());
    }
    let mut role =
        if let Some(m) = membership(s, kind, core::field(g, "id"), core::field(u, "id")).await? {
            resolve_role(s, kind, core::field(g, "id"), core::field(&m, "role_id")).await?
        } else {
            Value::Null
        };
    if kind == "project" {
        let t = s.get("teams", core::field(g, "team_id")).await?;
        if live(&t) {
            let tr = if core::field(&t, "owner_id") == core::field(u, "id") {
                presets("team")[0].clone()
            } else if let Some(m) =
                membership(s, "team", core::field(&t, "id"), core::field(u, "id")).await?
            {
                resolve_role(s, "team", core::field(&t, "id"), core::field(&m, "role_id")).await?
            } else {
                Value::Null
            };
            if has(&tr, 1010) && level(&role) < 400 {
                role = presets("project")[1].clone();
            }
        }
    }
    Ok(role)
}
pub async fn require_user(
    state: &AppState,
    user: &Value,
    kind: &str,
    gid: &str,
    permission: i64,
) -> Result<Value> {
    if core::field(user, "id").is_empty() {
        return Err(ApiError::unauthorized("Authentication required"));
    }
    if user["disabled"].as_bool() == Some(true) {
        return Err(ApiError::forbidden("User is disabled"));
    }
    let g = state.get(collection(kind)?, gid).await?;
    if !live(&g) {
        return Err(ApiError::not_found("Group is recycled"));
    }
    if kind == "project" {
        let team = state.get("teams", core::field(&g, "team_id")).await?;
        if !live(&team) {
            return Err(ApiError::not_found("Team is recycled"));
        }
        if let Some(sid) = g["project_set_id"].as_str().filter(|s| !s.is_empty()) {
            if !live(&state.get("project_sets", sid).await?) {
                return Err(ApiError::not_found("Project set is recycled"));
            }
        }
        if g["status"].as_i64() == Some(1) && ![1, 1060, 1010, 5].contains(&permission) {
            return Err(ApiError::forbidden("Finished projects are read-only"));
        }
    }
    let role = user_role(state, kind, &g, Some(user)).await?;
    if !has(&role, permission) {
        return Err(ApiError::forbidden("Insufficient group permission"));
    }
    Ok(g)
}
pub async fn require(r: &Request, kind: &str, gid: &str, permission: i64) -> Result<Value> {
    let user = r
        .user
        .as_ref()
        .ok_or_else(|| ApiError::unauthorized("Authentication required"))?;
    require_user(&r.state, user, kind, gid, permission).await
}
async fn base_decorate(s: &AppState, kind: &str, g: &Value, user: Option<&Value>) -> Result<Value> {
    let mut v = g.clone();
    v["group_type"] = json!(kind);
    v["create_time"] = g["created_at"].clone();
    v["edit_time"] = g.get("updated_at").unwrap_or(&g["created_at"]).clone();
    v["user_count"] = json!(s
        .find(
            "memberships",
            doc! {"group_type":kind,"group_id":core::field(g,"id")}
        )
        .await?
        .len());
    v["role"] = user_role(s, kind, g, user).await?;
    v["joined"] = json!(!v["role"].is_null());
    v["has_avatar"] = json!(!core::field(g, "avatar").is_empty());
    v["is_need_check_application"] = json!(g["application_check_type"].as_i64().unwrap_or(2) == 2);
    if v["max_user"].is_null() {
        v["max_user"] = json!(100000)
    }
    Ok(v)
}
pub async fn decorate_team(s: &AppState, g: &Value, user: Option<&Value>) -> Result<Value> {
    let mut v = base_decorate(s, "team", g, user).await?;
    for k in ["ocr_quota_month", "ocr_quota_used"] {
        if v[k].is_null() {
            v[k] = json!(0)
        }
    }
    let avatar = core::field(g, "avatar_resource_id");
    v["avatar"] = Value::Null;
    v["has_avatar"] = json!(!avatar.is_empty());
    if !avatar.is_empty() && has(&v["role"], 1) {
        let resource = s.get("resources", avatar).await?;
        if core::field(&resource, "state") == "bound" {
            v["avatar"] = json!(s
                .cos
                .presign("GET", core::field(&resource, "key"), 300, &[])?);
        }
    }
    Ok(v)
}
pub async fn decorate_project(s: &AppState, g: &Value, user: Option<&Value>) -> Result<Value> {
    let mut v = base_decorate(s, "project", g, user).await?;
    let t = s.get("teams", core::field(g, "team_id")).await?;
    v["team"] = decorate_team(s, &t, user).await?;
    v["auto_become_project_admin"] = json!(has(&user_role(s, "team", &t, user).await?, 1010));
    v["project_set"] = if core::field(g, "project_set_id").is_empty() {
        Value::Null
    } else {
        s.get("project_sets", core::field(g, "project_set_id"))
            .await?
    };
    v["source_language"] = if core::field(g, "source_language_id").is_empty() {
        Value::Null
    } else {
        s.get("languages", core::field(g, "source_language_id"))
            .await?
    };
    let files = s
        .find(
            "files",
            doc! {"project_id":core::field(g,"id"),"deleted_at":null,"activated":{"$ne":false}},
        )
        .await?;
    let ids: Vec<&str> = files.iter().map(|x| core::field(x, "id")).collect();
    let text_files: HashSet<&str> = files
        .iter()
        .filter(|f| f["type"].as_i64() == Some(3))
        .map(|f| core::field(f, "id"))
        .collect();
    let sources = s
        .find("sources", doc! {"file_id":{"$in":ids},"deleted_at":null})
        .await?;
    let sources: Vec<&Value> = sources
        .iter()
        .filter(|x| {
            !text_files.contains(core::field(x, "file_id"))
                || !core::field(x, "content").trim().is_empty()
        })
        .collect();
    let ids: Vec<&str> = sources.iter().map(|x| core::field(x, "id")).collect();
    let targets = s
        .find(
            "targets",
            doc! {"project_id":core::field(g,"id"),"deleted_at":null},
        )
        .await?;
    let target_ids: HashSet<&str> = targets.iter().map(|x| core::field(x, "id")).collect();
    let trs = s
        .find(
            "translations",
            doc! {"source_id":{"$in":ids},"deleted_at":null},
        )
        .await?;
    let trs: Vec<&Value> = trs
        .iter()
        .filter(|x| {
            target_ids.contains(core::field(x, "target_id"))
                && (!core::field(x, "content").is_empty()
                    || !core::field(x, "proofread_content").is_empty())
        })
        .collect();
    let translated: HashSet<&str> = trs.iter().map(|x| core::field(x, "source_id")).collect();
    let checked: HashSet<&str> = trs
        .iter()
        .filter(|x| x["selected"].as_bool() == Some(true))
        .map(|x| core::field(x, "source_id"))
        .collect();
    v["source_count"] = json!(sources.len());
    v["translated_source_count"] = json!(translated.len());
    v["checked_source_count"] = json!(checked.len());
    v["target_count"] = json!(targets.len());
    v["file_count"] = json!(files.len());
    v["import_from_labelplus_status"] = g
        .get("import_from_labelplus_status")
        .cloned()
        .unwrap_or(json!(2));
    v["import_from_labelplus_percent"] =
        g.get("import_from_labelplus_percent")
            .cloned()
            .unwrap_or(json!(
                if v["import_from_labelplus_status"].as_i64() == Some(2) {
                    100
                } else {
                    0
                }
            ));
    v["import_from_labelplus_error_type"] = g
        .get("import_from_labelplus_error_type")
        .cloned()
        .unwrap_or(json!(0));
    v["import_from_labelplus_error_type_name"] = g
        .get("import_from_labelplus_error_type_name")
        .cloned()
        .unwrap_or(json!("Import failed"));
    Ok(v)
}
fn metadata(b: &Value, kind: &str) -> Result<Value> {
    let name = core::required(b, "name")?;
    if name.chars().count() > 100 {
        return Err(ApiError::bad("Name too long"));
    }
    let allow = b["allow_apply_type"].as_i64().unwrap_or(1);
    let check = b["application_check_type"].as_i64().unwrap_or(2);
    if !(allow == 1 || allow == 2 || (kind == "project" && allow == 3)) || ![1, 2].contains(&check)
    {
        return Err(ApiError::bad("Invalid application policy"));
    }
    let rid = b["default_role"].as_str().unwrap_or(if kind == "team" {
        "team-member"
    } else {
        "project-supporter"
    });
    Ok(
        json!({"name":name,"intro":b["intro"].as_str().unwrap_or(""),"allow_apply_type":allow,"application_check_type":check,"default_role":rid}),
    )
}
async fn validate_default(s: &AppState, kind: &str, gid: &str, b: &Value) -> Result<()> {
    let role = resolve_role(s, kind, gid, core::field(b, "default_role")).await?;
    if level(&role) >= 400 {
        return Err(ApiError::bad("Default role cannot be administrator"));
    }
    Ok(())
}
pub async fn create_import_project(
    s: &AppState,
    user: &Value,
    team_id: &str,
    project_set_id: &str,
    b: &Value,
) -> Result<Value> {
    let team = s.get("teams", team_id).await?;
    if !live(&team) || !has(&user_role(s, "team", &team, Some(user)).await?, 1090) {
        return Err(ApiError::forbidden("Cannot create project"));
    }
    if !project_set_id.is_empty() {
        let set = s.get("project_sets", project_set_id).await?;
        if !live(&set) || core::field(&set, "team_id") != team_id {
            return Err(ApiError::bad("Invalid project set"));
        }
    }
    let mut v = metadata(b, "project")?;
    let project_id = if let Some(job) = b["import_job_id"].as_str() {
        format!("import:{job}")
    } else {
        core::id()
    };
    if b["import_job_id"].is_string() {
        if let Some(existing) = s
            .find("projects", doc! {"id":&project_id})
            .await?
            .into_iter()
            .next()
        {
            if core::field(&existing, "team_id") != team_id
                || core::field(&existing, "owner_id") != core::field(user, "id")
            {
                return Err(ApiError::forbidden("Import ownership mismatch"));
            }
            return Ok(existing);
        }
    }
    validate_default(s, "project", "", &v).await?;
    v["id"] = json!(project_id);
    v["created_at"] = json!(core::now());
    v["owner_id"] = user["id"].clone();
    v["team_id"] = json!(team_id);
    v["project_set_id"] = json!(project_set_id);
    v["status"] = json!(0);
    v["import_from_labelplus_status"] = json!(if b["import_job_id"].is_string() { 0 } else { 2 });
    v["import_from_labelplus_percent"] = json!(if b["import_job_id"].is_string() {
        0
    } else {
        100
    });
    let source = b["source_language_id"]
        .as_str()
        .or(b["source_language"].as_str())
        .or(b["source_language"]["id"].as_str())
        .unwrap_or("");
    if !source.is_empty() {
        s.get("languages", source).await?;
    }
    v["source_language_id"] = json!(source);
    let mut langs = Vec::new();
    if let Some(a) = b["target_languages"].as_array() {
        for x in a {
            if let Some(id) = x.as_str().or(x["id"].as_str()) {
                langs.push(id.to_owned())
            }
        }
    }
    if let Some(id) = b["output_language"]
        .as_str()
        .or(b["output_language"]["id"].as_str())
    {
        langs.push(id.to_owned())
    }
    langs.sort();
    langs.dedup();
    for language in &langs {
        s.get("languages", language).await?;
    }
    let mut session = s.mongo.start_session().await?;
    session.start_transaction().await?;
    if let Some(job_id) = b["import_job_id"].as_str() {
        let token = core::required(b, "import_claim_token")?;
        let fence=s.db.collection::<Document>("jobs").update_one(doc!{"id":job_id,"status":"running","claim_token":token,"lease_until":{"$gt":core::epoch()}},doc!{"$inc":{"mutation_sequence":1_i64}}).session(&mut session).await?;
        if fence.matched_count != 1 {
            return Err(ApiError::bad("Import job claim lost or cancelled"));
        }
    }
    let uid = core::field(user, "id");
    if uid.is_empty() {
        return Err(ApiError::unauthorized("Authentication required"));
    }
    let fresh_user =
        s.db.collection::<Document>("users")
            .find_one_and_update(
                doc! {"id":uid,"disabled":{"$ne":true}},
                doc! {"$inc":{"job_permission_sequence":1_i64}},
            )
            .return_document(mongodb::options::ReturnDocument::After)
            .session(&mut session)
            .await?
            .ok_or_else(|| ApiError::forbidden("Import owner unavailable or disabled"))?;
    let fresh_user: Value =
        mongodb::bson::from_document(fresh_user).map_err(|e| ApiError::internal(e.to_string()))?;
    let guard =
        s.db.collection::<Document>("teams")
            .update_one(
                doc! {"id":team_id,"deleted_at":null},
                doc! {"$inc":{"metadata_version":1}},
            )
            .session(&mut session)
            .await?;
    if guard.matched_count != 1 {
        return Err(ApiError::bad("Team unavailable"));
    }
    if !project_set_id.is_empty() {
        let guard =
            s.db.collection::<Document>("project_sets")
                .update_one(
                    doc! {"id":project_set_id,"deleted_at":null},
                    doc! {"$inc":{"metadata_version":1}},
                )
                .session(&mut session)
                .await?;
        if guard.matched_count != 1 {
            return Err(ApiError::bad("Project set unavailable"));
        }
    }
    require_user(s, &fresh_user, "team", team_id, 1090).await?;
    s.db.collection::<Document>("projects")
        .insert_one(mongodb::bson::to_document(&v).map_err(|e| ApiError::internal(e.to_string()))?)
        .session(&mut session)
        .await?;
    let pid = core::field(&v, "id");
    let uid = core::field(user, "id");
    s.db.collection::<Document>("memberships").insert_one(doc!{"id":format!("project:{pid}:{uid}"),"group_type":"project","group_id":pid,"user_id":uid,"role_id":"project-creator","created_at":core::now()}).session(&mut session).await?;
    for language in langs {
        s.db.collection::<Document>("targets").insert_one(doc!{"id":core::id(),"created_at":core::now(),"project_id":pid,"language_id":language}).session(&mut session).await?;
    }
    session.commit_transaction().await?;
    Ok(v)
}
async fn cascade(s: &AppState, kind: &str, gid: &str, restore: bool) -> Result<()> {
    let root_collection = match kind {
        "team" => "teams",
        "project" => "projects",
        "project_set" => "project_sets",
        "term_bank" => "term_banks",
        _ => return Err(ApiError::bad("Invalid recycle root")),
    };
    let root = s.get(root_collection, gid).await?;
    if restore {
        let at = root["deleted_at"]
            .as_i64()
            .ok_or_else(|| ApiError::bad("Not recycled"))?;
        if core::epoch() - at >= 604800 {
            return Err(ApiError::bad("Restore window expired"));
        }
        if core::field(&root, "deletion_root_id") != gid {
            return Err(ApiError::bad("Restore parent first"));
        }
        if core::field(&root, "deletion_state") == "deleting" {
            return Err(ApiError::bad("Permanent deletion has started"));
        }
        for file in s
            .find(
                "files",
                doc! {"deletion_root_type":kind,"deletion_root_id":gid},
            )
            .await?
        {
            let rid = core::field(&file, "resource_id");
            if !rid.is_empty() {
                let resource = s.get("resources", rid).await?;
                if core::field(&resource, "state") != "bound" {
                    return Err(ApiError::bad("Recycled resource unavailable"));
                }
            }
        }
        let mut session = s.mongo.start_session().await?;
        session.start_transaction().await?;
        if kind != "team" {
            let team_id = core::field(&root, "team_id");
            if !team_id.is_empty() {
                let parent =
                    s.db.collection::<Document>("teams")
                        .update_one(
                            doc! {"id":team_id,"deleted_at":null},
                            doc! {"$inc":{"metadata_version":1}},
                        )
                        .session(&mut session)
                        .await?;
                if parent.matched_count != 1 {
                    return Err(ApiError::bad("Restore team first"));
                }
            }
        }
        if kind == "project" {
            let sid = core::field(&root, "project_set_id");
            if !sid.is_empty() {
                let parent =
                    s.db.collection::<Document>("project_sets")
                        .update_one(
                            doc! {"id":sid,"deleted_at":null},
                            doc! {"$inc":{"metadata_version":1}},
                        )
                        .session(&mut session)
                        .await?;
                if parent.matched_count != 1 {
                    return Err(ApiError::bad("Restore project set first"));
                }
            }
        }
        let changed =
            s.db.collection::<Document>(root_collection)
                .update_one(
                    doc! {"id":gid,"deleted_at":at,"deletion_state":{"$ne":"deleting"}},
                    doc! {"$inc":{"metadata_version":1}},
                )
                .session(&mut session)
                .await?;
        if changed.matched_count != 1 {
            return Err(ApiError::bad("Recycle root changed"));
        }
        for c in [
            "teams",
            "project_sets",
            "projects",
            "files",
            "sources",
            "translations",
            "targets",
            "term_banks",
            "terms",
            "tips",
            "outputs",
        ] {
            s.db.collection::<Document>(c)
                .update_many(
                    doc! {"deletion_root_type":kind,"deletion_root_id":gid},
                    doc! {"$unset":{"deleted_at":"","deletion_root_type":"","deletion_root_id":""}},
                )
                .session(&mut session)
                .await?;
        }
        session.commit_transaction().await?;
        return Ok(());
    }
    let mut session = s.mongo.start_session().await?;
    session.start_transaction().await?;
    let guard =
        s.db.collection::<Document>(root_collection)
            .update_one(
                doc! {"id":gid,"deleted_at":null},
                doc! {"$inc":{"metadata_version":1}},
            )
            .session(&mut session)
            .await?;
    if guard.matched_count != 1 {
        return Err(ApiError::bad("Group already recycled"));
    }
    let at = core::epoch();
    let mut projects = Vec::new();
    if kind == "project" {
        projects.push(gid.to_string())
    } else if kind == "team" || kind == "project_set" {
        let filter = if kind == "team" {
            doc! {"team_id":gid,"deleted_at":null}
        } else {
            doc! {"project_set_id":gid,"deleted_at":null}
        };
        projects = s
            .find("projects", filter)
            .await?
            .iter()
            .map(|v| core::field(v, "id").to_owned())
            .collect();
    }
    if !projects.is_empty() {
        s.db.collection::<Document>("projects")
            .update_many(
                doc! {"id":{"$in":&projects},"deleted_at":null},
                doc! {"$inc":{"metadata_version":1}},
            )
            .session(&mut session)
            .await?;
    }
    let files = s
        .find(
            "files",
            doc! {"project_id":{"$in":&projects},"deleted_at":null},
        )
        .await?;
    let fids: Vec<&str> = files.iter().map(|x| core::field(x, "id")).collect();
    let sources = s
        .find(
            "sources",
            doc! {"file_id":{"$in":fids.clone()},"deleted_at":null},
        )
        .await?;
    let sids: Vec<&str> = sources.iter().map(|x| core::field(x, "id")).collect();
    let banks = if kind == "team" {
        s.find("term_banks", doc! {"team_id":gid,"deleted_at":null})
            .await?
    } else if kind == "term_bank" {
        vec![root.clone()]
    } else {
        vec![]
    };
    let bids: Vec<&str> = banks.iter().map(|x| core::field(x, "id")).collect();
    let mut filters = vec![
        (root_collection, doc! {"id":gid}),
        ("projects", doc! {"id":{"$in":&projects}}),
        ("files", doc! {"id":{"$in":fids}}),
        ("sources", doc! {"id":{"$in":sids.clone()}}),
        ("translations", doc! {"source_id":{"$in":sids.clone()}}),
        ("tips", doc! {"source_id":{"$in":sids}}),
        ("targets", doc! {"project_id":{"$in":&projects}}),
        ("outputs", doc! {"project_id":{"$in":&projects}}),
        ("term_banks", doc! {"id":{"$in":bids.clone()}}),
        ("terms", doc! {"term_bank_id":{"$in":bids}}),
    ];
    if kind == "team" {
        filters.push(("project_sets", doc! {"team_id":gid}));
    }
    for (c, mut filter) in filters {
        filter.insert("deleted_at", mongodb::bson::Bson::Null);
        s.db.collection::<Document>(c)
            .update_many(
                filter,
                doc! {"$set":{"deleted_at":at,"deletion_root_type":kind,"deletion_root_id":gid}},
            )
            .session(&mut session)
            .await?;
    }
    if !projects.is_empty() {
        s.db.collection::<Document>("jobs").update_many(doc!{"payload.project_id":{"$in":&projects},"status":{"$in":["preparing","queued","running"]}},doc!{"$set":{"status":"cancelled","cancel_reason":"group_recycled","last_error":"Project subtree recycled","finished_epoch":at,"updated_at":core::now()},"$unset":{"claim_token":"","lease_until":""}}).session(&mut session).await?;
    }
    session.commit_transaction().await?;
    Ok(())
}
fn matches(v: &Value, word: &str) -> bool {
    word.is_empty() || v.to_string().to_lowercase().contains(&word.to_lowercase())
}
async fn group_roles(s: &AppState, kind: &str, gid: &str) -> Result<Vec<Value>> {
    let mut roles = presets(kind);
    roles.extend(
        s.find("roles", doc! {"group_type":kind,"group_id":gid})
            .await?,
    );
    Ok(roles)
}
async fn assignable(r: &Request, kind: &str, g: &Value, rid: &str) -> Result<Value> {
    let role = resolve_role(&r.state, kind, core::field(g, "id"), rid).await?;
    let mine = user_role(&r.state, kind, g, r.user.as_ref()).await?;
    if level(&role) >= level(&mine) || core::field(&role, "system_code") == "creator" {
        return Err(ApiError::forbidden("Cannot assign equal or higher role"));
    }
    if role["permissions"].as_array().map_or(false, |ps| {
        ps.iter()
            .any(|p| !has(&mine, p["id"].as_i64().or(p.as_i64()).unwrap_or(0)))
    }) {
        return Err(ApiError::forbidden("Cannot grant permissions you lack"));
    }
    Ok(role)
}
async fn members(s: &AppState, kind: &str, gid: &str) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for m in s
        .find("memberships", doc! {"group_type":kind,"group_id":gid})
        .await?
    {
        let mut u = crate::auth::decorate_user(
            s,
            &s.get("users", core::field(&m, "user_id")).await?,
            false,
        )
        .await?;
        u["role"] = resolve_role(s, kind, gid, core::field(&m, "role_id")).await?;
        u["remark"] = m["remark"].clone();
        out.push(u)
    }
    Ok(out)
}
async fn workflow_view(r: &Request, w: &Value) -> Result<Value> {
    let mut v = w.clone();
    let kind = core::field(w, "group_type");
    let gid = core::field(w, "group_id");
    let g = r.state.get(collection(kind)?, gid).await?;
    v["group"] = if kind == "team" {
        decorate_team(&r.state, &g, r.user.as_ref()).await?
    } else {
        decorate_project(&r.state, &g, r.user.as_ref()).await?
    };
    let user = r.state.get("users", core::field(w, "user_id")).await?;
    v["user"] = crate::auth::decorate_user(&r.state, &user, false).await?;
    v["operator"] = if core::field(w, "operator_id").is_empty() {
        Value::Null
    } else {
        crate::auth::decorate_user(
            &r.state,
            &r.state.get("users", core::field(w, "operator_id")).await?,
            false,
        )
        .await?
    };
    v["role"] = resolve_role(&r.state, kind, gid, core::field(w, "role_id")).await?;
    v["user_role"] = user_role(&r.state, kind, &g, Some(&user)).await?;
    let viewer = user_role(&r.state, kind, &g, r.user.as_ref()).await?;
    v["group_roles"] = json!(group_roles(&r.state, kind, gid)
        .await?
        .into_iter()
        .filter(|role| level(role) < level(&viewer)
            && role["permissions"].as_array().map_or(false, |ps| ps
                .iter()
                .all(|p| has(&viewer, p["id"].as_i64().unwrap_or(0)))))
        .collect::<Vec<_>>());
    v["create_time"] = w["created_at"].clone();
    Ok(v)
}
async fn active_group(s: &AppState, kind: &str, gid: &str) -> Result<Value> {
    let g = s.get(collection(kind)?, gid).await?;
    if !live(&g) {
        return Err(ApiError::not_found("Group recycled"));
    }
    if kind == "project" {
        let team = s.get("teams", core::field(&g, "team_id")).await?;
        if !live(&team) {
            return Err(ApiError::not_found("Team recycled"));
        }
        let sid = core::field(&g, "project_set_id");
        if !sid.is_empty() && !live(&s.get("project_sets", sid).await?) {
            return Err(ApiError::not_found("Project set recycled"));
        }
    }
    Ok(g)
}
async fn settle(r: &Request, c: &str, w: &Value, allow: bool) -> Result<Value> {
    let kind = core::field(w, "group_type");
    let gid = core::field(w, "group_id");
    let uid = core::field(w, "user_id");
    let mut session = r.state.mongo.start_session().await?;
    session.start_transaction().await?;
    let counter = r
        .state
        .db
        .collection::<Document>(collection(kind)?)
        .update_one(
            doc! {"id":gid,"deleted_at":null},
            doc! {"$inc":{"membership_version":1}},
        )
        .session(&mut session)
        .await?;
    if counter.matched_count != 1 {
        return Err(ApiError::bad("Group unavailable"));
    }
    let g = active_group(&r.state, kind, gid).await?;
    if kind == "project" && g["status"].as_i64() == Some(1) {
        return Err(ApiError::forbidden("Finished projects are read-only"));
    }
    let mut rid = core::field(w, "role_id").to_string();
    if c == "applications" {
        require(r, kind, gid, 101).await?;
        if allow {
            rid = r.body["role_id"].as_str().unwrap_or(&rid).to_owned();
            assignable(r, kind, &g, &rid).await?;
        }
    } else {
        if r.uid()? != core::field(w, "user_id") {
            return Err(ApiError::forbidden("Invitation is private"));
        }
        if allow {
            let operator = r.state.get("users", core::field(w, "operator_id")).await?;
            let inviter = user_role(&r.state, kind, &g, Some(&operator)).await?;
            let assigned = resolve_role(&r.state, kind, gid, &rid).await?;
            if !has(&inviter, 105)
                || level(&assigned) >= level(&inviter)
                || !assigned["permissions"].as_array().map_or(false, |ps| {
                    ps.iter()
                        .all(|p| has(&inviter, p["id"].as_i64().unwrap_or(0)))
                })
            {
                return Err(ApiError::forbidden(
                    "Invitation authority is no longer valid",
                ));
            }
        }
    }
    resolve_role(&r.state, kind, gid, &rid).await?;
    let count = r
        .state
        .db
        .collection::<Document>("memberships")
        .count_documents(doc! {"group_type":kind,"group_id":gid})
        .session(&mut session)
        .await?;
    if allow && count >= g["max_user"].as_u64().unwrap_or(100000) {
        return Err(ApiError::bad("Group is full"));
    }
    let changed=r.state.db.collection::<Document>(c).update_one(doc!{"id":core::field(w,"id"),"status":0,"role_id":core::field(w,"role_id")},doc!{"$set":{"status":if allow{1}else{2},"role_id":&rid,"operator_id":r.uid()?,"updated_at":core::now()}}).session(&mut session).await?;
    if changed.matched_count != 1 {
        return Err(ApiError::bad("Request already processed or changed"));
    }
    if allow {
        let id = format!("{kind}:{gid}:{uid}");
        r.state.db.collection::<Document>("memberships").update_one(doc!{"group_type":kind,"group_id":gid,"user_id":uid},doc!{"$setOnInsert":{"id":id,"group_type":kind,"group_id":gid,"user_id":uid,"role_id":rid,"created_at":core::now()}}).upsert(true).session(&mut session).await?;
    }
    session.commit_transaction().await?;
    workflow_view(r, &r.state.get(c, core::field(w, "id")).await?).await
}

pub async fn handle(r: &Request) -> Result<Option<Reply>> {
    let p: Vec<&str> = r.path.iter().map(String::as_str).collect();
    let method = &r.method;
    let s = &r.state;
    if p == ["types", "locale"] {
        if *method != Method::GET {
            return Err(ApiError::bad("GET required"));
        }
        return Ok(Some(json!([{"id":"auto","name":"Automatic","intro":"Use browser locale"},{"id":"zh-CN","name":"简体中文"},{"id":"zh-TW","name":"繁體中文"},{"id":"en","name":"English"}]).into()));
    }
    if p.first() == Some(&"types") && p.len() == 2 {
        if *method != Method::GET {
            return Err(ApiError::bad("GET required"));
        }
        let kind = r.q("group_type");
        collection(kind)?;
        let data = match p[1] {
            "permission" => json!(codes(kind)
                .into_iter()
                .map(|id| json!({"id":id,"name":permission_name(id,kind),"intro":""}))
                .collect::<Vec<_>>()),
            "system-role" => json!(presets(kind)
                .into_iter()
                .filter(
                    |v| r.q("with_creator") == "true" || core::field(v, "system_code") != "creator"
                )
                .collect::<Vec<_>>()),
            "allow-apply-type" => {
                let mut options = vec![
                    json!({"id":1,"name":"Nobody"}),
                    json!({"id":2,"name":"Everyone"}),
                ];
                if kind == "project" {
                    options.push(json!({"id":3,"name":"Team members"}));
                }
                json!(options)
            }
            "application-check-type" => {
                json!([{ "id":1,"name":"Join immediately"},{"id":2,"name":"Administrator review"}])
            }
            _ => return Ok(None),
        };
        return Ok(Some(data.into()));
    }
    if p.len() == 2
        && p[0] == "user"
        && [
            "teams",
            "projects",
            "invitations",
            "related-applications",
            "recycle-bin",
        ]
        .contains(&p[1])
    {
        if *method != Method::GET {
            return Err(ApiError::bad("GET required"));
        }
        let uid = r.uid()?;
        let mut out = Vec::new();
        match p[1] {
            "teams" | "projects" => {
                let kind = if p[1] == "teams" { "team" } else { "project" };
                for g in s.find(p[1], doc! {"deleted_at":null}).await? {
                    if kind == "project"
                        && active_group(s, kind, core::field(&g, "id")).await.is_err()
                    {
                        continue;
                    }
                    if !user_role(s, kind, &g, r.user.as_ref()).await?.is_null()
                        && matches(&g, r.q("word"))
                        && (r.q("status").is_empty() || g["status"].to_string() == r.q("status"))
                    {
                        out.push(if kind == "team" {
                            decorate_team(s, &g, r.user.as_ref()).await?
                        } else {
                            decorate_project(s, &g, r.user.as_ref()).await?
                        })
                    }
                }
            }
            "invitations" | "related-applications" => {
                let c = if p[1] == "invitations" {
                    "invitations"
                } else {
                    "applications"
                };
                for w in s.find(c, doc! {}).await? {
                    let own = core::field(&w, "user_id") == uid;
                    let g = s
                        .get(
                            collection(core::field(&w, "group_type"))?,
                            core::field(&w, "group_id"),
                        )
                        .await?;
                    if own
                        || (c == "applications"
                            && has(
                                &user_role(s, core::field(&w, "group_type"), &g, r.user.as_ref())
                                    .await?,
                                101,
                            ))
                    {
                        out.push(workflow_view(r, &w).await?)
                    }
                }
            }
            _ => {
                for (kind, c) in [("team", "teams"), ("project", "projects")] {
                    if !r.q("group_type").is_empty() && r.q("group_type") != kind {
                        continue;
                    }
                    for g in s.find(c, doc! {"deleted_at":{"$ne":null}}).await? {
                        if has(&user_role(s, kind, &g, r.user.as_ref()).await?, 5) {
                            out.push(recycle_item(g, kind));
                        }
                    }
                }
                if r.q("group_type").is_empty() || r.q("group_type") == "file" {
                    for file in s.find("files", doc! {"deleted_at":{"$ne":null}}).await? {
                        let project = s.get("projects", core::field(&file, "project_id")).await?;
                        if has(
                            &user_role(s, "project", &project, r.user.as_ref()).await?,
                            1050,
                        ) {
                            out.push(recycle_item(file, "file"));
                        }
                    }
                }
            }
        }
        return Ok(Some(Reply::list(out, r)));
    }
    if p == ["teams"] {
        if *method == Method::POST {
            let uid = r.uid()?;
            let settings = s.get("site_settings", "site").await?;
            if settings["only_allow_admin_create_team"].as_bool() == Some(true) && !r.is_admin() {
                return Err(ApiError::forbidden("Only administrators may create teams"));
            }
            let mut v = metadata(&r.body, "team")?;
            validate_default(s, "team", "", &v).await?;
            v["owner_id"] = json!(uid);
            v["id"] = json!(core::id());
            v["created_at"] = json!(core::now());
            let mut session = s.mongo.start_session().await?;
            session.start_transaction().await?;
            s.db.collection::<Document>("teams")
                .insert_one(
                    mongodb::bson::to_document(&v)
                        .map_err(|e| ApiError::internal(e.to_string()))?,
                )
                .session(&mut session)
                .await?;
            let gid = core::field(&v, "id");
            s.db.collection::<Document>("memberships").insert_one(doc!{"id":format!("team:{gid}:{uid}"),"group_type":"team","group_id":gid,"user_id":uid,"role_id":"team-creator","created_at":core::now()}).session(&mut session).await?;
            session.commit_transaction().await?;
            return Ok(Some(
                json!({"message":"Created","team":decorate_team(s,&v,r.user.as_ref()).await?})
                    .into(),
            ));
        }
        if *method == Method::GET {
            let mut out = Vec::new();
            for g in s.find("teams", doc! {"deleted_at":null}).await? {
                if matches(&g, r.q("word")) {
                    out.push(public_team_metadata(
                        &decorate_team(s, &g, r.user.as_ref()).await?,
                    ));
                }
            }
            return Ok(Some(Reply::list(out, r)));
        }
        return Err(ApiError::bad("Unsupported method"));
    }
    if p.len() >= 2 && (p[0] == "teams" || p[0] == "projects") {
        let kind = if p[0] == "teams" { "team" } else { "project" };
        let gid = p[1];
        if p.len() == 2 {
            if *method == Method::GET && kind == "team" {
                r.uid()?;
                let team = active_group(s, "team", gid).await?;
                let decorated = decorate_team(s, &team, r.user.as_ref()).await?;
                return Ok(Some(
                    if has(&decorated["role"], 1) {
                        decorated
                    } else {
                        public_team_metadata(&decorated)
                    }
                    .into(),
                ));
            }
            let permission = if *method == Method::GET {
                1
            } else if *method == Method::DELETE {
                5
            } else {
                10
            };
            let g = require(r, kind, gid, permission).await?;
            if *method == Method::DELETE {
                cascade(s, kind, gid, false).await?;
                return Ok(Some(
                    json!({"message":"Recycled; restore within seven days"}).into(),
                ));
            }
            let g = if *method == Method::PUT {
                let mut fields = metadata(&r.body, kind)?;
                validate_default(s, kind, gid, &fields).await?;
                if kind == "project" && r.body.get("project_set_id").is_some() {
                    let sid = core::field(&r.body, "project_set_id");
                    if !sid.is_empty() {
                        let set = s.get("project_sets", sid).await?;
                        if !live(&set) || core::field(&set, "team_id") != core::field(&g, "team_id")
                        {
                            return Err(ApiError::bad("Invalid project set"));
                        }
                    }
                    fields["project_set_id"] = json!(sid)
                }
                s.patch(p[0], gid, fields).await?
            } else if *method == Method::GET {
                g
            } else {
                return Err(ApiError::bad("Unsupported method"));
            };
            let v = if kind == "team" {
                decorate_team(s, &g, r.user.as_ref()).await?
            } else {
                decorate_project(s, &g, r.user.as_ref()).await?
            };
            if *method == Method::PUT {
                let mut reply = json!({"message":"Updated"});
                reply[kind] = v;
                return Ok(Some(reply.into()));
            }
            return Ok(Some(v.into()));
        }
        if p.len() == 3 && ["finish", "resume", "restore"].contains(&p[2]) {
            if *method != Method::POST {
                return Err(ApiError::bad("POST required"));
            }
            if p[2] == "restore" {
                let g = s.get(p[0], gid).await?;
                r.uid()?;
                let role = user_role(s, kind, &g, r.user.as_ref()).await?;
                if !has(&role, 5) {
                    return Err(ApiError::forbidden("Cannot restore group"));
                }
                cascade(s, kind, gid, true).await?;
            } else {
                if kind != "project" {
                    return Err(ApiError::bad("Only projects can finish"));
                }
                require(r, kind, gid, 1010).await?;
                let mut session = s.mongo.start_session().await?;
                session.start_transaction().await?;
                let changed=s.db.collection::<Document>("projects").update_one(doc!{"id":gid,"deleted_at":null},doc!{"$set":{"status":if p[2]=="finish"{1}else{0},"updated_at":core::now()},"$inc":{"metadata_version":1}}).session(&mut session).await?;
                if changed.matched_count != 1 {
                    return Err(ApiError::bad("Project unavailable"));
                }
                if p[2] == "finish" {
                    s.db.collection::<Document>("jobs").update_many(doc!{"payload.project_id":gid,"kind":{"$in":["ai","text_parse","import"]},"status":{"$in":["preparing","queued","running"]}},doc!{"$set":{"status":"cancelled","cancel_reason":"project_finished","last_error":"Project finished; modifying tasks cancelled","finished_epoch":core::epoch(),"updated_at":core::now()},"$unset":{"claim_token":"","lease_until":""}}).session(&mut session).await?;
                }
                session.commit_transaction().await?;
            }
            return Ok(Some(json!({"message":"Updated"}).into()));
        }
        if p.len() == 3 && p[2] == "public-info" {
            if *method != Method::GET {
                return Err(ApiError::bad("GET required"));
            }
            let g = active_group(s, kind, gid).await?;
            let role = user_role(s, kind, &g, r.user.as_ref()).await?;
            return Ok(Some(json!({"id":gid,"name":g["name"],"joined":!role.is_null(),"user_count":s.find("memberships",doc!{"group_type":kind,"group_id":gid}).await?.len(),"application_check_type":g["application_check_type"],"allow_apply_type":g["allow_apply_type"]}).into()));
        }
        if p.len() >= 3 && p[2] == "users" {
            let actor = r.uid()?;
            let leaving = *method == Method::DELETE && p.get(3).copied() == Some(actor.as_str());
            require(
                r,
                kind,
                gid,
                if p.len() == 3 || leaving {
                    1
                } else if *method == Method::DELETE {
                    110
                } else {
                    115
                },
            )
            .await?;
            if p.len() == 3 && *method == Method::GET {
                return Ok(Some(Reply::list(
                    members(s, kind, gid)
                        .await?
                        .into_iter()
                        .filter(|v| matches(v, r.q("word")))
                        .collect(),
                    r,
                )));
            }
            if p.len() != 4 {
                return Err(ApiError::bad("Unsupported member route"));
            }
            if *method != Method::DELETE && *method != Method::PUT {
                return Err(ApiError::bad("Unsupported member method"));
            }
            let mut session = s.mongo.start_session().await?;
            session.start_transaction().await?;
            let guard =
                s.db.collection::<Document>(collection(kind)?)
                    .update_one(
                        doc! {"id":gid,"deleted_at":null},
                        doc! {"$inc":{"membership_version":1}},
                    )
                    .session(&mut session)
                    .await?;
            if guard.matched_count != 1 {
                return Err(ApiError::bad("Group unavailable"));
            }
            let g = require(
                r,
                kind,
                gid,
                if leaving {
                    1
                } else if *method == Method::DELETE {
                    110
                } else {
                    115
                },
            )
            .await?;
            let uid = p[3];
            if core::field(&g, "owner_id") == uid {
                return Err(ApiError::forbidden("Owner cannot be removed or demoted"));
            }
            let m = membership(s, kind, gid, uid)
                .await?
                .ok_or_else(|| ApiError::not_found("Member not found"))?;
            let target = resolve_role(s, kind, gid, core::field(&m, "role_id")).await?;
            let mine = user_role(s, kind, &g, r.user.as_ref()).await?;
            if uid != actor && level(&target) >= level(&mine) {
                return Err(ApiError::forbidden("Cannot change equal or higher member"));
            }
            let filter = doc! {"id":core::field(&m,"id"),"role_id":core::field(&m,"role_id")};
            if *method == Method::DELETE {
                let changed =
                    s.db.collection::<Document>("memberships")
                        .delete_one(filter)
                        .session(&mut session)
                        .await?;
                if changed.deleted_count != 1 {
                    return Err(ApiError::bad("Member changed concurrently"));
                }
                session.commit_transaction().await?;
                let group = if kind == "team" {
                    decorate_team(s, &g, r.user.as_ref()).await?
                } else {
                    decorate_project(s, &g, r.user.as_ref()).await?
                };
                return Ok(Some(json!({"message":"Removed","group":group}).into()));
            }
            if *method == Method::PUT {
                let rid = r.body["role"]
                    .as_str()
                    .or(r.body["role_id"].as_str())
                    .ok_or_else(|| ApiError::bad("role required"))?;
                let role = assignable(r, kind, &g, rid).await?;
                let changed =
                    s.db.collection::<Document>("memberships")
                        .update_one(
                            filter,
                            doc! {"$set":{"role_id":rid,"updated_at":core::now()}},
                        )
                        .session(&mut session)
                        .await?;
                if changed.matched_count != 1 {
                    return Err(ApiError::bad("Member changed concurrently"));
                }
                session.commit_transaction().await?;
                return Ok(Some(json!({"message":"Updated","role":role}).into()));
            }
            return Err(ApiError::bad("Unsupported member route"));
        }
        if p.len() >= 3 && p[2] == "roles" {
            return Ok(Some(roles(r, kind, gid, &p[3..]).await?));
        }
        if p.len() == 3 && ["invitations", "applications"].contains(&p[2]) {
            let c = p[2];
            if *method == Method::GET {
                require(r, kind, gid, if c == "invitations" { 105 } else { 101 }).await?;
                let mut out = Vec::new();
                for w in s.find(c, doc! {"group_type":kind,"group_id":gid}).await? {
                    out.push(workflow_view(r, &w).await?)
                }
                return Ok(Some(Reply::list(out, r)));
            }
            if *method != Method::POST {
                return Err(ApiError::bad("Unsupported method"));
            }
            let mut session = s.mongo.start_session().await?;
            session.start_transaction().await?;
            let guard =
                s.db.collection::<Document>(p[0])
                    .update_one(
                        doc! {"id":gid,"deleted_at":null},
                        doc! {"$inc":{"membership_version":1}},
                    )
                    .session(&mut session)
                    .await?;
            if guard.matched_count != 1 {
                return Err(ApiError::bad("Group unavailable"));
            }
            let uid = r.uid()?;
            let g = active_group(s, kind, gid).await?;
            if kind == "project" && g["status"].as_i64() == Some(1) {
                return Err(ApiError::forbidden("Finished projects are read-only"));
            }
            let (user, rid) = if c == "invitations" {
                require(r, kind, gid, 105).await?;
                let user = core::required(&r.body, "user_id")?;
                s.get("users", &user).await?;
                let rid = core::required(&r.body, "role_id")?;
                assignable(r, kind, &g, &rid).await?;
                (user, rid)
            } else {
                match g["allow_apply_type"].as_i64() {
                    Some(2) => {}
                    Some(3) if kind == "project" => {
                        require(r, "team", core::field(&g, "team_id"), 1).await?;
                    }
                    _ => return Err(ApiError::forbidden("Applications closed")),
                }
                (uid, core::field(&g, "default_role").to_string())
            };
            resolve_role(s, kind, gid, &rid).await?;
            let immediate = c == "applications" && g["application_check_type"].as_i64() == Some(1);
            let w = json!({"id":core::id(),"created_at":core::now(),"group_type":kind,"group_id":gid,"user_id":user,"role_id":rid,"operator_id":if c=="invitations"{r.uid()?}else{String::new()},"status":if immediate{1}else{0},"message":r.body["message"].as_str().unwrap_or("")});
            if s.db
                .collection::<Document>("memberships")
                .count_documents(
                    doc! {"group_type":kind,"group_id":gid,"user_id":core::field(&w,"user_id")},
                )
                .session(&mut session)
                .await?
                > 0
            {
                return Err(ApiError::bad("Already a member"));
            }
            if s.db.collection::<Document>(c).count_documents(doc!{"group_type":kind,"group_id":gid,"user_id":core::field(&w,"user_id"),"status":0}).session(&mut session).await?>0{return Err(ApiError::bad("Pending request already exists"))}
            if immediate {
                let count =
                    s.db.collection::<Document>("memberships")
                        .count_documents(doc! {"group_type":kind,"group_id":gid})
                        .session(&mut session)
                        .await?;
                if count >= g["max_user"].as_u64().unwrap_or(100000) {
                    return Err(ApiError::bad("Group is full"));
                }
                let user = core::field(&w, "user_id");
                s.db.collection::<Document>("memberships").insert_one(doc!{"id":format!("{kind}:{gid}:{user}"),"group_type":kind,"group_id":gid,"user_id":user,"role_id":core::field(&w,"role_id"),"created_at":core::now()}).session(&mut session).await?;
            }
            s.db.collection::<Document>(c)
                .insert_one(
                    mongodb::bson::to_document(&w)
                        .map_err(|e| ApiError::internal(e.to_string()))?,
                )
                .session(&mut session)
                .await?;
            session.commit_transaction().await?;
            return Ok(Some(workflow_view(r, &w).await?.into()));
        }
        if kind == "team"
            && p.len() == 3
            && ["projects", "project-sets", "recycle-bin", "term-banks"].contains(&p[2])
        {
            let c = p[2];
            if c == "recycle-bin" {
                require(r, "team", gid, 10).await?;
                if *method != Method::GET {
                    return Err(ApiError::bad("GET required"));
                }
                let mut out = s
                    .find("projects", doc! {"team_id":gid,"deleted_at":{"$ne":null}})
                    .await?
                    .into_iter()
                    .map(|v| recycle_item(v, "project"))
                    .collect::<Vec<_>>();
                let projects = s.find("projects", doc! {"team_id":gid}).await?;
                let ids: Vec<&str> = projects.iter().map(|x| core::field(x, "id")).collect();
                out.extend(
                    s.find(
                        "files",
                        doc! {"project_id":{"$in":ids},"deleted_at":{"$ne":null}},
                    )
                    .await?
                    .into_iter()
                    .map(|v| recycle_item(v, "file")),
                );
                out.extend(
                    s.find(
                        "project_sets",
                        doc! {"team_id":gid,"deleted_at":{"$ne":null}},
                    )
                    .await?
                    .into_iter()
                    .map(|v| recycle_item(v, "project_set")),
                );
                return Ok(Some(Reply::list(out, r)));
            }
            if *method == Method::POST {
                require(
                    r,
                    "team",
                    gid,
                    match c {
                        "projects" => 1090,
                        "project-sets" => 1100,
                        _ => 1020,
                    },
                )
                .await?;
                let v = if c == "projects" {
                    let sid = r.body["project_set_id"]
                        .as_str()
                        .or(r.body["project_set"].as_str())
                        .unwrap_or("");
                    let user = r
                        .user
                        .as_ref()
                        .ok_or_else(|| ApiError::unauthorized("Authentication required"))?;
                    let g = create_import_project(s, user, gid, sid, &r.body).await?;
                    decorate_project(s, &g, r.user.as_ref()).await?
                } else if c == "project-sets" {
                    insert_child(s,"teams",gid,"project_sets",json!({"name":core::required(&r.body,"name")?,"intro":r.body["intro"].as_str().unwrap_or(""),"team_id":gid,"owner_id":r.uid()?})).await?
                } else {
                    let mut v = bank_fields(s, &r.body).await?;
                    v["team_id"] = json!(gid);
                    v["owner_id"] = json!(r.uid()?);
                    insert_child(s, "teams", gid, "term_banks", v).await?
                };
                let key = match c {
                    "projects" => "project",
                    "project-sets" => "project_set",
                    _ => "term_bank",
                };
                let mut reply = json!({"message":"Created"});
                reply[key] = v;
                return Ok(Some(reply.into()));
            }
            if *method == Method::GET {
                r.uid()?;
                let team = active_group(s, "team", gid).await?;
                let team_access = has(&user_role(s, "team", &team, r.user.as_ref()).await?, 1);
                if c == "term-banks" {
                    require(r, "team", gid, 1).await?;
                }
                let col = match c {
                    "projects" => "projects",
                    "project-sets" => "project_sets",
                    _ => "term_banks",
                };
                let mut out = Vec::new();
                for g in s.find(col, doc! {"team_id":gid,"deleted_at":null}).await? {
                    if !matches(&g, r.q("word")) {
                        continue;
                    }
                    if c == "projects" {
                        if !r.q("project_set").is_empty()
                            && core::field(&g, "project_set_id") != r.q("project_set")
                        {
                            continue;
                        }
                        if !r.q("status").is_empty() && g["status"].to_string() != r.q("status") {
                            continue;
                        }
                        let sid = core::field(&g, "project_set_id");
                        if !sid.is_empty() && !live(&s.get("project_sets", sid).await?) {
                            continue;
                        }
                        if !has(&user_role(s, "project", &g, r.user.as_ref()).await?, 1) {
                            continue;
                        }
                        out.push(decorate_project(s, &g, r.user.as_ref()).await?)
                    } else if c == "term-banks" {
                        out.push(decorate_bank(s, &g).await?)
                    } else {
                        let visible = visible_set_projects(r, &g, team_access).await?;
                        if !team_access && visible.is_empty() {
                            continue;
                        }
                        let mut v = g.clone();
                        v["project_count"] = json!(visible.len());
                        v["create_time"] = g["created_at"].clone();
                        out.push(v)
                    }
                }
                return Ok(Some(Reply::list(out, r)));
            }
            return Err(ApiError::bad("Unsupported method"));
        }
        if kind == "team" && p.len() >= 4 && p[2] == "insight" {
            require(r, "team", gid, 1150).await?;
            if *method != Method::GET {
                return Err(ApiError::bad("GET required"));
            }
            return Ok(Some(insight(r, gid, &p[3..]).await?));
        }
    }
    if p.len() == 2 && ["invitations", "applications"].contains(&p[0]) {
        let c = p[0];
        let w = s.get(c, p[1]).await?;
        if *method == Method::PATCH {
            let allow = r.body["allow"]
                .as_bool()
                .ok_or_else(|| ApiError::bad("allow required"))?;
            let result = settle(r, c, &w, allow).await?;
            return Ok(Some(if c == "applications" {
                json!({"message":"Processed","application":result}).into()
            } else {
                result.into()
            }));
        }
        let kind = core::field(&w, "group_type");
        let gid = core::field(&w, "group_id");
        if *method == Method::DELETE {
            let uid = r.uid()?;
            if uid != core::field(&w, "user_id") {
                require(r, kind, gid, if c == "invitations" { 105 } else { 101 }).await?;
            }
            s.db.collection::<Document>(c)
                .update_one(
                    doc! {"id":p[1],"status":0},
                    doc! {"$set":{"status":3,"updated_at":core::now()}},
                )
                .await?;
            return Ok(Some(json!({"message":"Withdrawn"}).into()));
        }
        if *method == Method::PUT && c == "invitations" {
            let g = require(r, kind, gid, 105).await?;
            let rid = core::required(&r.body, "role_id")?;
            assignable(r, kind, &g, &rid).await?;
            let changed =
                s.db.collection::<Document>(c)
                    .update_one(doc! {"id":p[1],"status":0}, doc! {"$set":{"role_id":rid}})
                    .await?;
            if changed.matched_count == 0 {
                return Err(ApiError::bad("Already processed"));
            }
            return Ok(Some(workflow_view(r, &s.get(c, p[1]).await?).await?.into()));
        }
        return Err(ApiError::bad("Unsupported workflow method"));
    }
    if p.len() >= 2 && p[0] == "project-sets" {
        let mut set = s.get("project_sets", p[1]).await?;
        if p.len() == 3 && p[2] == "restore" && *method == Method::POST {
            require(r, "team", core::field(&set, "team_id"), 1120).await?;
            cascade(s, "project_set", p[1], true).await?;
            return Ok(Some(json!({"message":"Restored"}).into()));
        }
        if !live(&set) {
            return Err(ApiError::not_found("Project set recycled"));
        }
        if p.len() != 2 {
            return Err(ApiError::bad("Unsupported project set route"));
        }
        if *method == Method::GET {
            r.uid()?;
            let team = active_group(s, "team", core::field(&set, "team_id")).await?;
            let team_access = has(&user_role(s, "team", &team, r.user.as_ref()).await?, 1);
            let visible = visible_set_projects(r, &set, team_access).await?;
            if !team_access && visible.is_empty() {
                return Err(ApiError::forbidden("Project set is private"));
            }
            set["project_count"] = json!(visible.len());
            set["create_time"] = set["created_at"].clone();
            return Ok(Some(set.into()));
        }
        require(
            r,
            "team",
            core::field(&set, "team_id"),
            if *method == Method::DELETE {
                1120
            } else {
                1110
            },
        )
        .await?;
        if *method == Method::PUT {
            return Ok(Some(json!({"message":"Updated","project_set":s.patch("project_sets",p[1],json!({"name":core::required(&r.body,"name")?,"intro":r.body["intro"].as_str().unwrap_or("")})).await?}).into()));
        }
        if *method == Method::DELETE {
            cascade(s, "project_set", p[1], false).await?;
            return Ok(Some(json!({"message":"Recycled"}).into()));
        }
        return Err(ApiError::bad("Unsupported method"));
    }
    if p.len() >= 2 && (p[0] == "term-banks" || p[0] == "terms") {
        return Ok(Some(terms(r, &p).await?));
    }
    Ok(None)
}
async fn bank_fields(s: &AppState, b: &Value) -> Result<Value> {
    let source = core::required(b, "source_language_id")?;
    let target = core::required(b, "target_language_id")?;
    s.get("languages", &source).await?;
    s.get("languages", &target).await?;
    Ok(
        json!({"name":core::required(b,"name")?,"tip":b["tip"].as_str().unwrap_or(""),"source_language_id":source,"target_language_id":target}),
    )
}
async fn decorate_bank(s: &AppState, b: &Value) -> Result<Value> {
    let mut v = b.clone();
    v["source_language"] = s
        .get("languages", core::field(b, "source_language_id"))
        .await?;
    v["target_language"] = s
        .get("languages", core::field(b, "target_language_id"))
        .await?;
    v["term_count"] = json!(s
        .find(
            "terms",
            doc! {"term_bank_id":core::field(b,"id"),"deleted_at":null}
        )
        .await?
        .len());
    Ok(v)
}
async fn terms(r: &Request, p: &[&str]) -> Result<Reply> {
    let s = &r.state;
    let term = if p[0] == "terms" {
        Some(s.get("terms", p[1]).await?)
    } else {
        None
    };
    let bank = s
        .get(
            "term_banks",
            term.as_ref()
                .map(|t| core::field(t, "term_bank_id"))
                .unwrap_or(p[1]),
        )
        .await?;
    require(r, "team", core::field(&bank, "team_id"), 1).await?;
    let uid = r.uid()?;
    let own = core::field(&bank, "owner_id") == uid
        || term
            .as_ref()
            .map_or(false, |t| core::field(t, "owner_id") == uid);
    if p.len() == 3 && p[2] == "restore" && r.method == Method::POST {
        if !own {
            require(
                r,
                "team",
                core::field(&bank, "team_id"),
                if term.is_some() { 1080 } else { 1050 },
            )
            .await?;
        }
        if let Some(t) = term {
            if !live(&bank) {
                return Err(ApiError::bad("Restore term bank first"));
            }
            let at = t["deleted_at"]
                .as_i64()
                .ok_or_else(|| ApiError::bad("Not recycled"))?;
            if core::epoch() - at >= 604800 || core::field(&t, "deletion_root_id") != p[1] {
                return Err(ApiError::bad("Restore window expired or parent recycled"));
            }
            let changed = s
                .db
                .collection::<Document>("terms")
                .update_one(
                    doc! {"id":p[1],"deleted_at":at,"deletion_state":{"$ne":"deleting"}},
                    doc! {"$unset":{"deleted_at":"","deletion_root_type":"","deletion_root_id":""}},
                )
                .await?;
            if changed.matched_count != 1 {
                return Err(ApiError::bad("Permanent deletion started"));
            }
        } else {
            cascade(s, "term_bank", p[1], true).await?;
        }
        return Ok(json!({"message":"Restored"}).into());
    }
    if !live(&bank) || term.as_ref().map_or(false, |v| !live(v)) {
        return Err(ApiError::not_found("Term bank or term recycled"));
    }
    let perm = if term.is_some() {
        if r.method == Method::DELETE {
            1080
        } else if r.method == Method::GET {
            1030
        } else {
            1070
        }
    } else if p.len() == 3 && p[2] == "terms" {
        if r.method == Method::GET {
            1030
        } else {
            1060
        }
    } else if r.method == Method::GET {
        1030
    } else if r.method == Method::DELETE {
        1050
    } else {
        1040
    };
    if !own {
        require(r, "team", core::field(&bank, "team_id"), perm).await?;
    }
    if term.is_none() && p.len() == 3 && p[2] == "terms" {
        if r.method == Method::GET {
            return Ok(Reply::list(
                s.find("terms", doc! {"term_bank_id":p[1],"deleted_at":null})
                    .await?
                    .into_iter()
                    .filter(|v| matches(v, r.q("word")))
                    .collect(),
                r,
            ));
        }
        if r.method == Method::POST {
            return Ok(insert_child(s,"term_banks",p[1],"terms",json!({"term_bank_id":p[1],"team_id":bank["team_id"],"source":core::required(&r.body,"source")?,"target":core::required(&r.body,"target")?,"tip":r.body["tip"].as_str().unwrap_or(""),"owner_id":uid})).await?.into());
        }
    }
    if p.len() == 2 {
        if r.method == Method::GET {
            return Ok(if let Some(t) = term {
                t
            } else {
                decorate_bank(s, &bank).await?
            }
            .into());
        }
        if r.method == Method::PUT {
            return Ok(if term.is_some(){s.patch("terms",p[1],json!({"source":core::required(&r.body,"source")?,"target":core::required(&r.body,"target")?,"tip":r.body["tip"].as_str().unwrap_or("")})).await?}else{s.patch("term_banks",p[1],bank_fields(s,&r.body).await?).await?}.into());
        }
        if r.method == Method::DELETE {
            if term.is_some() {
                s.patch("terms",p[1],json!({"deleted_at":core::epoch(),"deletion_root_type":"term","deletion_root_id":p[1]})).await?;
            } else {
                cascade(s, "term_bank", p[1], false).await?;
            }
            return Ok(json!({"message":"Recycled"}).into());
        }
    }
    Err(ApiError::bad("Unsupported term route"))
}
async fn insight(r: &Request, team: &str, path: &[&str]) -> Result<Reply> {
    let s = &r.state;
    if !matches!(
        path,
        ["projects"] | ["users"] | ["projects", _, "users"] | ["users", _, "projects"]
    ) {
        return Err(ApiError::bad("Invalid insight route"));
    }
    let projects = s
        .find("projects", doc! {"team_id":team,"deleted_at":null})
        .await?;
    let mut entries = Vec::new();
    if path[0] == "projects" {
        for p in projects {
            if !matches(&p, r.q("word")) || (path.len() == 3 && core::field(&p, "id") != path[1]) {
                continue;
            }
            let users = members(s, "project", core::field(&p, "id")).await?;
            if path.len() == 3 {
                return Ok(Reply::list(users, r));
            }
            let can_download = has(&user_role(s, "project", &p, r.user.as_ref()).await?, 1);
            let mut outputs = Vec::new();
            for mut output in s
                .find(
                    "outputs",
                    doc! {"project_id":core::field(&p,"id"),"deleted_at":null},
                )
                .await?
            {
                if let Some(fields) = output.as_object_mut() {
                    fields.remove("link");
                    fields.remove("user");
                }
                let mut output = crate::jobs::decorate_output(s, &output).await?;
                if !can_download {
                    output["link"] = Value::Null;
                }
                outputs.push(output);
            }
            entries.push(json!({"project":decorate_project(s,&p,r.user.as_ref()).await?,"count":users.len(),"users":users,"outputs":outputs}));
        }
    } else {
        let mut ids = HashSet::new();
        for u in members(s, "team", team).await? {
            ids.insert(core::field(&u, "id").to_string());
        }
        for p in &projects {
            for u in members(s, "project", core::field(p, "id")).await? {
                ids.insert(core::field(&u, "id").to_string());
            }
        }
        let mut ids = ids.into_iter().collect::<Vec<_>>();
        ids.sort();
        for uid in ids {
            if path.len() == 3 && uid != path[1] {
                continue;
            }
            let u = s.get("users", &uid).await?;
            let public = crate::auth::decorate_user(s, &u, false).await?;
            if !matches(&public, r.q("word")) {
                continue;
            }
            let mut list = Vec::new();
            for p in &projects {
                if !user_role(s, "project", p, Some(&u)).await?.is_null() {
                    list.push(decorate_project(s, p, Some(&u)).await?)
                }
            }
            if path.len() == 3 {
                return Ok(Reply::list(list, r));
            }
            entries.push(json!({"user":public,"count":list.len(),"projects":list}));
        }
    }
    if path.len() == 3 {
        return Err(ApiError::not_found("Insight subject is not in this team"));
    }
    Ok(Reply::list(entries, r))
}
fn recycle_item(mut v: Value, kind: &str) -> Value {
    v["group_type"] = json!(kind);
    v["expires_at"] = json!(v["deleted_at"].as_i64().unwrap_or(0) + 604800);
    v
}
async fn insert_child(
    s: &AppState,
    parent_collection: &str,
    parent_id: &str,
    child_collection: &str,
    mut value: Value,
) -> Result<Value> {
    value["id"] = json!(core::id());
    value["created_at"] = json!(core::now());
    let mut session = s.mongo.start_session().await?;
    session.start_transaction().await?;
    let guard =
        s.db.collection::<Document>(parent_collection)
            .update_one(
                doc! {"id":parent_id,"deleted_at":null},
                doc! {"$inc":{"metadata_version":1}},
            )
            .session(&mut session)
            .await?;
    if guard.matched_count != 1 {
        return Err(ApiError::bad("Parent recycled"));
    }
    s.db.collection::<Document>(child_collection)
        .insert_one(
            mongodb::bson::to_document(&value).map_err(|e| ApiError::internal(e.to_string()))?,
        )
        .session(&mut session)
        .await?;
    session.commit_transaction().await?;
    Ok(value)
}
async fn roles(r: &Request, kind: &str, gid: &str, path: &[&str]) -> Result<Reply> {
    let s = &r.state;
    let permission = if r.method == Method::GET {
        1
    } else if r.method == Method::DELETE {
        20
    } else {
        15
    };
    require(r, kind, gid, permission).await?;
    if r.method == Method::GET && path.is_empty() {
        return Ok(Reply::list(group_roles(s, kind, gid).await?, r));
    }
    if !((r.method == Method::POST && path.is_empty())
        || ((r.method == Method::PUT || r.method == Method::DELETE) && path.len() == 1))
    {
        return Err(ApiError::bad("Unsupported role route"));
    }
    let mut session = s.mongo.start_session().await?;
    session.start_transaction().await?;
    let guard =
        s.db.collection::<Document>(collection(kind)?)
            .update_one(
                doc! {"id":gid,"deleted_at":null},
                doc! {"$inc":{"membership_version":1}},
            )
            .session(&mut session)
            .await?;
    if guard.matched_count != 1 {
        return Err(ApiError::bad("Group unavailable"));
    }
    let g = require(r, kind, gid, permission).await?;
    let mine = user_role(s, kind, &g, r.user.as_ref()).await?;
    let old = if path.is_empty() {
        Value::Null
    } else {
        resolve_role(s, kind, gid, path[0]).await?
    };
    if !old.is_null() && (old["system"] == true || level(&old) >= level(&mine)) {
        return Err(ApiError::forbidden("Cannot change this role"));
    }
    if r.method == Method::DELETE {
        if core::field(&g, "default_role") == path[0] {
            return Err(ApiError::bad("Role is default"));
        }
        for c in ["memberships", "invitations", "applications"] {
            if s.db
                .collection::<Document>(c)
                .count_documents(doc! {"group_type":kind,"group_id":gid,"role_id":path[0]})
                .session(&mut session)
                .await?
                > 0
            {
                return Err(ApiError::bad("Role is in use"));
            }
        }
        s.db.collection::<Document>("roles")
            .delete_one(doc! {"id":path[0]})
            .session(&mut session)
            .await?;
        session.commit_transaction().await?;
        return Ok(json!({"message":"Deleted"}).into());
    }
    let name = core::required(&r.body, "name")?;
    let lvl = r.body["level"]
        .as_i64()
        .ok_or_else(|| ApiError::bad("level required"))?;
    if lvl <= 0 || lvl >= level(&mine) {
        return Err(ApiError::forbidden("Invalid role level"));
    }
    let ps = r.body["permissions"]
        .as_array()
        .ok_or_else(|| ApiError::bad("permissions required"))?;
    let mut perms = Vec::new();
    let mut seen = HashSet::new();
    for p in ps {
        let n = p
            .as_i64()
            .or(p["id"].as_i64())
            .ok_or_else(|| ApiError::bad("Invalid permission"))?;
        if !codes(kind).contains(&n) || !has(&mine, n) {
            return Err(ApiError::forbidden("Cannot grant permission"));
        }
        if seen.insert(n) {
            perms.push(json!({"id":n,"name":permission_name(n,kind),"intro":""}));
        }
    }
    let id = if path.is_empty() {
        core::id()
    } else {
        path[0].to_owned()
    };
    let v = json!({"id":id,"created_at":if old.is_null(){json!(core::now())}else{old["created_at"].clone()},"updated_at":core::now(),"name":name,"level":lvl,"permissions":perms,"intro":r.body["intro"].as_str().unwrap_or(""),"group_type":kind,"group_id":gid,"system":false});
    let doc = mongodb::bson::to_document(&v).map_err(|e| ApiError::internal(e.to_string()))?;
    if r.method == Method::POST {
        s.db.collection::<Document>("roles")
            .insert_one(doc)
            .session(&mut session)
            .await?;
    } else {
        s.db.collection::<Document>("roles")
            .replace_one(doc! {"id":&id}, doc)
            .session(&mut session)
            .await?;
    }
    session.commit_transaction().await?;
    Ok(v.into())
}
fn public_team_metadata(team: &Value) -> Value {
    let mut public = json!({});
    for key in [
        "id",
        "name",
        "intro",
        "group_type",
        "avatar",
        "has_avatar",
        "joined",
        "allow_apply_type",
        "application_check_type",
        "is_need_check_application",
        "user_count",
        "max_user",
        "create_time",
        "edit_time",
    ] {
        public[key] = team[key].clone();
    }
    public["role"] = Value::Null;
    public
}
async fn visible_set_projects(r: &Request, set: &Value, team_access: bool) -> Result<Vec<Value>> {
    let projects=r.state.find("projects",doc!{"team_id":core::field(set,"team_id"),"project_set_id":core::field(set,"id"),"deleted_at":null}).await?;
    if team_access {
        return Ok(projects);
    }
    let mut visible = Vec::new();
    for project in projects {
        if has(
            &user_role(&r.state, "project", &project, r.user.as_ref()).await?,
            1,
        ) {
            visible.push(project);
        }
    }
    Ok(visible)
}

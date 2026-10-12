use crate::core::{self, epoch, field, ApiError, AppState, Reply, Request, Result};
use base64::Engine;
use lettre::{
    message::Mailbox, transport::smtp::authentication::Credentials, AsyncSmtpTransport,
    AsyncTransport, Message, Tokio1Executor,
};
use mongodb::{
    bson::{doc, Document},
    options::{IndexOptions, ReturnDocument},
    IndexModel,
};
use serde_json::{json, Value};
use std::{
    io::{Cursor, Read, Write},
    sync::Arc,
    time::Duration,
};
use tokio::sync::watch;
const WEEK: i64 = 604800;
const LEASE: i64 = 120;

fn resources(p: &Value) -> Vec<String> {
    [
        "resource_id",
        "project_resource_id",
        "labelplus_resource_id",
    ]
    .iter()
    .filter_map(|k| {
        p.get(*k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    })
    .collect()
}
fn err(e: impl std::fmt::Display) -> ApiError {
    ApiError::internal(e.to_string())
}
async fn guard_commit(
    s: &AppState,
    j: &Value,
    file_id: &str,
    project_id: &str,
    session: &mut mongodb::ClientSession,
) -> Result<()> {
    let job=s.db.collection::<Document>("jobs").find_one_and_update(doc!{"id":field(j,"id"),"claim_token":field(j,"claim_token"),"status":"running","lease_until":{"$gt":epoch()}},doc!{"$inc":{"mutation_sequence":1_i64}}).return_document(ReturnDocument::After).session(&mut *session).await?.ok_or_else(||ApiError::bad("Job claim expired or was cancelled"))?;
    let job: Value = mongodb::bson::from_document(job).map_err(err)?;
    let p = &job["payload"];
    let kind = field(&job, "kind");
    let owner_id = field(p, "owner_id");
    let owner = if owner_id.is_empty() {
        if !["mail", "gc"].contains(&kind) {
            return Err(ApiError::forbidden("Business job owner is missing"));
        }
        None
    } else {
        let user =
            s.db.collection::<Document>("users")
                .find_one_and_update(
                    doc! {"id":owner_id,"disabled":{"$ne":true}},
                    doc! {"$inc":{"job_permission_sequence":1_i64}},
                )
                .return_document(ReturnDocument::After)
                .session(&mut *session)
                .await?
                .ok_or_else(|| ApiError::forbidden("Job owner is disabled or no longer exists"))?;
        Some(mongodb::bson::from_document::<Value>(user).map_err(err)?)
    };
    let fid = if file_id.is_empty() {
        field(p, "file_id")
    } else {
        file_id
    };
    let mut pid = if project_id.is_empty() {
        field(p, "project_id").to_owned()
    } else {
        project_id.to_owned()
    };
    if !fid.is_empty() {
        let mut filter = doc! {"id":fid,"deleted_at":null,"deletion_state":{"$ne":"deleting"},"safe_status":{"$ne":5}};
        if kind == "ai" {
            filter.insert("activated", doc! {"$ne":false});
        }
        if kind == "ai" || kind == "text_parse" {
            filter.insert("resource_id", field(p, "resource_id"));
        }
        let file =
            s.db.collection::<Document>("files")
                .find_one_and_update(filter, doc! {"$inc":{"job_mutation_sequence":1_i64}})
                .return_document(ReturnDocument::After)
                .session(&mut *session)
                .await?
                .ok_or_else(|| {
                    ApiError::bad("File was recycled, blocked, replaced, or is not active")
                })?;
        let actual = file.get_str("project_id").map_err(err)?;
        if !pid.is_empty() && pid != actual {
            return Err(ApiError::bad("Job file no longer belongs to project"));
        }
        pid = actual.to_owned();
    }
    if pid.is_empty() {
        pid = field(&job, "import_project_id").to_owned();
    }
    if !pid.is_empty() {
        let mut filter = doc! {"id":&pid,"deleted_at":null,"deletion_state":{"$ne":"deleting"}};
        if kind != "export" {
            filter.insert("status", 0_i64);
        }
        let project =
            s.db.collection::<Document>("projects")
                .find_one_and_update(filter, doc! {"$inc":{"job_mutation_sequence":1_i64}})
                .return_document(ReturnDocument::After)
                .session(&mut *session)
                .await?
                .ok_or_else(|| ApiError::bad("Project was recycled or is read-only"))?;
        let team_id = project.get_str("team_id").map_err(err)?;
        let team =
            s.db.collection::<Document>("teams")
                .update_one(
                    doc! {"id":team_id,"deleted_at":null,"deletion_state":{"$ne":"deleting"}},
                    doc! {"$inc":{"job_permission_sequence":1_i64}},
                )
                .session(&mut *session)
                .await?;
        if team.matched_count != 1 {
            return Err(ApiError::forbidden(
                "Project team is recycled or unavailable",
            ));
        }
        if let Ok(set_id) = project.get_str("project_set_id") {
            if !set_id.is_empty() {
                let set=s.db.collection::<Document>("project_sets").update_one(doc!{"id":set_id,"team_id":team_id,"deleted_at":null,"deletion_state":{"$ne":"deleting"}},doc!{"$inc":{"job_permission_sequence":1_i64}}).session(&mut *session).await?;
                if set.matched_count != 1 {
                    return Err(ApiError::forbidden(
                        "Project set is recycled or unavailable",
                    ));
                }
            }
        }
        let user = owner
            .as_ref()
            .ok_or_else(|| ApiError::forbidden("Business job owner is missing"))?;
        let permission = match kind {
            "export" => 1060,
            "ai" | "import" => 1080,
            _ => 1,
        };
        crate::groups::require_user(s, user, "project", &pid, permission).await?;
        if ["ai", "import"].contains(&kind) {
            crate::groups::require_user(s, user, "project", &pid, 1110).await?;
        }
    } else if kind == "import" {
        let team_id = field(p, "team_id");
        let team =
            s.db.collection::<Document>("teams")
                .update_one(
                    doc! {"id":team_id,"deleted_at":null,"deletion_state":{"$ne":"deleting"}},
                    doc! {"$inc":{"job_permission_sequence":1_i64}},
                )
                .session(&mut *session)
                .await?;
        if team.matched_count != 1 {
            return Err(ApiError::forbidden("Import team is unavailable"));
        }
        crate::groups::require_user(
            s,
            owner
                .as_ref()
                .ok_or_else(|| ApiError::forbidden("Import owner is missing"))?,
            "team",
            team_id,
            1090,
        )
        .await?;
    }
    for rid in resources(p) {
        let n =
            s.db.collection::<Document>("resources")
                .update_one(
                    doc! {"id":rid,"state":"bound"},
                    doc! {"$inc":{"job_mutation_sequence":1_i64}},
                )
                .session(&mut *session)
                .await?
                .matched_count;
        if n != 1 {
            return Err(ApiError::bad(
                "Protected job resource is no longer retained",
            ));
        }
    }
    if !field(p, "target_id").is_empty() {
        let n =
            s.db.collection::<Document>("targets")
                .update_one(
                    doc! {"id":field(p,"target_id"),"project_id":&pid,"deleted_at":null},
                    doc! {"$inc":{"job_mutation_sequence":1_i64}},
                )
                .session(&mut *session)
                .await?
                .matched_count;
        if n != 1 {
            return Err(ApiError::bad("Translation target was recycled or moved"));
        }
    }
    Ok(())
}
async fn finish_in_session(
    s: &AppState,
    j: &Value,
    result: &Value,
    session: &mut mongodb::ClientSession,
) -> Result<()> {
    let n=s.db.collection::<Document>("jobs").update_one(doc!{"id":field(j,"id"),"status":"running","claim_token":field(j,"claim_token")},doc!{"$set":{"status":"succeeded","result":mongodb::bson::to_bson(result).map_err(err)?,"progress":100_i64,"finished_epoch":epoch()},"$unset":{"claim_token":"","lease_until":""}}).session(&mut *session).await?.matched_count;
    if n != 1 {
        return Err(ApiError::bad("Job was cancelled before result commit"));
    }
    Ok(())
}
async fn guarded_patch(
    s: &AppState,
    j: &Value,
    collection: &str,
    id: &str,
    fields: Value,
    result: Option<&Value>,
) -> Result<()> {
    let mut session = s.mongo.start_session().await?;
    session.start_transaction().await?;
    guard_commit(
        s,
        j,
        if collection == "files" { id } else { "" },
        if collection == "projects" {
            id
        } else {
            field(&fields, "import_project_id")
        },
        &mut session,
    )
    .await?;
    let n =
        s.db.collection::<Document>(collection)
            .update_one(
                doc! {"id":id},
                doc! {"$set":mongodb::bson::to_document(&fields).map_err(err)?},
            )
            .session(&mut session)
            .await?
            .matched_count;
    if n != 1 {
        return Err(ApiError::not_found("Job business record disappeared"));
    }
    if let Some(result) = result {
        finish_in_session(s, j, result, &mut session).await?;
    }
    session.commit_transaction().await?;
    Ok(())
}
async fn retire_terminal_refs(s: &AppState, id: &str) -> Result<()> {
    if s.db
        .collection::<Document>("jobs")
        .count_documents(doc! {"id":id,"status":{"$in":["succeeded","failed","cancelled"]}})
        .await?
        == 0
    {
        return Ok(());
    }
    for reference in s
        .find("resource_refs", doc! {"kind":"job","target_id":id})
        .await?
    {
        crate::storage::retire(
            s,
            field(&reference, "resource_id"),
            "job",
            id,
            crate::storage::RetirementPolicy::RecoveryGrace,
        )
        .await?;
    }
    Ok(())
}
async fn commit_output(
    s: &AppState,
    j: &Value,
    output_id: &str,
    resource: &Value,
) -> Result<Value> {
    let mut session = s.mongo.start_session().await?;
    session.start_transaction().await?;
    guard_commit(s, j, "", "", &mut session).await?;
    let current =
        s.db.collection::<Document>("jobs")
            .find_one(doc! {"id":field(j,"id")})
            .session(&mut session)
            .await?
            .ok_or_else(|| ApiError::bad("Job disappeared"))?;
    let current: Value = mongodb::bson::from_document(current).map_err(err)?;
    if let Some(snapshot) = current["export_snapshot"].as_array() {
        for file in snapshot {
            let n=s.db.collection::<Document>("files").update_one(doc!{"id":field(file,"file_id"),"resource_id":mongodb::bson::to_bson(&file["resource_id"]).map_err(err)?,"deleted_at":null,"activated":{"$ne":false},"safe_status":{"$ne":5}},doc!{"$inc":{"job_mutation_sequence":1_i64}}).session(&mut session).await?.matched_count;
            if n != 1 {
                return Err(ApiError::bad(
                    "Export file was recycled, blocked, or replaced before commit",
                ));
            }
        }
    }
    crate::storage::attach_in_session(s, field(resource, "id"), "output", output_id, &mut session)
        .await?;
    s.db.collection::<Document>("resources")
        .update_one(
            doc! {"id":field(resource,"id"),"state":"bound"},
            doc! {"$set":{"expires_at":epoch()+WEEK}},
        )
        .session(&mut session)
        .await?;
    let changed=s.db.collection::<Document>("outputs").update_one(doc!{"id":output_id,"deleted_at":null,"state":{"$ne":"expired"}},doc!{"$set":{"resource_id":field(resource,"id"),"expires_at":epoch()+WEEK,"status":2_i64,"state":"available"}}).session(&mut session).await?;
    if changed.matched_count != 1 {
        return Err(ApiError::bad("Output was recycled or expired"));
    }
    let result = json!({"resource_id":resource["id"]});
    finish_in_session(s, j, &result, &mut session).await?;
    session.commit_transaction().await?;
    Ok(result)
}
async fn upsert(s: &AppState, j: &Value, c: &str, id: &str, v: Value) -> Result<()> {
    let mut session = s.mongo.start_session().await?;
    session.start_transaction().await?;
    guard_commit(
        s,
        j,
        if c == "sources" {
            field(&v, "file_id")
        } else {
            ""
        },
        field(&v, "project_id"),
        &mut session,
    )
    .await?;
    if c == "translations" {
        let source =
            s.db.collection::<Document>("sources")
                .find_one_and_update(
                    doc! {"id":field(&v,"source_id"),"deleted_at":null},
                    doc! {"$inc":{"job_mutation_sequence":1_i64}},
                )
                .return_document(ReturnDocument::After)
                .session(&mut session)
                .await?
                .ok_or_else(|| ApiError::bad("Source was deleted during job"))?;
        let fid = source.get_str("file_id").map_err(err)?;
        guard_commit(s, j, fid, "", &mut session).await?;
        let file =
            s.db.collection::<Document>("files")
                .find_one(doc! {"id":fid})
                .session(&mut session)
                .await?
                .ok_or_else(|| ApiError::bad("Source file disappeared"))?;
        let n=s.db.collection::<Document>("targets").update_one(doc!{"id":field(&v,"target_id"),"project_id":file.get_str("project_id").map_err(err)?,"deleted_at":null},doc!{"$inc":{"job_mutation_sequence":1_i64}}).session(&mut session).await?.matched_count;
        if n != 1 {
            return Err(ApiError::bad("Import translation target was recycled"));
        }
    }
    let mut d = mongodb::bson::to_document(&v).map_err(err)?;
    d.insert("id", id);
    if !d.contains_key("created_at") {
        d.insert("created_at", core::now());
    }
    if c == "files" {
        d.insert("name_key", field(&v, "name").to_lowercase());
        d.insert("deleted_at", mongodb::bson::Bson::Null);
        d.insert(
            "safe_status",
            if v["type"].as_i64() == Some(1) { 4 } else { 3 },
        );
        d.insert("parse_status", 4);
        if !d.contains_key("revision_family_id") {
            d.insert("revision_family_id", id);
        }
        if !d.contains_key("revision") {
            d.insert("revision", 1);
        }
    }
    if c == "files" && !field(&v, "resource_id").is_empty() {
        crate::storage::attach_in_session(s, field(&v, "resource_id"), "file", id, &mut session)
            .await?;
    }
    let filter = if c == "translations" {
        doc! {"source_id":field(&v,"source_id"),"target_id":field(&v,"target_id"),"user_id":field(&v,"user_id")}
    } else {
        doc! {"id":id}
    };
    s.db.collection::<Document>(c)
        .update_one(filter, doc! {"$setOnInsert":d})
        .upsert(true)
        .session(&mut session)
        .await?;
    session.commit_transaction().await?;
    Ok(())
}
async fn snapshot_export_file(s: &AppState, j: &Value, file: &Value) -> Result<()> {
    let mut session = s.mongo.start_session().await?;
    session.start_transaction().await?;
    let fid = field(file, "id");
    guard_commit(s, j, fid, field(file, "project_id"), &mut session).await?;
    let expected = mongodb::bson::to_bson(&file["resource_id"]).map_err(err)?;
    let n =
        s.db.collection::<Document>("files")
            .update_one(
                doc! {"id":fid,"resource_id":expected,"activated":{"$ne":false}},
                doc! {"$inc":{"job_mutation_sequence":1_i64}},
            )
            .session(&mut session)
            .await?
            .matched_count;
    if n != 1 {
        return Err(ApiError::bad("Export file was replaced during collection"));
    }
    if j["payload"]["type"].as_i64().unwrap_or(0) == 0 && !field(file, "resource_id").is_empty() {
        crate::storage::attach_in_session(
            s,
            field(file, "resource_id"),
            "job",
            field(j, "id"),
            &mut session,
        )
        .await?;
    }
    s.db.collection::<Document>("jobs").update_one(doc!{"id":field(j,"id")},doc!{"$addToSet":{"export_snapshot":{"file_id":fid,"resource_id":mongodb::bson::to_bson(&file["resource_id"]).map_err(err)?}}}).session(&mut session).await?;
    session.commit_transaction().await?;
    Ok(())
}
async fn reconcile_text(s: &AppState, file: &Value) -> Result<()> {
    let fid = field(file, "id");
    let key = format!("text_parse:{fid}");
    let mut session = s.mongo.start_session().await?;
    session.start_transaction().await?;
    let project=s.db.collection::<Document>("projects").update_one(doc!{"id":field(file,"project_id"),"status":0_i64,"deleted_at":null,"deletion_state":{"$ne":"deleting"}},doc!{"$inc":{"job_recovery_sequence":1_i64}}).session(&mut session).await?;
    let retained=s.db.collection::<Document>("files").update_one(doc!{"id":fid,"resource_id":field(file,"resource_id"),"type":3_i64,"parse_status":1_i64,"deleted_at":null,"deletion_state":{"$ne":"deleting"}},doc!{"$inc":{"job_recovery_sequence":1_i64}}).session(&mut session).await?;
    if project.matched_count != 1 || retained.matched_count != 1 {
        session.abort_transaction().await?;
        return Ok(());
    }
    let job=s.db.collection::<Document>("jobs").find_one_and_update(doc!{"kind":"text_parse","dedup_key":&key,"status":"cancelled","cancel_reason":{"$in":["project_finished","group_recycled","file_recycled"]}},doc!{"$set":{"status":"queued","attempts":0_i64,"available_at":epoch(),"progress":0_i64},"$unset":{"cancel_reason":"","finished_epoch":"","last_error":"","claim_token":"","lease_until":""}}).return_document(ReturnDocument::After).session(&mut session).await?;
    if let Some(job) = job {
        crate::storage::attach_in_session(
            s,
            field(file, "resource_id"),
            "job",
            job.get_str("id").map_err(err)?,
            &mut session,
        )
        .await?;
    }
    session.commit_transaction().await?;
    enqueue(s,"text_parse",json!({"file_id":fid,"resource_id":file["resource_id"],"project_id":file["project_id"],"owner_id":file["owner_id"]}),Some(&key)).await?;
    Ok(())
}

pub async fn enqueue(
    s: &AppState,
    kind: &str,
    payload: Value,
    dedup_key: Option<&str>,
) -> Result<Value> {
    if !["mail", "text_parse", "export", "import", "ai", "gc"].contains(&kind) {
        return Err(ApiError::bad("Unknown job kind"));
    }
    let id = core::id();
    let key = dedup_key.map(str::to_owned).unwrap_or_else(|| id.clone());
    let c = s.db.collection::<Document>("jobs");
    let d = doc! {"id":&id,"kind":kind,"payload":mongodb::bson::to_bson(&payload).map_err(err)?,"dedup_key":&key,"status":"preparing","attempts":0_i64,"max_attempts":5_i64,"available_at":epoch(),"created_at":core::now(),"created_epoch":epoch(),"progress":0_i64};
    let job = c
        .find_one_and_update(doc! {"kind":kind,"dedup_key":&key}, doc! {"$setOnInsert":d})
        .upsert(true)
        .return_document(ReturnDocument::After)
        .await?
        .ok_or_else(|| err("Cannot enqueue job"))?;
    let job: Value = mongodb::bson::from_document(job).map_err(err)?;
    if field(&job, "status") == "preparing" {
        for rid in resources(&payload) {
            crate::storage::attach(s, &rid, "job", field(&job, "id")).await?;
        }
        c.update_one(
            doc! {"id":field(&job,"id"),"status":"preparing"},
            doc! {"$set":{"status":"queued"}},
        )
        .await?;
    }
    s.get("jobs", field(&job, "id")).await
}

pub async fn run(s: Arc<AppState>, mut shutdown: watch::Receiver<bool>) {
    let index = IndexModel::builder()
        .keys(doc! {"kind":1,"dedup_key":1})
        .options(IndexOptions::builder().unique(true).build())
        .build();
    if let Err(e) =
        s.db.collection::<Document>("jobs")
            .create_index(index)
            .await
    {
        tracing::error!("job index: {e}");
        return;
    }
    let concurrency = core::env("JOB_CONCURRENCY", "4")
        .parse::<usize>()
        .unwrap_or(4)
        .clamp(1, 32);
    let heavy_limit = core::env("JOB_HEAVY_CONCURRENCY", "1")
        .parse::<usize>()
        .unwrap_or(1)
        .clamp(1, concurrency.saturating_sub(1).max(1));
    let mut heavy_workers = tokio::task::JoinSet::new();
    let mut light_workers = tokio::task::JoinSet::new();
    let mut ticker = tokio::time::interval(Duration::from_secs(2));
    let mut next_gc = 0;
    let mut next_cleanup = 0;
    if core::env("GC_ENABLED", "false") != "true"
        || core::env("RESOURCE_BACKUP_CONFIRMED", "false") != "true"
    {
        tracing::warn!("Destructive business GC paused until GC_ENABLED and RESOURCE_BACKUP_CONFIRMED; unreferenced temporary cleanup requires GC_ENABLED");
    }
    loop {
        tokio::select! { changed=shutdown.changed()=>{if changed.is_err()||*shutdown.borrow(){break}},_=ticker.tick()=>{
        if epoch()>=next_gc {let _=enqueue(&s,"gc",json!({}),Some(&format!("gc:{}",epoch()/3600))).await;next_gc=epoch()+3600;}
        let c=s.db.collection::<Document>("jobs");
        if epoch()>=next_cleanup{if let Ok(refs)=s.find("resource_refs",doc!{"kind":"job"}).await{for reference in refs{let _=retire_terminal_refs(&s,field(&reference,"target_id")).await;}}next_cleanup=epoch()+60;}
        // Crash recovery: preparing jobs retain their refs and are completed before becoming visible.
        if let Ok(jobs)=s.find("jobs",doc!{"status":"preparing"}).await {for j in jobs {let _=enqueue(&s,field(&j,"kind"),j["payload"].clone(),Some(field(&j,"dedup_key"))).await;}}
        if let Ok(files)=s.find("files",doc!{"type":3,"parse_status":1,"deleted_at":null}).await{for file in files{if !field(&file,"resource_id").is_empty(){let _=reconcile_text(&s,&file).await;}}}
        let _=c.update_many(doc!{"status":"running","lease_until":{"$lte":epoch()},"attempts":{"$gte":5}},doc!{"$set":{"status":"failed","last_error":"Worker lease expired after maximum attempts","finished_epoch":epoch()}}).await;
        if let Ok(failed)=s.find("jobs",doc!{"status":"failed","last_error":"Worker lease expired after maximum attempts"}).await{for job in failed{
        if field(&job,"kind")=="export"{let _=s.patch("outputs",field(&job["payload"],"output_id"),json!({"status":3,"last_error":job["last_error"]})).await;}
        if field(&job,"kind")=="import"{let pid=if field(&job,"import_project_id").is_empty(){field(&job["payload"],"project_id")}else{field(&job,"import_project_id")};if !pid.is_empty(){let _=s.patch("projects",pid,json!({"import_from_labelplus_status":3})).await;}}
        }}
        while heavy_workers.len()+light_workers.len()<concurrency {
        let mut claimed=false;
        for heavy in [true,false]{
        if heavy&&heavy_workers.len()>=heavy_limit{continue}
        let token=core::id();let mut filter=doc!{"$or":[{"status":"queued","available_at":{"$lte":epoch()}},{"status":"running","lease_until":{"$lte":epoch()}}],"attempts":{"$lt":5}};
        filter.insert("kind",if heavy{doc!{"$in":["export","import","ai","text_parse"]}}else{doc!{"$in":["mail","gc"]}});
        if let Ok(Some(d))=c.find_one_and_update(filter,doc!{"$set":{"status":"running","claim_token":&token,"lease_until":epoch()+LEASE},"$inc":{"attempts":1_i64}}).sort(doc!{"available_at":1}).return_document(ReturnDocument::After).await{
        let state=s.clone();let worker=async move{if let Ok(j)=mongodb::bson::from_document::<Value>(d){let _=execute(state,j,token).await;}};
        if heavy{heavy_workers.spawn(worker);}else{light_workers.spawn(worker);}claimed=true;break;
        }
        }
        if !claimed{break}
        }
        }, Some(_)=heavy_workers.join_next(),if !heavy_workers.is_empty()=>{}, Some(_)=light_workers.join_next(),if !light_workers.is_empty()=>{} }
    }
    // Abort in-flight work; persisted leases are reclaimed by the next process.
    heavy_workers.abort_all();
    light_workers.abort_all();
    while heavy_workers.join_next().await.is_some() {}
    while light_workers.join_next().await.is_some() {}
}

async fn execute(s: Arc<AppState>, j: Value, token: String) -> Result<()> {
    let id = field(&j, "id").to_owned();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(25));
    let work = perform(&s, &j);
    tokio::pin!(work);
    let result = loop {
        tokio::select! {r=&mut work=>break r,_=heartbeat.tick()=>{
        let n=s.db.collection::<Document>("jobs").update_one(doc!{"id":&id,"status":"running","claim_token":&token},doc!{"$set":{"lease_until":epoch()+LEASE}}).await?.matched_count;
        if n==0{retire_terminal_refs(&s,&id).await?;return Ok(())}
        }}
    };
    let attempts = j["attempts"].as_i64().unwrap_or(1);
    let (status, patch) = match result {
        Ok(v) => (
            "succeeded",
            doc! {"status":"succeeded","result":mongodb::bson::to_bson(&v).map_err(err)?,"progress":100,"finished_epoch":epoch()},
        ),
        Err(e) => {
            let status = if attempts >= 5
                || (e.status.is_client_error()
                    && e.status != axum::http::StatusCode::TOO_MANY_REQUESTS
                    && e.status != axum::http::StatusCode::REQUEST_TIMEOUT
                    && e.status != axum::http::StatusCode::CONFLICT)
            {
                "failed"
            } else {
                "queued"
            };
            (
                status,
                doc! {"status":status,"last_error":e.to_string(),"available_at":epoch()+(5_i64*2_i64.pow(attempts as u32)).min(3600),"finished_epoch":epoch()},
            )
        }
    };
    let mut session = s.mongo.start_session().await?;
    session.start_transaction().await?;
    let changed = s
        .db
        .collection::<Document>("jobs")
        .find_one_and_update(
            doc! {"id":&id,"status":"running","claim_token":&token,"lease_until":{"$gt":epoch()}},
            doc! {"$set":patch,"$unset":{"claim_token":"","lease_until":""}},
        )
        .return_document(ReturnDocument::After)
        .session(&mut session)
        .await?;
    if let Some(current) = changed {
        let current: Value = mongodb::bson::from_document(current).map_err(err)?;
        if field(&j, "kind") == "export" {
            s.db.collection::<Document>("outputs").update_one(doc!{"id":field(&j["payload"],"output_id"),"deleted_at":null,"state":{"$ne":"expired"}},doc!{"$set":{"status":if status=="succeeded"{2_i64}else if status=="failed"{3_i64}else{0_i64},"last_error":mongodb::bson::to_bson(&current["last_error"]).map_err(err)?}}).session(&mut session).await?;
        }
        if field(&j, "kind") == "import" {
            let pid = if field(&current, "import_project_id").is_empty() {
                field(&j["payload"], "project_id")
            } else {
                field(&current, "import_project_id")
            };
            if !pid.is_empty() {
                s.db.collection::<Document>("projects").update_one(doc!{"id":pid,"status":0_i64,"deleted_at":null},doc!{"$set":{"import_from_labelplus_status":if status=="succeeded"{2_i64}else if status=="failed"{3_i64}else{0_i64}}}).session(&mut session).await?;
            }
        }
    }
    session.commit_transaction().await?;
    retire_terminal_refs(&s, &id).await?;
    Ok(())
}

async fn perform(s: &AppState, j: &Value) -> Result<Value> {
    let p = &j["payload"];
    let kind = field(j, "kind");
    if !["mail", "gc"].contains(&kind) {
        let owner_id = field(p, "owner_id");
        if owner_id.is_empty() {
            return Err(ApiError::forbidden("Business job owner is missing"));
        }
        let user = s.get("users", owner_id).await?;
        if user["disabled"].as_bool() == Some(true) {
            return Err(ApiError::forbidden("Job owner is disabled"));
        }
        if !field(p, "project_id").is_empty() {
            let permission = match kind {
                "export" => 1060,
                "ai" | "import" => 1080,
                _ => 1,
            };
            let project = crate::groups::require_user(
                s,
                &user,
                "project",
                field(p, "project_id"),
                permission,
            )
            .await?;
            if kind == "text_parse" && project["status"].as_i64() != Some(0) {
                return Err(ApiError::forbidden(
                    "Finished projects are read-only for text parsing",
                ));
            }
            if ["ai", "import"].contains(&kind) {
                crate::groups::require_user(s, &user, "project", field(p, "project_id"), 1110)
                    .await?;
            }
        } else if kind == "import" {
            crate::groups::require_user(s, &user, "team", field(p, "team_id"), 1090).await?;
        }
    }
    match field(j, "kind") {
        "mail" => send_mail(p).await,
        "text_parse" => parse_text(s, j).await,
        "export" => export(s, j).await,
        "import" => import(s, j).await,
        "ai" => ai(s, j).await,
        "gc" => gc(s).await,
        _ => Err(ApiError::bad("Unsupported job")),
    }
}
async fn send_mail(p: &Value) -> Result<Value> {
    let host = core::env("SMTP_HOST", "");
    let from = core::env("SMTP_FROM", "");
    if host.is_empty() || from.is_empty() {
        return Err(err("SMTP_HOST and SMTP_FROM are required"));
    }
    let to = field(p, "to").parse::<Mailbox>().map_err(err)?;
    let body = p
        .get("body")
        .or_else(|| p.get("text"))
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad("Mail body missing"))?;
    let msg = Message::builder()
        .from(from.parse().map_err(err)?)
        .to(to)
        .subject(field(p, "subject"))
        .body(body.to_owned())
        .map_err(err)?;
    let tls = core::env("SMTP_TLS", "starttls");
    let builder = match tls.as_str() {
        "none" => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host),
        "tls" => AsyncSmtpTransport::<Tokio1Executor>::relay(&host).map_err(err)?,
        "starttls" => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&host).map_err(err)?,
        _ => return Err(err("SMTP_TLS must be none, tls, or starttls")),
    };
    let port = core::env("SMTP_PORT", if tls == "tls" { "465" } else { "587" })
        .parse::<u16>()
        .map_err(err)?;
    let mut builder = builder.port(port).timeout(Some(Duration::from_secs(60)));
    let user = core::env("SMTP_USERNAME", "");
    if !user.is_empty() {
        builder = builder.credentials(Credentials::new(user, core::env("SMTP_PASSWORD", "")));
    }
    builder.build().send(msg).await.map_err(err)?;
    Ok(json!({"sent":true}))
}
async fn bytes(s: &AppState, rid: &str, max_bytes: u64) -> Result<Vec<u8>> {
    let r = s.get("resources", rid).await?;
    if ["deleted", "deleting", "uploading"].contains(&field(&r, "state")) {
        return Err(ApiError::bad("Resource is not available"));
    }
    let size = r["size"]
        .as_u64()
        .ok_or_else(|| ApiError::bad("Resource size is invalid"))?;
    if size > max_bytes {
        return Err(ApiError::bad(format!(
            "Resource exceeds worker byte limit ({max_bytes} bytes)"
        )));
    }
    let data = s
        .cos
        .get_bounded(&s.http, field(&r, "key"), max_bytes)
        .await?;
    if data.len() as u64 != size {
        return Err(ApiError::bad("Resource bytes do not match confirmed size"));
    }
    Ok(data)
}
async fn bounded_ai_body(mut response: reqwest::Response) -> Result<Vec<u8>> {
    const MAX: usize = 8 * 1024 * 1024;
    if response
        .content_length()
        .is_some_and(|size| size > MAX as u64)
    {
        return Err(ApiError::bad("AI response exceeds 8 MiB"));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > MAX - body.len() {
            return Err(ApiError::bad("AI response exceeds 8 MiB"));
        }
        body.try_reserve(chunk.len()).map_err(err)?;
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
async fn parse_text(s: &AppState, j: &Value) -> Result<Value> {
    let p = &j["payload"];
    let fid = field(p, "file_id");
    let file = s.get("files", fid).await?;
    let data = bytes(s, field(p, "resource_id"), 16 * 1024 * 1024).await?;
    let lines = tokio::task::spawn_blocking(move || -> Result<Vec<String>> {
        let text =
            String::from_utf8(data).map_err(|_| ApiError::bad("Text resource is not UTF-8"))?;
        let lines = text
            .trim_start_matches('\u{feff}')
            .lines()
            .filter(|line| !line.trim().is_empty())
            .take(100001)
            .collect::<Vec<_>>();
        if lines.len() > 100000 {
            return Err(ApiError::bad("Text resource exceeds 100000 nonempty lines"));
        }
        Ok(lines.into_iter().map(str::to_owned).collect())
    })
    .await
    .map_err(err)??;
    let mut previous: std::collections::HashMap<String, std::collections::VecDeque<Value>> =
        std::collections::HashMap::new();
    let previous_id = field(&file, "previous_revision_id");
    if !previous_id.is_empty() {
        let prior = s.get("files", previous_id).await?;
        if field(&prior, "project_id") != field(&file, "project_id")
            || field(&prior, "revision_family_id") != field(&file, "revision_family_id")
            || prior["type"].as_i64() != Some(3)
            || prior.get("deleted_at").is_some_and(|at| !at.is_null())
        {
            return Err(ApiError::bad(
                "Previous text revision is not retained in this file family",
            ));
        }
        let mut sources = s
            .find("sources", doc! {"file_id":previous_id,"deleted_at":null})
            .await?;
        sources.sort_by(|a, b| {
            a["rank"]
                .as_i64()
                .unwrap_or(0)
                .cmp(&b["rank"].as_i64().unwrap_or(0))
                .then_with(|| field(a, "id").cmp(field(b, "id")))
        });
        for source in sources {
            previous
                .entry(field(&source, "content").to_owned())
                .or_default()
                .push_back(source);
        }
    }
    let targets = s
        .find(
            "targets",
            doc! {"project_id":field(&file,"project_id"),"deleted_at":null},
        )
        .await?;
    let live_targets: std::collections::HashSet<String> = targets
        .iter()
        .map(|target| field(target, "id").to_owned())
        .collect();
    let mut inherited_translations = 0_usize;
    for (i, line) in lines.iter().enumerate() {
        let sid = format!("{}:line:{}", fid, i);
        upsert(s,j,"sources",&sid,json!({"file_id":fid,"content":line,"rank":i,"x":0.0,"y":0.0,"position_type":1,"created_at":core::now()})).await?;
        if let Some(source) = previous
            .get_mut(line)
            .and_then(std::collections::VecDeque::pop_front)
        {
            for mut translation in s
                .find(
                    "translations",
                    doc! {"source_id":field(&source,"id"),"deleted_at":null},
                )
                .await?
            {
                if !live_targets.contains(field(&translation, "target_id")) {
                    continue;
                }
                let inherited_id = format!("{}:inherit:{}", sid, field(&translation, "id"));
                let fields = translation
                    .as_object_mut()
                    .ok_or_else(|| err("Invalid retained translation record"))?;
                for key in ["id", "_id"] {
                    fields.remove(key);
                }
                fields.insert("source_id".into(), json!(sid));
                upsert(s, j, "translations", &inherited_id, translation).await?;
                inherited_translations += 1;
            }
        }
    }
    let result = json!({"file_id":fid,"source_count":lines.len(),"inherited_translation_count":inherited_translations});
    guarded_patch(
        s,
        j,
        "files",
        fid,
        json!({"parse_status":4,"source_count":lines.len()}),
        Some(&result),
    )
    .await?;
    Ok(result)
}

#[derive(Clone)]
struct LpFile {
    name: String,
    labels: Vec<Value>,
}
fn labelplus(text: &str) -> Result<Vec<LpFile>> {
    let mut files: Vec<LpFile> = Vec::new();
    for line in text.trim_start_matches('\u{feff}').lines() {
        if let Some(a) = line.find(">>>[") {
            let b = line[a + 4..]
                .find("]<<<")
                .ok_or_else(|| ApiError::bad("Malformed LabelPlus file marker"))?;
            let name = &line[a + 4..a + 4 + b];
            safe_path(name)?;
            files.push(LpFile {
                name: name.to_owned(),
                labels: vec![],
            });
        } else if line.contains("---[") {
            let a = line
                .rfind('[')
                .ok_or_else(|| ApiError::bad("Malformed LabelPlus label"))?;
            let b = line[a..]
                .find(']')
                .ok_or_else(|| ApiError::bad("Malformed LabelPlus coordinate"))?;
            let coords = line[a + 1..a + b].split(',').collect::<Vec<_>>();
            if coords.len() != 3 {
                return Err(ApiError::bad("Malformed LabelPlus coordinate"));
            }
            let x = coords[0]
                .trim()
                .parse::<f64>()
                .map_err(|_| ApiError::bad("Malformed LabelPlus numeric coordinate"))?;
            let y = coords[1]
                .trim()
                .parse::<f64>()
                .map_err(|_| ApiError::bad("Malformed LabelPlus numeric coordinate"))?;
            let group = coords[2]
                .trim()
                .parse::<i64>()
                .map_err(|_| ApiError::bad("Malformed LabelPlus numeric coordinate"))?;
            if !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) || !(1..=2).contains(&group) {
                return Err(ApiError::bad("Invalid LabelPlus coordinate"));
            }
            files
                .last_mut()
                .ok_or_else(|| ApiError::bad("Label before file"))?
                .labels
                .push(json!({"x":x,"y":y,"position_type":group,"content":""}));
        } else if let Some(v) = files.last_mut().and_then(|f| f.labels.last_mut()) {
            let content = v["content"].as_str().unwrap_or("");
            v["content"] = json!(format!(
                "{}{}{}",
                content,
                if content.is_empty() { "" } else { "\n" },
                line
            ));
        }
    }
    if files.is_empty() {
        return Err(ApiError::bad("No files in LabelPlus input"));
    }
    Ok(files)
}
fn safe_path(name: &str) -> Result<()> {
    if name.is_empty()
        || name.starts_with('/')
        || name.contains('\\')
        || name.contains(':')
        || name.split('/').any(|s| s == ".." || s == ".")
    {
        return Err(ApiError::bad("Unsafe archive path"));
    }
    Ok(())
}
fn unzip(data: Vec<u8>) -> Result<Vec<(String, Vec<u8>)>> {
    const MAX: u64 = 512 * 1024 * 1024;
    let mut zip = zip::ZipArchive::new(Cursor::new(data))
        .map_err(|e| ApiError::bad(format!("Invalid ZIP: {e}")))?;
    if zip.len() > 10000 {
        return Err(ApiError::bad("Too many ZIP entries"));
    }
    let mut entries = Vec::new();
    let mut total = 0_u64;
    let mut names = std::collections::HashSet::new();
    let mut chunk = [0_u8; 32768];
    for i in 0..zip.len() {
        let mut f = zip
            .by_index(i)
            .map_err(|e| ApiError::bad(format!("Invalid ZIP entry: {e}")))?;
        if f.is_dir() {
            continue;
        }
        safe_path(f.name())?;
        let name = f.name().to_owned();
        if !names.insert(name.clone()) {
            return Err(ApiError::bad("Duplicate ZIP entry name"));
        }
        let declared = f.size();
        if declared > MAX - total {
            return Err(ApiError::bad("Expanded ZIP exceeds 512 MiB"));
        }
        let mut out = Vec::new();
        let mut actual = 0_u64;
        loop {
            let count = f
                .read(&mut chunk)
                .map_err(|e| ApiError::bad(format!("Invalid ZIP payload: {e}")))?;
            if count == 0 {
                break;
            }
            let count = count as u64;
            if count > MAX - total || count > declared.saturating_sub(actual) {
                return Err(ApiError::bad(
                    "ZIP expanded bytes exceed declared or aggregate size",
                ));
            }
            total += count;
            actual += count;
            out.extend_from_slice(&chunk[..count as usize]);
        }
        if actual != declared {
            return Err(ApiError::bad("ZIP declared and actual sizes differ"));
        }
        if name != "project.json" && name != "translations.txt" {
            crate::storage::validate_bytes(&name, &out, mime(&name), "file")?;
        }
        entries.push((name, out));
    }
    Ok(entries)
}
fn zip_entries(entries: Vec<(String, Vec<u8>)>) -> Result<Vec<u8>> {
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in entries {
        safe_path(&name)?;
        z.start_file(
            name,
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated),
        )
        .map_err(err)?;
        z.write_all(&data).map_err(err)?;
    }
    Ok(z.finish().map_err(err)?.into_inner())
}
async fn progress(s: &AppState, j: &Value, value: i64) -> Result<()> {
    let changed =
        s.db.collection::<Document>("jobs")
            .update_one(
                doc! {"id":field(j,"id"),"claim_token":field(j,"claim_token"),"status":"running"},
                doc! {"$set":{"progress":value}},
            )
            .await?;
    if changed.matched_count == 0 {
        return Err(err("Job claim lost or cancelled"));
    }
    Ok(())
}
async fn path(s: &AppState, file: &Value) -> Result<String> {
    let mut parts = vec![field(file, "name").to_owned()];
    let mut parent = field(file, "parent_id").to_owned();
    for _ in 0..128 {
        if parent.is_empty() {
            break;
        }
        let f = s.get("files", &parent).await?;
        parts.push(field(&f, "name").to_owned());
        parent = field(&f, "parent_id").to_owned();
    }
    if !parent.is_empty() {
        return Err(ApiError::bad("File parent cycle"));
    }
    parts.reverse();
    let name = parts.join("/");
    safe_path(&name)?;
    Ok(name)
}
fn export_limit(name: &str) -> ApiError {
    ApiError::bad(format!(
        "Export exceeds {name}; choose a narrower file_ids_include filter"
    ))
}
fn export_setting(name: &str, default: &str) -> Result<u64> {
    let value = core::env(name, default).parse::<u64>().map_err(|_| {
        ApiError::unavailable(format!("{name} must be a positive byte/count limit"))
    })?;
    if value == 0 || usize::try_from(value).is_err() {
        return Err(ApiError::unavailable(format!(
            "{name} must be a positive limit supported by this server"
        )));
    }
    Ok(value)
}
struct ExportJsonCounter {
    used: u64,
    limit: u64,
}
impl Write for ExportJsonCounter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let next = self
            .used
            .checked_add(data.len() as u64)
            .filter(|size| *size <= self.limit)
            .ok_or_else(|| std::io::Error::other("Export JSON exceeds input byte budget"))?;
        self.used = next;
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn export_metadata_budget(
    counter: &mut ExportJsonCounter,
    input: u64,
    resources: u64,
    text: usize,
) -> Result<()> {
    counter.limit = input
        .checked_sub(resources)
        .and_then(|remaining| remaining.checked_sub(text as u64))
        .ok_or_else(|| export_limit("MAX_EXPORT_INPUT_BYTES"))?;
    if counter.used > counter.limit {
        return Err(export_limit("MAX_EXPORT_INPUT_BYTES"));
    }
    Ok(())
}
fn append_export_text(text: &mut String, value: &str, limit: u64) -> Result<()> {
    for chunk in value.split_inclusive('\n') {
        let (body, newline) = if chunk.ends_with('\n') && !chunk.ends_with("\r\n") {
            (&chunk[..chunk.len() - 1], "\r\n")
        } else {
            (chunk, "")
        };
        let added = body
            .len()
            .checked_add(newline.len())
            .ok_or_else(|| export_limit("MAX_EXPORT_TEXT_BYTES"))?;
        if added as u64 > limit.saturating_sub(text.len() as u64) {
            return Err(export_limit("MAX_EXPORT_TEXT_BYTES"));
        }
        text.try_reserve(added).map_err(err)?;
        text.push_str(body);
        text.push_str(newline);
    }
    Ok(())
}
async fn export(s: &AppState, j: &Value) -> Result<Value> {
    let p = &j["payload"];
    let output_id = field(p, "output_id");
    let output = s.get("outputs", output_id).await?;
    if let Some(rid) = output["resource_id"].as_str() {
        let resource = s.get("resources", rid).await?;
        if field(&resource, "state") == "bound" {
            return commit_output(s, j, output_id, &resource).await;
        }
    }
    let max_input = export_setting("MAX_EXPORT_INPUT_BYTES", "536870912")?;
    let max_text = export_setting("MAX_EXPORT_TEXT_BYTES", "67108864")?;
    let max_sources = export_setting("MAX_EXPORT_SOURCES", "100000")?;
    let txt = p["type"].as_i64() == Some(1);
    guarded_patch(s, j, "outputs", output_id, json!({"status":1}), None).await?;
    let project = s.get("projects", field(p, "project_id")).await?;
    let target = s.get("targets", field(p, "target_id")).await?;
    let include = p["file_ids_include"].as_array();
    let exclude = p["file_ids_exclude"].as_array();
    let mut files = s
        .find(
            "files",
            doc! {"project_id":field(&project,"id"),"deleted_at":null,"activated":{"$ne":false}},
        )
        .await?;
    files.retain(|file| {
        file["type"].as_i64() != Some(1)
            && !include.is_some_and(|ids| {
                !ids.is_empty() && !ids.iter().any(|id| id.as_str() == Some(field(file, "id")))
            })
            && !exclude
                .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(field(file, "id"))))
    });
    files.sort_by(|a, b| {
        field(a, "name")
            .cmp(field(b, "name"))
            .then_with(|| field(a, "id").cmp(field(b, "id")))
    });
    let selected_count = {
        let ids: Vec<&str> = files.iter().map(|file| field(file, "id")).collect();
        s.db.collection::<Document>("sources")
            .count_documents(doc! {"file_id":{"$in":ids},"deleted_at":null})
            .await?
    };
    if selected_count > max_sources {
        return Err(export_limit("MAX_EXPORT_SOURCES"));
    }
    // Reject oversized binary input before any COS download; TXT exports do not read images.
    let mut resource_bytes = 0_u64;
    if !txt {
        for file in &files {
            let rid = field(file, "resource_id");
            if rid.is_empty() {
                continue;
            }
            let resource = s.get("resources", rid).await?;
            if field(&resource, "state") != "bound" {
                return Err(ApiError::bad("Selected export resource is unavailable"));
            }
            let size = resource["size"]
                .as_u64()
                .ok_or_else(|| ApiError::bad("Selected export resource has invalid size"))?;
            resource_bytes = resource_bytes
                .checked_add(size)
                .filter(|size| *size <= max_input)
                .ok_or_else(|| export_limit("MAX_EXPORT_INPUT_BYTES"))?;
        }
    }
    let mut text = String::new();
    append_export_text(
        &mut text,
        "1,0\r\n-\r\n框内\r\n框外\r\n-\r\nMoeFlow LabelPlus export\r\n-\r\n",
        max_text,
    )?;
    let mut metadata = if txt { None } else { Some(project.clone()) };
    let mut metadata_count = ExportJsonCounter { used: 0, limit: 0 };
    if let Some(value) = metadata.as_mut() {
        value["output_language"] = target["language_id"].clone();
        value["source_manifest"] = json!({});
        export_metadata_budget(&mut metadata_count, max_input, resource_bytes, text.len())?;
        serde_json::to_writer(&mut metadata_count, value)
            .map_err(|_| export_limit("MAX_EXPORT_INPUT_BYTES"))?;
    }
    let mut source_manifest = serde_json::Map::new();
    let mut downloads = Vec::new();
    let mut processed_sources = 0_u64;
    let file_total = files.len().max(1);
    for (file_index, file) in files.iter().enumerate() {
        let fid = field(file, "id");
        snapshot_export_file(s, j, file).await?;
        let name = path(s, file).await?;
        append_export_text(
            &mut text,
            &format!(">>>>>>>>[{}]<<<<<<<<\r\n", name),
            max_text,
        )?;
        if !txt {
            export_metadata_budget(&mut metadata_count, max_input, resource_bytes, text.len())?;
            if !source_manifest.is_empty() {
                metadata_count
                    .write_all(b",")
                    .map_err(|_| export_limit("MAX_EXPORT_INPUT_BYTES"))?;
            }
            serde_json::to_writer(&mut metadata_count, &name)
                .map_err(|_| export_limit("MAX_EXPORT_INPUT_BYTES"))?;
            metadata_count
                .write_all(b":[]")
                .map_err(|_| export_limit("MAX_EXPORT_INPUT_BYTES"))?;
        }
        let remaining = max_sources
            .saturating_sub(processed_sources)
            .saturating_add(1)
            .min(i64::MAX as u64) as i64;
        let mut cursor =
            s.db.collection::<Document>("sources")
                .find(doc! {"file_id":fid,"deleted_at":null})
                .sort(doc! {"rank":1,"id":1})
                .limit(remaining)
                .await?;
        let mut manifest_sources = Vec::new();
        let mut source_index = 0_u64;
        while cursor.advance().await? {
            if processed_sources >= max_sources {
                return Err(export_limit("MAX_EXPORT_SOURCES"));
            }
            processed_sources += 1;
            source_index += 1;
            let source: Value =
                mongodb::bson::from_document(cursor.deserialize_current()?).map_err(err)?;
            if !txt {
                export_metadata_budget(&mut metadata_count, max_input, resource_bytes, text.len())?;
                if !manifest_sources.is_empty() {
                    metadata_count
                        .write_all(b",")
                        .map_err(|_| export_limit("MAX_EXPORT_INPUT_BYTES"))?;
                }
                serde_json::to_writer(&mut metadata_count, &source)
                    .map_err(|_| export_limit("MAX_EXPORT_INPUT_BYTES"))?;
            }
            // Keep only the best candidate; comparison and equal-key last-wins match the former stable sort.
            let mut translations=s.db.collection::<Document>("translations").find(doc!{"source_id":field(&source,"id"),"target_id":field(&target,"id"),"deleted_at":null}).await?;
            let mut best: Option<Value> = None;
            while translations.advance().await? {
                let candidate: Value =
                    mongodb::bson::from_document(translations.deserialize_current()?)
                        .map_err(err)?;
                let key = (
                    candidate["selected"].as_bool().unwrap_or(false),
                    !field(&candidate, "proofread_content").is_empty(),
                    field(&candidate, "updated_at"),
                );
                let replace = best.as_ref().is_none_or(|current| {
                    key >= (
                        current["selected"].as_bool().unwrap_or(false),
                        !field(current, "proofread_content").is_empty(),
                        field(current, "updated_at"),
                    )
                });
                if replace {
                    best = Some(candidate);
                }
            }
            let content = best
                .as_ref()
                .map(|value| {
                    if field(value, "proofread_content").is_empty() {
                        field(value, "content")
                    } else {
                        field(value, "proofread_content")
                    }
                })
                .unwrap_or("");
            append_export_text(
                &mut text,
                &format!(
                    "----------------[{}]----------------[{},{},{}]\r\n",
                    source_index,
                    source["x"].as_f64().unwrap_or(0.0),
                    source["y"].as_f64().unwrap_or(0.0),
                    source["position_type"].as_i64().unwrap_or(1)
                ),
                max_text,
            )?;
            append_export_text(&mut text, content, max_text)?;
            append_export_text(&mut text, "\r\n", max_text)?;
            if !txt {
                export_metadata_budget(&mut metadata_count, max_input, resource_bytes, text.len())?;
                manifest_sources.push(source);
            }
        }
        if !txt {
            source_manifest.insert(name.clone(), Value::Array(manifest_sources));
            let rid = field(file, "resource_id");
            if !rid.is_empty() {
                downloads.push((
                    format!("images/{}", name),
                    rid.to_owned(),
                    if file["type"].as_i64() == Some(3) {
                        16 * 1024 * 1024
                    } else {
                        32 * 1024 * 1024
                    },
                ));
            }
        }
        progress(s, j, ((file_index + 1) * 90 / file_total) as i64).await?;
    }
    let data = if txt {
        text.into_bytes()
    } else {
        let mut metadata = metadata.ok_or_else(|| err("ZIP metadata initialization failed"))?;
        metadata["source_manifest"] = Value::Object(source_manifest);
        export_metadata_budget(&mut metadata_count, max_input, resource_bytes, text.len())?;
        let metadata_size = usize::try_from(metadata_count.used)
            .map_err(|_| export_limit("MAX_EXPORT_INPUT_BYTES"))?;
        let metadata_bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
            let mut bytes = Vec::new();
            bytes.try_reserve_exact(metadata_size).map_err(err)?;
            serde_json::to_writer(&mut bytes, &metadata)?;
            Ok(bytes)
        })
        .await
        .map_err(err)??;
        let generated_bytes = (metadata_bytes.len() as u64)
            .checked_add(text.len() as u64)
            .ok_or_else(|| export_limit("MAX_EXPORT_INPUT_BYTES"))?;
        if generated_bytes
            .checked_add(resource_bytes)
            .is_none_or(|size| size > max_input)
        {
            return Err(export_limit("MAX_EXPORT_INPUT_BYTES"));
        }
        let mut actual_input = generated_bytes;
        let mut entries = vec![
            ("project.json".into(), metadata_bytes),
            ("translations.txt".into(), text.into_bytes()),
        ];
        for (name, rid, limit) in downloads {
            let data = bytes(s, &rid, limit).await?;
            actual_input = actual_input
                .checked_add(data.len() as u64)
                .filter(|size| *size <= max_input)
                .ok_or_else(|| export_limit("MAX_EXPORT_INPUT_BYTES"))?;
            entries.push((name, data));
        }
        tokio::task::spawn_blocking(move || zip_entries(entries))
            .await
            .map_err(err)??
    };
    let user = s.get("users", field(p, "owner_id")).await?;
    let name = format!(
        "{}.{}",
        field(&project, "name"),
        if txt { "txt" } else { "zip" }
    );
    let resource = crate::storage::create_object(
        s,
        &user,
        Some(field(&project, "team_id")),
        &name,
        data,
        if txt {
            "text/plain; charset=utf-8"
        } else {
            "application/zip"
        },
    )
    .await?;
    commit_output(s, j, output_id, &resource).await
}

async fn import(s: &AppState, j: &Value) -> Result<Value> {
    let p = &j["payload"];
    let user = s.get("users", field(p, "owner_id")).await?;
    for rid in resources(p) {
        let input = s.get("resources", &rid).await?;
        if field(&input, "purpose") != "import" || field(&input, "owner_id") != field(p, "owner_id")
        {
            return Err(ApiError::forbidden(
                "Import requires your dedicated uploaded import resource",
            ));
        }
    }
    let project_resource = if field(p, "project_resource_id").is_empty() {
        field(p, "resource_id")
    } else {
        field(p, "project_resource_id")
    };
    let (mut metadata, entries) = if !project_resource.is_empty() {
        let data = bytes(
            s,
            project_resource,
            if field(p, "format") == "labelplus" {
                16 * 1024 * 1024
            } else {
                512 * 1024 * 1024
            },
        )
        .await?;
        if field(p, "format") == "labelplus" {
            (
                p["metadata"].clone(),
                vec![("translations.txt".into(), data)],
            )
        } else if data.starts_with(b"PK") {
            let entries = tokio::task::spawn_blocking(move || unzip(data))
                .await
                .map_err(err)??;
            let meta = entries
                .iter()
                .find(|(n, _)| n == "project.json")
                .ok_or_else(|| ApiError::bad("ZIP requires project.json"))?;
            let metadata = serde_json::from_slice::<Value>(&meta.1)
                .map_err(|_| ApiError::bad("Invalid project.json"))?;
            (metadata, entries)
        } else {
            (
                serde_json::from_slice::<Value>(&data)
                    .map_err(|_| ApiError::bad("Invalid project metadata JSON"))?,
                vec![],
            )
        }
    } else {
        (p["metadata"].clone(), vec![])
    };
    if !metadata.is_object() {
        metadata = json!({})
    }
    metadata["import_job_id"] = j["id"].clone();
    metadata["import_claim_token"] = j["claim_token"].clone();
    let text = if !field(p, "labelplus_resource_id").is_empty() {
        String::from_utf8(bytes(s, field(p, "labelplus_resource_id"), 16 * 1024 * 1024).await?)
            .map_err(|_| ApiError::bad("LabelPlus text is not UTF-8"))?
    } else if let Some((_, data)) = entries.iter().find(|(n, _)| n == "translations.txt") {
        String::from_utf8(data.clone()).map_err(|_| ApiError::bad("LabelPlus text is not UTF-8"))?
    } else {
        return Err(ApiError::bad(
            "Import requires translations.txt or uploaded LabelPlus resource",
        ));
    };
    let lp = tokio::task::spawn_blocking(move || labelplus(&text))
        .await
        .map_err(err)??;
    // The job stores its project before importing children, so recovery never creates duplicate projects.
    let current = s.get("jobs", field(j, "id")).await?;
    let project = if let Some(id) = current["import_project_id"].as_str() {
        s.get("projects", id).await?
    } else if !field(p, "project_id").is_empty() {
        s.get("projects", field(p, "project_id")).await?
    } else {
        let project = crate::groups::create_import_project(
            s,
            &user,
            field(p, "team_id"),
            field(p, "project_set_id"),
            &metadata,
        )
        .await?;
        guarded_patch(
            s,
            j,
            "jobs",
            field(j, "id"),
            json!({"import_project_id":project["id"]}),
            None,
        )
        .await?;
        project
    };
    let pid = field(&project, "id");
    guarded_patch(
        s,
        j,
        "projects",
        pid,
        json!({"import_from_labelplus_status":1}),
        None,
    )
    .await?;
    let targets = s.find("targets", doc! {"project_id":pid}).await?;
    let target = targets
        .first()
        .ok_or_else(|| ApiError::bad("Import requires target language"))?;
    for (i, f) in lp.iter().enumerate() {
        let mut fid = format!("{}:import:{}", field(j, "id"), i);
        let mut parent: Option<String> = None;
        let parts = f.name.split('/').collect::<Vec<_>>();
        for (depth, part) in parts[..parts.len() - 1].iter().enumerate() {
            let candidate=s.find("files",doc!{"project_id":pid,"parent_id":parent.as_deref(),"name_key":part.to_lowercase(),"type":1,"activated":true,"deleted_at":null}).await?;
            let folder_id = if let Some(folder) = candidate.first() {
                field(folder, "id").to_owned()
            } else {
                format!("{}:folder:{}", field(j, "id"), parts[..=depth].join("/"))
            };
            upsert(
                s,
                j,
                "files",
                &folder_id,
                json!({"project_id":pid,"parent_id":parent,"name":part,"type":1,"activated":true}),
            )
            .await?;
            parent = Some(folder_id);
        }
        let named=s.find("files",doc!{"project_id":pid,"parent_id":parent.as_deref(),"name_key":parts.last().unwrap().to_lowercase(),"activated":true,"deleted_at":null}).await?;
        if let Some(existing) = named.first() {
            fid = field(existing, "id").to_owned();
        }
        let exists = s.find("files", doc! {"id":&fid}).await?;
        if exists.is_empty() {
            let image = entries
                .iter()
                .find(|(n, _)| n == &format!("images/{}", f.name) || n == &f.name);
            let mut resource_id = json!("");
            if let Some((name, data)) = image {
                if name == "project.json" || name == "translations.txt" {
                    return Err(ApiError::bad(
                        "Archive metadata cannot be imported as a file resource",
                    ));
                }
                let refs = s
                    .find("resource_refs", doc! {"kind":"file","target_id":&fid})
                    .await?;
                let resource = if let Some(reference) = refs.first() {
                    s.get("resources", field(reference, "resource_id")).await?
                } else {
                    crate::storage::create_object(
                        s,
                        &user,
                        Some(field(&project, "team_id")),
                        name,
                        data.clone(),
                        mime(name),
                    )
                    .await?
                };
                resource_id = resource["id"].clone();
            }
            upsert(s,j,"files",&fid,json!({"project_id":pid,"parent_id":parent,"name":parts.last(),"type":if mime(&f.name).starts_with("text/"){3}else{2},"resource_id":resource_id,"revision":1,"revision_family_id":fid,"activated":true,"owner_id":user["id"]})).await?;
        }
        for (rank, label) in f.labels.iter().enumerate() {
            let sid = format!("{}:label:{}", fid, rank);
            upsert(s,j,"sources",&sid,json!({"file_id":fid,"content":metadata["source_manifest"][&f.name][rank]["content"].as_str().unwrap_or(""),"x":label["x"],"y":label["y"],"position_type":label["position_type"],"rank":rank})).await?;
            upsert(s,j,"translations",&format!("{}:translation",sid),json!({"source_id":sid,"target_id":target["id"],"user_id":user["id"],"content":label["content"]})).await?;
        }
        guarded_patch(
            s,
            j,
            "projects",
            pid,
            json!({"import_from_labelplus_percent":(i+1)*100/lp.len()}),
            None,
        )
        .await?;
        progress(s, j, ((i + 1) * 90 / lp.len()) as i64).await?;
    }
    let result = json!({"project_id":pid,"file_count":lp.len()});
    guarded_patch(
        s,
        j,
        "projects",
        pid,
        json!({"import_from_labelplus_status":2}),
        Some(&result),
    )
    .await?;
    Ok(result)
}
fn mime(name: &str) -> &'static str {
    match name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "txt" | "md" | "srt" => "text/plain",
        _ => "application/octet-stream",
    }
}

async fn ai(s: &AppState, j: &Value) -> Result<Value> {
    let p = &j["payload"];
    let fid = field(p, "file_id");
    let file = s.get("files", fid).await?;
    let project = s.get("projects", field(&file, "project_id")).await?;
    if project["status"].as_i64() == Some(1)
        || project.get("deleted_at").is_some_and(|v| !v.is_null())
    {
        return Err(ApiError::bad("Project is read-only"));
    }
    let base = core::env("AI_BASE_URL", "");
    let key = core::env("AI_API_KEY", "");
    let model = core::env("AI_MODEL", "");
    if base.is_empty() || key.is_empty() || model.is_empty() {
        return Err(ApiError::unavailable(
            "AI_BASE_URL, AI_API_KEY, and AI_MODEL are required",
        ));
    }
    let target = s.get("targets", field(p, "target_id")).await?;
    if field(&target, "project_id") != field(&project, "id") {
        return Err(ApiError::bad("Target does not belong to project"));
    }
    let image = bytes(s, field(p, "resource_id"), 20 * 1024 * 1024).await?;
    let image_mime = mime(field(&file, "name"));
    let data_uri = tokio::task::spawn_blocking(move || {
        let mut data_uri =
            String::with_capacity(13 + image_mime.len() + image.len().div_ceil(3) * 4);
        data_uri.push_str("data:");
        data_uri.push_str(image_mime);
        data_uri.push_str(";base64,");
        base64::engine::general_purpose::STANDARD.encode_string(&image, &mut data_uri);
        data_uri
    })
    .await
    .map_err(err)?;
    let prompt=format!("Extract every text region in this image and translate to language {}. Return ONLY a JSON object {{\"sources\":[{{\"content\":\"original text\",\"x\":0.0,\"y\":0.0,\"translation\":\"translated text\"}}]}}. x and y are normalized center coordinates within [0,1]. Preserve reading order. Additional user instruction: {}",field(&target,"language_id"),field(p,"extra_prompt"));
    let saved = s.get("jobs", field(j, "id")).await?;
    let answer = if let Some(answer) = saved.get("ai_response") {
        answer.clone()
    } else {
        let mut request = json!({"model":model,"messages":[{"role":"user","content":[{"type":"text","text":prompt},{"type":"image_url","image_url":{"url":null}}]}],"response_format":{"type":"json_object"}});
        request["messages"][0]["content"][1]["image_url"]["url"] = Value::String(data_uri);
        let response = s
            .http
            .post(format!("{}/chat/completions", base.trim_end_matches('/')))
            .bearer_auth(key)
            .timeout(Duration::from_secs(300))
            .json(&request)
            .send()
            .await?;
        let status = response.status();
        let body = bounded_ai_body(response).await?;
        if !status.is_success() {
            let text = String::from_utf8_lossy(&body);
            return Err(ApiError {
                status,
                message: format!(
                    "AI service {status}: {}",
                    text.chars().take(1024).collect::<String>()
                ),
            });
        }
        let response: Value = serde_json::from_slice(&body)?;
        let content = response["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| err("AI response missing content"))?;
        let answer: Value = serde_json::from_str(content)?;
        guarded_patch(
            s,
            j,
            "jobs",
            field(j, "id"),
            json!({"ai_response":answer}),
            None,
        )
        .await?;
        answer
    };
    let sources = answer["sources"]
        .as_array()
        .ok_or_else(|| err("AI response missing sources"))?;
    if sources.len() > 10000 {
        return Err(err("AI returned too many sources"));
    }
    // Validate the whole response before any database mutation.
    for source in sources {
        for coord in ["x", "y"] {
            let n = source[coord]
                .as_f64()
                .ok_or_else(|| err("AI coordinates must be numbers"))?;
            if !(0.0..=1.0).contains(&n) {
                return Err(err("AI coordinate outside image"));
            }
        }
        if source["content"].as_str().is_none() || source["translation"].as_str().is_none() {
            return Err(err("AI text fields missing"));
        }
    }
    let current = s.get("jobs", field(j, "id")).await?;
    if field(&current, "status") != "running"
        || field(&current, "claim_token") != field(j, "claim_token")
    {
        return Err(err("AI job lease lost"));
    }
    let mut translated = 0;
    for (i, source) in sources.iter().enumerate() {
        let sid = format!("{}:ai:{}", field(j, "id"), i);
        upsert(s,j,"sources",&sid,json!({"file_id":fid,"content":source["content"],"x":source["x"],"y":source["y"],"rank":i,"position_type":1,"job_id":j["id"]})).await?;
        if !field(source, "translation").is_empty() {
            translated += 1;
            upsert(s,j,"translations",&format!("{}:translation",sid),json!({"source_id":sid,"target_id":target["id"],"user_id":p["owner_id"],"content":source["translation"],"job_id":j["id"]})).await?;
        }
    }
    let result =
        json!({"file_id":fid,"source_count":sources.len(),"translated_source_count":translated});
    guarded_patch(s, j, "files", fid, json!({}), Some(&result)).await?;
    Ok(result)
}

async fn claim_deleted(s: &AppState, collection: &str, item: &Value) -> Result<bool> {
    if field(item, "deletion_state") == "deleting" {
        return Ok(true);
    }
    let root_type = field(item, "deletion_root_type");
    let root_id = if field(item, "deletion_root_id").is_empty() {
        field(item, "id")
    } else {
        field(item, "deletion_root_id")
    };
    let root_collection = match root_type {
        "team" => "teams",
        "project" => "projects",
        "project_set" => "project_sets",
        "file" => "files",
        "target" => "targets",
        "term_bank" => "term_banks",
        "term" => "terms",
        _ => collection,
    };
    let mut session = s.mongo.start_session().await?;
    session.start_transaction().await?;
    let changed =
        s.db.collection::<Document>(root_collection)
            .update_one(
                doc! {"id":root_id,"deleted_at":{"$lte":epoch()-WEEK}},
                doc! {"$set":{"deletion_state":"deleting"}},
            )
            .session(&mut session)
            .await?;
    if changed.matched_count == 0 {
        session.abort_transaction().await?;
        return Ok(false);
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
        s.db.collection::<Document>(c).update_many(doc!{"deletion_root_type":root_type,"deletion_root_id":root_id,"deleted_at":{"$lte":epoch()-WEEK}},doc!{"$set":{"deletion_state":"deleting"}}).session(&mut session).await?;
    }
    session.commit_transaction().await?;
    Ok(true)
}
async fn gc(s: &AppState) -> Result<Value> {
    if core::env("GC_ENABLED", "false") != "true" {
        return Ok(json!({"enabled":false,"business_enabled":false}));
    }
    let business = core::env("RESOURCE_BACKUP_CONFIRMED", "false") == "true"
        && core::env("RESOURCE_PROTECTION_ENABLED", "false") == "true";
    crate::storage::expire_sessions(s).await?;
    crate::storage::recover_expired(s).await?;
    // Retained staging registries permit deletion again if a late signed PUT recreated the key.
    for stage in s
        .find("resources", doc! {"state":"deleted","purpose":"staging"})
        .await?
    {
        s.cos.delete(&s.http, field(&stage, "key")).await?;
    }
    for terminal in s
        .find(
            "jobs",
            doc! {"status":{"$in":["succeeded","failed","cancelled"]}},
        )
        .await?
    {
        for reference in s
            .find(
                "resource_refs",
                doc! {"kind":"job","target_id":field(&terminal,"id")},
            )
            .await?
        {
            crate::storage::retire(
                s,
                field(&reference, "resource_id"),
                "job",
                field(&terminal, "id"),
                crate::storage::RetirementPolicy::RecoveryGrace,
            )
            .await?;
        }
    }
    for reference in s.find("resource_refs", doc! {"kind":"file"}).await? {
        let target = field(&reference, "target_id");
        if let Some((job_id, _)) = target.split_once(":import:") {
            if !s.find("files", doc! {"id":target}).await?.is_empty() {
                continue;
            }
            let job = s
                .find(
                    "jobs",
                    doc! {"id":job_id,"status":{"$in":["preparing","queued","running"]}},
                )
                .await?;
            if job.is_empty() {
                crate::storage::retire(
                    s,
                    field(&reference, "resource_id"),
                    "file",
                    target,
                    crate::storage::RetirementPolicy::RecoveryGrace,
                )
                .await?;
            }
        }
    }
    let mut deleted = 0;
    for output in s
        .find(
            "outputs",
            doc! {"expires_at":{"$lte":epoch()},"state":{"$ne":"expired"}},
        )
        .await?
    {
        if !field(&output, "resource_id").is_empty() {
            crate::storage::retire(
                s,
                field(&output, "resource_id"),
                "output",
                field(&output, "id"),
                crate::storage::RetirementPolicy::LifetimeExpired,
            )
            .await?;
        }
        s.patch(
            "outputs",
            field(&output, "id"),
            json!({"state":"expired","expired":true,"expired_at":epoch()}),
        )
        .await?;
    }
    if business {
        for file in s
            .find("files", doc! {"deleted_at":{"$lte":epoch()-WEEK}})
            .await?
        {
            let fid = field(&file, "id");
            let active=s.find("jobs",doc!{"status":{"$in":["running","queued","preparing"]},"$or":[{"payload.file_id":fid},{"payload.project_id":field(&file,"project_id")}]}).await?;
            if !active.is_empty() {
                continue;
            }
            if !claim_deleted(s, "files", &file).await? {
                continue;
            }
            for source in s.find("sources", doc! {"file_id":fid}).await? {
                s.db.collection::<Document>("translations")
                    .delete_many(doc! {"source_id":field(&source,"id")})
                    .await?;
                s.db.collection::<Document>("tips")
                    .delete_many(doc! {"source_id":field(&source,"id")})
                    .await?;
                s.remove("sources", field(&source, "id")).await?;
            }
            let refs = s
                .find("resource_refs", doc! {"kind":"file","target_id":fid})
                .await?;
            for r in refs {
                crate::storage::retire(
                    s,
                    field(&r, "resource_id"),
                    "file",
                    fid,
                    crate::storage::RetirementPolicy::LifetimeExpired,
                )
                .await?;
            }
            s.remove("files", fid).await?;
        }
        for c in ["projects", "project_sets", "teams"] {
            for group in s.find(c, doc! {"deleted_at":{"$lte":epoch()-WEEK}}).await? {
                let gid = field(&group, "id");
                if !s.find("files", doc! {"project_id":gid}).await?.is_empty() {
                    continue;
                }
                let child_collection = if c == "projects" { "files" } else { "projects" };
                let child_field = if c == "teams" {
                    "team_id"
                } else if c == "project_sets" {
                    "project_set_id"
                } else {
                    "project_id"
                };
                if !s
                    .find(child_collection, doc! {child_field:gid})
                    .await?
                    .is_empty()
                {
                    continue;
                }
                if !claim_deleted(s, c, &group).await? {
                    continue;
                }
                s.db.collection::<Document>("memberships")
                    .delete_many(doc! {"group_id":gid})
                    .await?;
                s.db.collection::<Document>("targets")
                    .delete_many(doc! {"project_id":gid})
                    .await?;
                for collection in ["invitations", "applications", "roles", "workflows"] {
                    s.db.collection::<Document>(collection)
                        .delete_many(doc! {"group_id":gid})
                        .await?;
                }
                for reference in s
                    .find("resource_refs", doc! {"target_id":gid,"kind":"avatar"})
                    .await?
                {
                    crate::storage::retire(
                        s,
                        field(&reference, "resource_id"),
                        field(&reference, "kind"),
                        gid,
                        crate::storage::RetirementPolicy::LifetimeExpired,
                    )
                    .await?;
                }
                if c == "teams" {
                    for collection in ["terms", "term_banks"] {
                        s.db.collection::<Document>(collection)
                            .delete_many(doc! {"team_id":gid})
                            .await?;
                    }
                }
                s.remove(c, gid).await?;
            }
        }
    }
    if business {
        for collection in [
            "sources",
            "translations",
            "targets",
            "tips",
            "terms",
            "term_banks",
        ] {
            for item in s
                .find(collection, doc! {"deleted_at":{"$lte":epoch()-WEEK}})
                .await?
            {
                if !claim_deleted(s, collection, &item).await? {
                    continue;
                }
                if collection == "sources" {
                    for dependent in ["translations", "tips"] {
                        s.db.collection::<Document>(dependent)
                            .delete_many(doc! {"source_id":field(&item,"id")})
                            .await?;
                    }
                }
                if collection == "targets" {
                    for dependent in ["translations", "tips"] {
                        s.db.collection::<Document>(dependent)
                            .delete_many(doc! {"target_id":field(&item,"id")})
                            .await?;
                    }
                }
                if collection == "term_banks" {
                    s.db.collection::<Document>("terms")
                        .delete_many(doc! {"term_bank_id":field(&item,"id")})
                        .await?;
                }
                s.remove(collection, field(&item, "id")).await?;
            }
        }
    }
    for resource in s
        .find(
            "resources",
            doc! {"state":{"$in":["unbound","uploading","deleting"]},"expires_at":{"$lte":epoch()}},
        )
        .await?
    {
        let rid = field(&resource, "id");
        if !business
            && (!field(&resource, "bound_file_id").is_empty()
                || !field(&resource, "bound_avatar_id").is_empty())
        {
            continue;
        }
        if !s
            .find("resource_refs", doc! {"resource_id":rid})
            .await?
            .is_empty()
        {
            continue;
        }
        if !s.find("jobs",doc!{"status":{"$in":["running","queued","preparing"]},"$or":[{"payload.resource_id":rid},{"payload.project_resource_id":rid},{"payload.labelplus_resource_id":rid}]}).await?.is_empty(){continue}
        let mut session = s.mongo.start_session().await?;
        session.start_transaction().await?;
        if s.db
            .collection::<Document>("resource_refs")
            .count_documents(doc! {"resource_id":rid})
            .session(&mut session)
            .await?
            > 0
        {
            session.abort_transaction().await?;
            continue;
        }
        let mut filter =
            doc! {"id":rid,"state":field(&resource,"state"),"expires_at":{"$lte":epoch()}};
        if !business {
            filter.insert("$and",mongodb::bson::bson!([{"$or":[{"bound_file_id":null},{"bound_file_id":""}]},{"$or":[{"bound_avatar_id":null},{"bound_avatar_id":""}]}]));
        }
        let claim =
            s.db.collection::<Document>("resources")
                .update_one(filter, doc! {"$set":{"state":"deleting"}})
                .session(&mut session)
                .await?;
        session.commit_transaction().await?;
        if claim.matched_count == 0 {
            continue;
        }
        // Recheck refs after the deletion state fences all new attaches. Never delete the registry before COS confirms deletion.
        if !s
            .find("resource_refs", doc! {"resource_id":rid})
            .await?
            .is_empty()
        {
            s.patch("resources", rid, json!({"state":"bound"})).await?;
            continue;
        }
        s.cos.delete(&s.http, field(&resource, "key")).await?;
        s.patch(
            "resources",
            rid,
            json!({"state":"deleted","deleted_epoch":epoch()}),
        )
        .await?;
        crate::storage::release_quota(s, &s.get("resources", rid).await?).await?;
        deleted += 1;
    }
    // Terminal job data contains no credentials; retain seven days, never TTL-delete resource records.
    for job in s.find("jobs",doc!{"status":{"$in":["succeeded","failed","cancelled"]},"finished_epoch":{"$lte":epoch()-WEEK}}).await?{let refs=s.find("resource_refs",doc!{"kind":"job","target_id":field(&job,"id")}).await?;for r in refs{crate::storage::retire(s,field(&r,"resource_id"),"job",field(&job,"id"),crate::storage::RetirementPolicy::RecoveryGrace).await?;}s.remove("jobs",field(&job,"id")).await?;}
    Ok(json!({"enabled":true,"business_enabled":business,"objects_deleted":deleted}))
}

async fn job_access(r: &Request, j: &Value) -> Result<()> {
    let uid = r.uid()?;
    if field(j, "kind") == "mail" {
        return Err(ApiError::forbidden("Mail jobs are private"));
    }
    if r.is_admin() || field(&j["payload"], "owner_id") == uid {
        return Ok(());
    }
    if !field(&j["payload"], "project_id").is_empty() {
        crate::groups::require(r, "project", field(&j["payload"], "project_id"), 1).await?;
        return Ok(());
    }
    Err(ApiError::forbidden("Job belongs to another user"))
}
fn job_public(j: &Value) -> Value {
    json!({"id":j["id"],"task_id":j["id"],"kind":j["kind"],"status":j["status"],"progress":j["progress"],"result":j["result"],"last_error":j["last_error"]})
}
pub async fn decorate_output(s: &AppState, o: &Value) -> Result<Value> {
    let mut out = o.clone();
    for key in ["file_ids_include", "file_ids_exclude"] {
        if out.get(key).is_none_or(Value::is_null) {
            out[key] = json!([]);
        }
    }
    let expired = o["expires_at"].as_i64().is_some_and(|at| at <= epoch());
    out["expired"] = json!(expired);
    if expired {
        out["state"] = json!("expired");
        out["link"] = Value::Null;
    }
    out["status_details"] = json!([
    {"id":0,"name":"Queued","intro":"Waiting for an available export worker"},
    {"id":1,"name":"Running","intro":"Collecting translations and files and generating the export"},
    {"id":2,"name":"Succeeded","intro":"Export is available for download for seven days"},
    {"id":3,"name":"Failed","intro":"Export failed or was cancelled; see last_error for details"}
    ]);
    out["progress"] = json!(o["progress"]
        .as_i64()
        .unwrap_or(if o["status"].as_i64() == Some(2) {
            100
        } else {
            0
        }));
    if !field(o, "task_id").is_empty() {
        if let Ok(job) = s.get("jobs", field(o, "task_id")).await {
            if job["progress"].is_number() {
                out["progress"] = job["progress"].clone();
            }
            out["last_error"] = job["last_error"].clone();
            out["status"] = json!(match field(&job, "status") {
                "succeeded" => 2,
                "failed" | "cancelled" => 3,
                "running" => 1,
                _ => 0,
            });
        }
    }
    out["create_time"] = o["created_at"].clone();
    out["project"] = s.get("projects", field(o, "project_id")).await?;
    let mut target = s.get("targets", field(o, "target_id")).await?;
    target["language"] = s.get("languages", field(&target, "language_id")).await?;
    out["target"] = target;
    if let Ok(user) = s.get("users", field(o, "owner_id")).await {
        out["user"] = crate::auth::decorate_user(s, &user, false).await?;
    }
    if let Some(rid) = o["resource_id"].as_str() {
        let resource = s.get("resources", rid).await?;
        if o["expires_at"].as_i64().unwrap_or(0) > epoch() {
            out["link"] = json!(s.cos.presign("GET", field(&resource, "key"), 300, &[])?);
        }
    }
    Ok(out)
}
async fn output_file_ids(r: &Request, project_id: &str, key: &str) -> Result<Vec<String>> {
    let Some(value) = r.body.get(key) else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| ApiError::bad(format!("{key} must be an array of file IDs")))?;
    let mut ids = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for value in values {
        let id = value
            .as_str()
            .ok_or_else(|| ApiError::bad(format!("{key} must contain only file ID strings")))?;
        if id.is_empty() || id.trim() != id || id.len() > 4096 {
            return Err(ApiError::bad(format!("{key} contains an invalid file ID")));
        }
        if seen.insert(id) {
            ids.push(id.to_owned());
        }
    }
    if !ids.is_empty() {
        let count=r.state.db.collection::<Document>("files").count_documents(doc!{"id":{"$in":&ids},"project_id":project_id,"deleted_at":null,"activated":{"$ne":false}}).await?;
        if count != ids.len() as u64 {
            return Err(ApiError::bad(format!(
                "{key} contains a missing, recycled, inactive, or foreign-project file ID"
            )));
        }
    }
    Ok(ids)
}
async fn create_output(r: &Request, pid: &str, tid: &str) -> Result<Value> {
    crate::groups::require(r, "project", pid, 1060).await?;
    let target = r.state.get("targets", tid).await?;
    if field(&target, "project_id") != pid {
        return Err(ApiError::bad("Target belongs to another project"));
    }
    if !r.state.cos.configured() {
        return Err(ApiError::unavailable("COS is not configured"));
    }
    let t = r.body["type"].as_i64().unwrap_or(0);
    if t != 0 && t != 1 {
        return Err(ApiError::bad("Unknown output type"));
    }
    let include = output_file_ids(r, pid, "file_ids_include").await?;
    let exclude = output_file_ids(r, pid, "file_ids_exclude").await?;
    let owner = r.uid()?;
    let o=r.state.insert("outputs",json!({"project_id":pid,"target_id":tid,"owner_id":owner,"type":t,"status":0,"file_ids_include":include,"file_ids_exclude":exclude})).await?;
    let mut payload = o.clone();
    payload["output_id"] = o["id"].clone();
    let job = enqueue(
        &r.state,
        "export",
        payload,
        Some(&format!("export:{}", field(&o, "id"))),
    )
    .await?;
    let o = r
        .state
        .patch("outputs", field(&o, "id"), json!({"task_id":job["id"]}))
        .await?;
    decorate_output(&r.state, &o).await
}
pub async fn handle(r: &Request) -> Result<Option<Reply>> {
    let p = r.path.iter().map(String::as_str).collect::<Vec<_>>();
    let method = r.method.as_str();
    match p.as_slice() {
        ["jobs", id] => {
            let j = r.state.get("jobs", id).await?;
            job_access(r, &j).await?;
            if method != "GET" {
                return Err(ApiError::bad("Use GET for job status"));
            }
            Ok(Some(job_public(&j).into()))
        }
        ["jobs", id, "cancel"] => {
            let j = r.state.get("jobs", id).await?;
            job_access(r, &j).await?;
            if method != "POST" {
                return Err(ApiError::bad("Use POST to cancel"));
            }
            if field(&j["payload"], "owner_id") != r.uid()? && !r.is_admin() {
                return Err(ApiError::forbidden("Only owner may cancel job"));
            }
            r.state.db.collection::<Document>("jobs").update_one(doc!{"id":*id,"status":{"$in":["preparing","queued","running"]}},doc!{"$set":{"status":"cancelled","finished_epoch":epoch()},"$unset":{"claim_token":"","lease_until":""}}).await?;
            Ok(Some(job_public(&r.state.get("jobs", id).await?).into()))
        }
        ["files", fid, "ai"] => {
            if method != "POST" {
                return Err(ApiError::bad("Use POST to start AI"));
            }
            if !r.state.cos.configured() {
                return Err(ApiError::unavailable("COS is not configured"));
            }
            let file = r.state.get("files", fid).await?;
            let pid = field(&file, "project_id");
            crate::groups::require(r, "project", pid, 1080).await?;
            crate::groups::require(r, "project", pid, 1110).await?;
            for key in ["AI_BASE_URL", "AI_API_KEY", "AI_MODEL"] {
                if core::env(key, "").is_empty() {
                    return Err(ApiError::unavailable(format!("{key} is required")));
                }
            }
            let tid = core::required(&r.body, "target_id")?;
            let target = r.state.get("targets", &tid).await?;
            if field(&target, "project_id") != pid {
                return Err(ApiError::bad("Target belongs to another project"));
            }
            let job=enqueue(&r.state,"ai",json!({"file_id":fid,"project_id":pid,"resource_id":file["resource_id"],"target_id":tid,"owner_id":r.uid()?,"extra_prompt":r.body["extra_prompt"]}),None).await?;
            Ok(Some(job_public(&job).into()))
        }
        ["projects", pid, "ocr"] => {
            if method != "POST" {
                return Err(ApiError::bad("Use POST to start OCR"));
            }
            crate::groups::require(r, "project", pid, 1080).await?;
            crate::groups::require(r, "project", pid, 1110).await?;
            for key in ["AI_BASE_URL", "AI_API_KEY", "AI_MODEL"] {
                if core::env(key, "").is_empty() {
                    return Err(ApiError::unavailable(format!("{key} is required")));
                }
            }
            if !r.state.cos.configured() {
                return Err(ApiError::unavailable("COS is not configured"));
            }
            let targets = r.state.find("targets", doc! {"project_id":*pid}).await?;
            let target = targets
                .first()
                .ok_or_else(|| ApiError::bad("Project has no target"))?;
            let files = r
                .state
                .find(
                    "files",
                    doc! {"project_id":*pid,"type":2,"deleted_at":null,"activated":{"$ne":false}},
                )
                .await?;
            let mut jobs = vec![];
            for f in files {
                let j=enqueue(&r.state,"ai",json!({"file_id":f["id"],"project_id":pid,"resource_id":f["resource_id"],"target_id":target["id"],"owner_id":r.uid()?}),None).await?;
                jobs.push(job_public(&j));
            }
            Ok(Some(json!({"jobs":jobs}).into()))
        }
        ["projects", pid, "targets", tid, "outputs"] => {
            if method == "POST" {
                return Ok(Some(create_output(r, pid, tid).await?.into()));
            }
            if method != "GET" {
                return Err(ApiError::bad("Unsupported output method"));
            }
            crate::groups::require(r, "project", pid, 1060).await?;
            let mut values = vec![];
            for o in r
                .state
                .find("outputs", doc! {"project_id":*pid,"target_id":*tid})
                .await?
            {
                values.push(decorate_output(&r.state, &o).await?)
            }
            Ok(Some(Reply::list(values, r)))
        }
        ["projects", pid, "outputs"] => {
            if method != "POST" && method != "GET" {
                return Err(ApiError::bad("Unsupported output method"));
            }
            crate::groups::require(r, "project", pid, 1060).await?;
            let mut out = vec![];
            if method == "POST" {
                for t in r.state.find("targets", doc! {"project_id":*pid}).await? {
                    out.push(create_output(r, pid, field(&t, "id")).await?)
                }
            } else {
                for o in r.state.find("outputs", doc! {"project_id":*pid}).await? {
                    out.push(decorate_output(&r.state, &o).await?)
                }
            }
            Ok(Some(Reply::list(out, r)))
        }
        ["teams", tid, "outputs"] => {
            if method != "POST" {
                return Err(ApiError::bad("Use POST for team exports"));
            }
            crate::groups::require(r, "team", tid, 1).await?;
            let mut out = vec![];
            for project in r
                .state
                .find("projects", doc! {"team_id":*tid,"deleted_at":null})
                .await?
            {
                for target in r
                    .state
                    .find("targets", doc! {"project_id":field(&project,"id")})
                    .await?
                {
                    out.push(create_output(r, field(&project, "id"), field(&target, "id")).await?)
                }
            }
            Ok(Some(Reply::list(out, r)))
        }
        ["teams", tid, "imports"] => {
            if method != "POST" {
                return Err(ApiError::bad("Use POST for import"));
            }
            crate::groups::require(r, "team", tid, 1090).await?;
            if !r.state.cos.configured() {
                return Err(ApiError::unavailable("COS is not configured"));
            }
            let owner = r.uid()?;
            let set_id = if !field(&r.body, "project_id").is_empty() {
                let project =
                    crate::groups::require(r, "project", field(&r.body, "project_id"), 1080)
                        .await?;
                crate::groups::require(r, "project", field(&r.body, "project_id"), 1110).await?;
                if field(&project, "team_id") != *tid {
                    return Err(ApiError::bad("Project belongs to another team"));
                }
                field(&project, "project_set_id").to_owned()
            } else {
                core::required(&r.body, "project_set_id")?
            };
            let set = r.state.get("project_sets", &set_id).await?;
            if field(&set, "team_id") != *tid {
                return Err(ApiError::bad("Project set belongs to another team"));
            }
            let mut payload = r.body.clone();
            payload["team_id"] = json!(tid);
            payload["project_set_id"] = json!(set_id);
            payload["owner_id"] = json!(owner);
            if resources(&payload).is_empty() {
                return Err(ApiError::bad("Import requires an uploaded resource"));
            }
            for rid in resources(&payload) {
                let resource = r.state.get("resources", &rid).await?;
                if field(&resource, "purpose") != "import" {
                    return Err(ApiError::forbidden(
                        "Import requires a dedicated uploaded import resource",
                    ));
                }
                if field(&resource, "owner_id") != owner {
                    return Err(ApiError::forbidden(
                        "Import resource belongs to another user",
                    ));
                }
                if !["unbound", "bound"].contains(&field(&resource, "state")) {
                    return Err(ApiError::bad("Import resource is not ready"));
                }
            }
            let j = enqueue(&r.state, "import", payload, None).await?;
            Ok(Some(job_public(&j).into()))
        }
        _ => Ok(None),
    }
}

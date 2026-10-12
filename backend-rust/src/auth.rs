use crate::core::{self, ApiError, AppState, Reply, Request, Result};
use argon2::{password_hash::SaltString, Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use axum::http::Method;
use base64::{engine::general_purpose::STANDARD, Engine};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use mongodb::{
    bson::{doc, Document},
    options::{IndexOptions, ReturnDocument},
    IndexModel,
};
use rand::{rngs::OsRng, Rng};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[derive(Serialize, Deserialize)]
struct Claims {
    sub: String,
    jti: String,
    exp: i64,
    iat: i64,
    version: i64,
}
fn secret() -> Result<String> {
    let value = core::env("JWT_SECRET", "");
    if value.len() < 32 {
        return Err(ApiError::internal(
            "JWT_SECRET must contain at least 32 bytes",
        ));
    }
    Ok(value)
}
fn email(body: &Value, key: &str) -> Result<String> {
    let value = core::required(body, key)?.to_lowercase();
    let (local, domain) = value
        .split_once('@')
        .ok_or_else(|| ApiError::bad("Invalid email address"))?;
    if value.len() > 254
        || local.is_empty()
        || domain.contains('@')
        || !domain.contains('.')
        || value.chars().any(char::is_whitespace)
    {
        return Err(ApiError::bad("Invalid email address"));
    }
    Ok(value)
}
fn password(body: &Value, key: &str) -> Result<String> {
    let value = body[key]
        .as_str()
        .ok_or_else(|| ApiError::bad("Password required"))?;
    if value.len() < 8 || value.len() > 128 {
        return Err(ApiError::bad("Password must contain 8–128 bytes"));
    }
    Ok(value.to_owned())
}
async fn hash_password(value: String) -> Result<String> {
    tokio::task::spawn_blocking(move || {
        Argon2::default()
            .hash_password(value.as_bytes(), &SaltString::generate(&mut OsRng))
            .map(|h| h.to_string())
            .map_err(|_| ApiError::internal("Password hashing failed"))
    })
    .await
    .map_err(|_| ApiError::internal("Password task failed"))?
}
async fn matches_password(user: &Value, value: &str) -> Result<bool> {
    if value.is_empty() || value.len() > 128 {
        return Ok(false);
    }
    let hash = core::field(user, "password_hash").to_owned();
    let value = value.to_owned();
    tokio::task::spawn_blocking(move || {
        PasswordHash::new(&hash)
            .map(|h| {
                Argon2::default()
                    .verify_password(value.as_bytes(), &h)
                    .is_ok()
            })
            .unwrap_or(false)
    })
    .await
    .map_err(|_| ApiError::internal("Password task failed"))
}
fn code_hash(value: &str) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(format!("{}:{value}", secret()?).as_bytes())
    ))
}
async fn by_email(state: &AppState, address: &str) -> Result<Option<Value>> {
    Ok(state
        .find("users", doc! {"email":address})
        .await?
        .into_iter()
        .next())
}
fn version(user: &Value) -> i64 {
    user["auth_version"].as_i64().unwrap_or(0)
}
pub async fn decorate_user(state: &AppState, user: &Value, include_email: bool) -> Result<Value> {
    let mut value = core::public_user(user);
    value["disabled"] = json!(!active(user));
    value["banned"] = value["disabled"].clone();
    value["avatar"] = Value::Null;
    value["has_avatar"] = json!(false);
    let avatar = core::field(user, "avatar_resource_id");
    if !avatar.is_empty() {
        let resource = state.get("resources", avatar).await?;
        if core::field(&resource, "state") == "bound"
            && core::field(&resource, "owner_id") == core::field(user, "id")
        {
            value["avatar"] =
                json!(state
                    .cos
                    .presign("GET", core::field(&resource, "key"), 300, &[])?);
            value["has_avatar"] = json!(true);
        }
    }
    if !include_email {
        value.as_object_mut().unwrap().remove("email");
    }
    Ok(value)
}
fn active(user: &Value) -> bool {
    !user["disabled"].as_bool().unwrap_or(false)
}
async fn token(state: &AppState, user: &Value) -> Result<Value> {
    if !active(user) {
        return Err(ApiError::unauthorized("Account disabled"));
    }
    let ttl = core::env("SESSION_TTL_SECONDS", "604800")
        .parse::<i64>()
        .unwrap_or(604800)
        .clamp(300, 2592000);
    let claims = Claims {
        sub: core::field(user, "id").into(),
        jti: core::id(),
        iat: core::epoch(),
        exp: core::epoch() + ttl,
        version: version(user),
    };
    let jwt = jsonwebtoken::encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret()?.as_bytes()),
    )
    .map_err(|_| ApiError::internal("Token signing failed"))?;
    state.db.collection::<Document>("sessions").insert_one(doc!{"id":&claims.jti,"user_id":&claims.sub,"expires_at":claims.exp,"auth_version":claims.version,"purge_at":mongodb::bson::DateTime::from_millis(claims.exp*1000)}).await?;
    Ok(json!({"token":jwt,"expires_at":claims.exp}))
}
pub async fn authenticate(state: &AppState, bearer: &str) -> Result<Value> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = 0;
    let claims = jsonwebtoken::decode::<Claims>(
        bearer,
        &DecodingKey::from_secret(secret()?.as_bytes()),
        &validation,
    )
    .map_err(|_| ApiError::unauthorized("Invalid or expired session"))?
    .claims;
    let session = state
        .db
        .collection::<Document>("sessions")
        .find_one(doc! {"id":&claims.jti,"user_id":&claims.sub,"expires_at":{"$gt":core::epoch()}})
        .await?;
    if session.is_none() {
        return Err(ApiError::unauthorized("Session revoked"));
    }
    let user = state
        .get("users", &claims.sub)
        .await
        .map_err(|_| ApiError::unauthorized("Account unavailable"))?;
    if !active(&user) || version(&user) != claims.version {
        return Err(ApiError::unauthorized("Session revoked"));
    }
    Ok(user)
}
async fn revoke(state: &AppState, uid: &str) -> Result<()> {
    state
        .db
        .collection::<Document>("users")
        .update_one(doc! {"id":uid}, doc! {"$inc":{"auth_version":1}})
        .await?;
    state
        .db
        .collection::<Document>("sessions")
        .delete_many(doc! {"user_id":uid})
        .await?;
    Ok(())
}
async fn change_password(state: &AppState, uid: &str, value: String) -> Result<()> {
    let hash = hash_password(value).await?;
    state
        .db
        .collection::<Document>("users")
        .update_one(
            doc! {"id":uid},
            doc! {"$set":{"password_hash":hash,"updated_at":core::now()},"$inc":{"auth_version":1}},
        )
        .await?;
    state
        .db
        .collection::<Document>("sessions")
        .delete_many(doc! {"user_id":uid})
        .await?;
    Ok(())
}
async fn admin_update(r: &Request, uid: &str, mut fields: Document) -> Result<()> {
    let mut session = r.state.mongo.start_session().await?;
    session.start_transaction().await?;
    // Serialize privilege changes so two administrators cannot concurrently disable each other.
    r.state
        .db
        .collection::<Document>("site_settings")
        .update_one(
            doc! {"id":"site"},
            doc! {"$inc":{"admin_change_version":1i64}},
        )
        .session(&mut session)
        .await?;
    let users = r.state.db.collection::<Document>("users");
    let actor=users.find_one(doc!{"id":r.uid()?,"admin":true,"disabled":{"$ne":true},"auth_version":version(r.user.as_ref().unwrap())}).session(&mut session).await?;
    if actor.is_none() {
        session.abort_transaction().await?;
        return Err(ApiError::forbidden("Administrator session no longer valid"));
    }
    let target = users
        .find_one(doc! {"id":uid})
        .session(&mut session)
        .await?
        .ok_or_else(|| ApiError::not_found("User not found"))?;
    let disabling = fields.get_bool("disabled").unwrap_or(false)
        || fields.get_bool("admin").ok() == Some(false);
    if disabling && target.get_bool("admin").unwrap_or(false) {
        let others = users
            .count_documents(doc! {"id":{"$ne":uid},"admin":true,"disabled":{"$ne":true}})
            .session(&mut session)
            .await?;
        if others == 0 {
            session.abort_transaction().await?;
            return Err(ApiError::bad(
                "At least one active administrator must remain",
            ));
        }
    }
    fields.insert("updated_at", core::now());
    users
        .update_one(
            doc! {"id":uid},
            doc! {"$set":fields,"$inc":{"auth_version":1i64}},
        )
        .session(&mut session)
        .await?;
    r.state
        .db
        .collection::<Document>("sessions")
        .delete_many(doc! {"user_id":uid})
        .session(&mut session)
        .await?;
    session.commit_transaction().await?;
    Ok(())
}
async fn cooldown(state: &AppState, key: &str, seconds: i64) -> Result<()> {
    let now = core::epoch();
    let result=state.db.collection::<Document>("auth_rate_limits").update_one(doc!{"id":key,"next_at":{"$lte":now}},doc!{"$set":{"next_at":now+seconds,"purge_at":mongodb::bson::DateTime::from_millis((now+seconds+3600)*1000)}}).upsert(true).await;
    match result {
        Ok(_) => Ok(()),
        Err(e) if e.to_string().contains("E11000") => {
            Err(ApiError::bad("Please wait before trying again"))
        }
        Err(e) => Err(e.into()),
    }
}
async fn attempt_limit(state: &AppState, key: &str, window: i64, max: i64) -> Result<()> {
    let bucket = core::epoch() / window;
    let result=state.db.collection::<Document>("auth_rate_limits").find_one_and_update(doc!{"id":format!("attempt:{key}:{bucket}")},doc!{"$inc":{"count":1i64},"$set":{"purge_at":mongodb::bson::DateTime::from_millis((bucket+2)*window*1000)}}).upsert(true).return_document(ReturnDocument::After).await?;
    if result.and_then(|d| d.get_i64("count").ok()).unwrap_or(1) > max {
        return Err(ApiError::bad("Too many attempts; try again later"));
    }
    Ok(())
}
async fn check_code(state: &AppState, kind: &str, address: &str, answer: &str) -> Result<()> {
    let key = format!("{kind}:{address}");
    let collection = state.db.collection::<Document>("verification");
    let eligible = collection
        .find_one_and_update(
            doc! {"id":&key,"expires_at":{"$gt":core::epoch()},"attempts_left":{"$gt":0}},
            doc! {"$inc":{"attempts_left":-1}},
        )
        .return_document(ReturnDocument::Before)
        .await?;
    let Some(record) = eligible else {
        return Err(ApiError::bad("Verification expired or exhausted"));
    };
    let hash = code_hash(answer)?;
    if record.get_str("code_hash").unwrap_or("") != hash {
        return Err(ApiError::bad("Incorrect verification code"));
    }
    let used=collection.find_one_and_delete(doc!{"id":key,"code_hash":hash,"generation":record.get_str("generation").unwrap_or("")}).await?;
    if used.is_none() {
        return Err(ApiError::bad("Verification already used"));
    }
    Ok(())
}
async fn check_captcha(r: &Request) -> Result<()> {
    check_code(
        &r.state,
        "captcha",
        &core::required(&r.body, "captcha_info")?,
        &core::required(&r.body, "captcha")?,
    )
    .await
}
async fn send_code(state: &AppState, kind: &str, address: &str) -> Result<Value> {
    cooldown(state, &format!("mail:{address}"), 60).await?;
    let code = format!("{:06}", OsRng.gen_range(0..1_000_000));
    let generation = core::id();
    attempt_limit(state, &format!("email:{address}"), 86400, 30).await?;
    state.db.collection::<Document>("verification").update_one(doc!{"id":format!("{kind}:{address}")},doc!{"$set":{"kind":kind,"email":address,"code_hash":code_hash(&code)?,"expires_at":core::epoch()+600,"purge_at":mongodb::bson::DateTime::from_millis((core::epoch()+600)*1000),"attempts_left":5,"send_time":core::now(),"generation":&generation}}).upsert(true).await?;
    crate::jobs::enqueue(state,"mail",json!({"to":address,"subject":"MoeFlow account verification","text":format!("Your {kind} verification code is {code}. It expires in 10 minutes. If you did not request this email, ignore it.")}),Some(&format!("mail:{generation}"))).await?;
    Ok(json!({"wait":60}))
}
async fn captcha(state: &AppState) -> Result<Value> {
    attempt_limit(state, "captcha-global", 60, 1000).await?;
    let code = format!("{:06}", OsRng.gen_range(0..1_000_000));
    let info = core::id();
    let digits: [[u8; 7]; 10] = [
        [14, 17, 19, 21, 25, 17, 14],
        [4, 12, 4, 4, 4, 4, 14],
        [14, 17, 1, 2, 4, 8, 31],
        [30, 1, 1, 14, 1, 1, 30],
        [2, 6, 10, 18, 31, 2, 2],
        [31, 16, 16, 30, 1, 1, 30],
        [14, 16, 16, 30, 17, 17, 14],
        [31, 1, 2, 4, 8, 8, 8],
        [14, 17, 17, 14, 17, 17, 14],
        [14, 17, 17, 15, 1, 1, 14],
    ];
    let (w, h) = (160usize, 44usize);
    let mut pixels = vec![245u8; w * h * 3];
    for (i, d) in code.bytes().enumerate() {
        let offset = OsRng.gen_range(4..12);
        for y in 0..7 {
            for x in 0..5 {
                if digits[(d - b'0') as usize][y] & (1 << (4 - x)) != 0 {
                    for dy in 0..4 {
                        for dx in 0..4 {
                            let p = ((offset + y * 4 + dy) * w + 7 + i * 25 + x * 4 + dx) * 3;
                            pixels[p..p + 3].copy_from_slice(&[35, 55, 90]);
                        }
                    }
                }
            }
        }
    }
    for _ in 0..250 {
        let p = OsRng.gen_range(0..w * h) * 3;
        pixels[p..p + 3].copy_from_slice(&[100, 130, 150]);
    }
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, w as u32, h as u32);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|_| ApiError::internal("Captcha encoding failed"))?;
        writer
            .write_image_data(&pixels)
            .map_err(|_| ApiError::internal("Captcha encoding failed"))?;
    }
    state.db.collection::<Document>("verification").insert_one(doc!{"id":format!("captcha:{info}"),"kind":"captcha","code_hash":code_hash(&code)?,"generation":core::id(),"expires_at":core::epoch()+300,"purge_at":mongodb::bson::DateTime::from_millis((core::epoch()+300)*1000),"attempts_left":1}).await?;
    Ok(
        json!({"info":info,"image":format!("data:image/png;base64,{}",STANDARD.encode(bytes)),"expires_in":300}),
    )
}
async fn create_user(state: &AppState, body: &Value, admin: bool) -> Result<Value> {
    let address = email(body, "email")?;
    if by_email(state, &address).await?.is_some() {
        return Err(ApiError::bad("Email already registered"));
    }
    let name = core::required(body, "name")?;
    if name.chars().count() > 64 {
        return Err(ApiError::bad("Name too long"));
    }
    let hash = hash_password(password(body, "password")?).await?;
    let user=state.insert("users",json!({"email":address,"name":name,"password_hash":hash,"admin":admin,"disabled":false,"auth_version":0,"signature":"","avatar":null,"has_avatar":false,"locale":{"id":"auto","name":"Automatic","intro":"Use browser locale"}})).await?;
    let settings = state.get("site_settings", "site").await?;
    if let Some(teams) = settings["auto_join_team_ids"].as_array() {
        for team in teams.iter().filter_map(Value::as_str) {
            if state
                .db
                .collection::<Document>("teams")
                .find_one(doc! {"id":team,"deleted_at":null})
                .await?
                .is_some()
            {
                state.db.collection::<Document>("memberships").update_one(doc!{"group_type":"team","group_id":team,"user_id":core::field(&user,"id")},doc!{"$setOnInsert":{"id":core::id(),"created_at":core::now(),"role_id":"team-member"}}).upsert(true).await?;
            }
        }
    }
    Ok(user)
}
pub async fn bootstrap(state: &AppState) -> Result<()> {
    secret()?;
    for (collection, key) in [
        ("users", "email"),
        ("users", "id"),
        ("sessions", "id"),
        ("verification", "id"),
        ("auth_rate_limits", "id"),
        ("site_settings", "id"),
        ("languages", "id"),
    ] {
        state
            .db
            .collection::<Document>(collection)
            .create_index(
                IndexModel::builder()
                    .keys(doc! {key:1})
                    .options(IndexOptions::builder().unique(true).build())
                    .build(),
            )
            .await?;
    }
    state
        .db
        .collection::<Document>("auth_rate_limits")
        .create_index(
            IndexModel::builder()
                .keys(doc! {"purge_at":1})
                .options(
                    IndexOptions::builder()
                        .expire_after(std::time::Duration::ZERO)
                        .build(),
                )
                .build(),
        )
        .await?;
    for collection in ["sessions", "verification"] {
        state
            .db
            .collection::<Document>(collection)
            .create_index(
                IndexModel::builder()
                    .keys(doc! {"purge_at":1})
                    .options(
                        IndexOptions::builder()
                            .expire_after(std::time::Duration::ZERO)
                            .build(),
                    )
                    .build(),
            )
            .await?;
    }
    state.db.collection::<Document>("site_settings").update_one(doc!{"id":"site"},doc!{"$setOnInsert":{"enable_whitelist":false,"whitelist_emails":[],"only_allow_admin_create_team":false,"auto_join_team_ids":[],"homepage":{"html":"","css":""}}}).upsert(true).await?;
    for (sort, &(code, en, local, tra, ocr, no_space)) in LANGUAGES.iter().enumerate() {
        state.db.collection::<Document>("languages").update_one(doc!{"id":code},doc!{"$setOnInsert":{"code":code,"name":local,"en_name":en,"lo_name":local,"i18n_name":en,"g_tra_code":tra,"g_ocr_code":ocr,"no_space":no_space,"sort":sort as i64}}).upsert(true).await?;
    }
    let address = core::env("ADMIN_EMAIL", "");
    let pass = core::env("ADMIN_PASSWORD", "");
    if address.is_empty() != pass.is_empty() {
        return Err(ApiError::internal(
            "ADMIN_EMAIL and ADMIN_PASSWORD must be set together",
        ));
    }
    if !address.is_empty() {
        let address = email(&json!({"email":address}), "email")?;
        if by_email(state, &address).await?.is_none() {
            create_user(
                state,
                &json!({"email":address,"name":"Administrator","password":pass}),
                true,
            )
            .await?;
        }
    }
    Ok(())
}
fn admin(r: &Request) -> Result<()> {
    if !r.is_admin() {
        return Err(ApiError::forbidden("Administrator required"));
    }
    Ok(())
}
fn ok() -> Value {
    json!({"message":"Success"})
}
pub async fn handle(r: &Request) -> Result<Option<Reply>> {
    let p: Vec<&str> = r.path.iter().map(String::as_str).collect();
    let m = &r.method;
    let s = &r.state;
    let value = match p.as_slice() {
        ["captchas"] if m == Method::POST => captcha(s).await?,
        ["confirm-email-codes"] | ["reset-password-codes"] if m == Method::POST => {
            check_captcha(r).await?;
            let address = email(&r.body, "email")?;
            if p[0] == "reset-password-codes" && by_email(s, &address).await?.is_none() {
                cooldown(s, &format!("mail:{address}"), 60).await?;
                json!({"wait":60})
            } else {
                send_code(
                    s,
                    if p[0] == "confirm-email-codes" {
                        "confirm_email"
                    } else {
                        "reset_password"
                    },
                    &address,
                )
                .await?
            }
        }
        ["reset-email-codes"] if m == Method::POST => {
            r.uid()?;
            send_code(
                s,
                "reset_email",
                core::field(r.user.as_ref().unwrap(), "email"),
            )
            .await?
        }
        ["users"] if m == Method::POST => {
            let address = email(&r.body, "email")?;
            let settings = s.get("site_settings", "site").await?;
            if settings["enable_whitelist"].as_bool().unwrap_or(false)
                && !settings["whitelist_emails"]
                    .as_array()
                    .map(|a| a.iter().any(|v| v.as_str() == Some(&address)))
                    .unwrap_or(false)
            {
                return Err(ApiError::forbidden("Registration restricted"));
            }
            password(&r.body, "password")?;
            core::required(&r.body, "name")?;
            check_code(
                s,
                "confirm_email",
                &address,
                &core::required(&r.body, "v_code")?,
            )
            .await?;
            let user = create_user(s, &r.body, false).await?;
            let mut response = token(s, &user).await?;
            response["user"] = decorate_user(s, &user, true).await?;
            response
        }
        ["users"] | ["admin", "users"] if m == Method::GET => {
            r.uid()?;
            let include_email = p[0] == "admin";
            if include_email {
                admin(r)?;
            }
            let word = r.q("word").to_lowercase();
            let values = s
                .find("users", doc! {})
                .await?
                .into_iter()
                .filter(|u| {
                    core::field(u, "name").to_lowercase().contains(&word)
                        || (include_email && core::field(u, "email").contains(&word))
                })
                .collect();
            let mut reply = Reply::list(values, r);
            if let Some(users) = reply.data.as_array_mut() {
                for user in users {
                    *user = decorate_user(s, user, include_email).await?;
                }
            }
            return Ok(Some(reply));
        }
        ["users", name] if m == Method::GET => {
            let user = s
                .find("users", doc! {"name":*name})
                .await?
                .into_iter()
                .next()
                .ok_or_else(|| ApiError::not_found("User not found"))?;
            decorate_user(s, &user, false).await?
        }
        ["user", "token"] if m == Method::POST => {
            check_captcha(r).await?;
            let address = email(&r.body, "email")?;
            attempt_limit(s, &format!("login:{address}"), 900, 20).await?;
            let user = by_email(s, &address)
                .await?
                .ok_or_else(|| ApiError::unauthorized("Invalid email or password"))?;
            if !matches_password(&user, core::field(&r.body, "password")).await? {
                return Err(ApiError::unauthorized("Invalid email or password"));
            }
            token(s, &user).await?
        }
        ["user", "token"] if m == Method::DELETE => {
            revoke(s, &r.uid()?).await?;
            ok()
        }
        ["user", "info"] if m == Method::GET => {
            r.uid()?;
            decorate_user(s, r.user.as_ref().unwrap(), true).await?
        }
        ["user", "info"] if m == Method::PUT => {
            let uid = r.uid()?;
            let mut fields = json!({});
            if r.body.get("name").is_some() {
                let name = core::required(&r.body, "name")?;
                if name.chars().count() > 64 {
                    return Err(ApiError::bad("Name too long"));
                }
                fields["name"] = json!(name);
            }
            if let Some(v) = r.body.get("signature") {
                let signature = v
                    .as_str()
                    .ok_or_else(|| ApiError::bad("Signature must be a string"))?;
                if signature.chars().count() > 500 {
                    return Err(ApiError::bad("Signature too long"));
                }
                fields["signature"] = json!(signature);
            }
            if let Some(v) = r.body.get("locale") {
                let locale = v
                    .as_str()
                    .ok_or_else(|| ApiError::bad("Locale must be a string"))?;
                let name = match locale {
                    "auto" => "Automatic",
                    "en" => "English",
                    "zh-CN" => "简体中文",
                    "zh-TW" => "繁體中文",
                    _ => return Err(ApiError::bad("Unsupported interface locale")),
                };
                fields["locale"] = json!({"id":locale,"name":name,"intro":if locale=="auto"{"Use browser locale"}else{name}});
            }
            if fields.as_object().unwrap().is_empty() {
                return Err(ApiError::bad("No profile fields supplied"));
            }
            json!({"message":"Success","user":decorate_user(s,&s.patch("users",&uid,fields).await?,true).await?})
        }
        ["user", "password"] if m == Method::PUT => {
            let uid = r.uid()?;
            let next = password(&r.body, "new_password")?;
            if !matches_password(
                r.user.as_ref().unwrap(),
                core::field(&r.body, "old_password"),
            )
            .await?
            {
                return Err(ApiError::bad("Incorrect password"));
            }
            change_password(s, &uid, next).await?;
            ok()
        }
        ["user", "password"] if m == Method::DELETE => {
            let address = email(&r.body, "email")?;
            let next = password(&r.body, "password")?;
            check_code(
                s,
                "reset_password",
                &address,
                &core::required(&r.body, "v_code")?,
            )
            .await?;
            let user = by_email(s, &address)
                .await?
                .ok_or_else(|| ApiError::bad("Account unavailable"))?;
            let uid = core::field(&user, "id");
            change_password(s, uid, next).await?;
            let user = s.get("users", uid).await?;
            let mut response = token(s, &user).await?;
            response["user"] = decorate_user(s, &user, true).await?;
            response
        }
        ["user", "email"] if m == Method::PUT => {
            let uid = r.uid()?;
            let address = email(&r.body, "new_email")?;
            if by_email(s, &address).await?.is_some() {
                return Err(ApiError::bad("Email already registered"));
            }
            check_code(
                s,
                "reset_email",
                core::field(r.user.as_ref().unwrap(), "email"),
                &core::required(&r.body, "old_email_v_code")?,
            )
            .await?;
            check_code(
                s,
                "confirm_email",
                &address,
                &core::required(&r.body, "new_email_v_code")?,
            )
            .await?;
            s.db.collection::<Document>("users").update_one(doc!{"id":&uid},doc!{"$set":{"email":address,"updated_at":core::now()},"$inc":{"auth_version":1}}).await?;
            s.db.collection::<Document>("sessions")
                .delete_many(doc! {"user_id":&uid})
                .await?;
            json!({"message":"Success; sign in again","user":decorate_user(s,&s.get("users",&uid).await?,true).await?})
        }
        ["admin", "users"] if m == Method::POST => {
            admin(r)?;
            decorate_user(s, &create_user(s, &r.body, false).await?, true).await?
        }
        ["admin", "users", uid] if m == Method::PUT => {
            admin(r)?;
            s.get("users", uid).await?;
            if let Some(v) = r.body.get("disabled") {
                if !v.is_boolean() {
                    return Err(ApiError::bad("Boolean disabled status required"));
                }
                if *uid == r.uid()? {
                    return Err(ApiError::bad("Cannot change your own disabled status"));
                }
            }
            let mut fields = doc! {};
            if r.body.get("password").is_some() {
                fields.insert(
                    "password_hash",
                    hash_password(password(&r.body, "password")?).await?,
                );
            }
            if let Some(disabled) = r.body["disabled"].as_bool() {
                fields.insert("disabled", disabled);
            }
            if fields.is_empty() {
                return Err(ApiError::bad("Password or disabled status required"));
            }
            admin_update(r, uid, fields).await?;
            decorate_user(s, &s.get("users", uid).await?, true).await?
        }
        ["admin", "admin-status"] if m == Method::PUT => {
            admin(r)?;
            let uid = core::required(&r.body, "user_id")?;
            let status = r.body["status"]
                .as_bool()
                .ok_or_else(|| ApiError::bad("Boolean status required"))?;
            if uid == r.uid()? && !status {
                return Err(ApiError::bad("Cannot remove your own administrator access"));
            }
            admin_update(r, &uid, doc! {"admin":status}).await?;
            ok()
        }
        ["admin", "v-codes"] if m == Method::GET => {
            admin(r)?;
            let codes=s.find("verification",doc!{"kind":{"$ne":"captcha"},"expires_at":{"$gt":core::epoch()}}).await?.into_iter().map(|v|json!({"id":v["id"],"kind":v["kind"],"info":v["email"],"expires":v["expires_at"],"attempts_left":v["attempts_left"],"send_time":v["send_time"]})).collect();
            return Ok(Some(Reply::list(codes, r)));
        }
        ["admin", "site-setting"] if m == Method::GET => {
            admin(r)?;
            s.get("site_settings", "site").await?
        }
        ["admin", "site-setting"] if m == Method::PUT => {
            admin(r)?;
            let mut fields = json!({});
            for key in ["enable_whitelist", "only_allow_admin_create_team"] {
                if let Some(v) = r.body.get(key) {
                    if !v.is_boolean() {
                        return Err(ApiError::bad("Boolean setting required"));
                    }
                    fields[key] = v.clone();
                }
            }
            for key in ["whitelist_emails", "auto_join_team_ids"] {
                if let Some(v) = r.body.get(key) {
                    let array = v
                        .as_array()
                        .ok_or_else(|| ApiError::bad("Array setting required"))?;
                    if array.len() > 1000 {
                        return Err(ApiError::bad("Setting list too long"));
                    }
                    let mut normalized = Vec::new();
                    for entry in array {
                        let value = entry
                            .as_str()
                            .ok_or_else(|| ApiError::bad("String setting required"))?;
                        if key == "whitelist_emails" {
                            normalized
                                .push(Value::String(email(&json!({"email":value}), "email")?));
                        } else {
                            s.get("teams", value).await?;
                            normalized.push(entry.clone());
                        }
                    }
                    fields[key] = Value::Array(normalized);
                }
            }
            if let Some(home) = r.body.get("homepage") {
                let html = home["html"]
                    .as_str()
                    .ok_or_else(|| ApiError::bad("Homepage html required"))?;
                let css = home["css"]
                    .as_str()
                    .ok_or_else(|| ApiError::bad("Homepage css required"))?;
                if html.len() + css.len() > 262144 {
                    return Err(ApiError::bad("Homepage too large"));
                }
                fields["homepage"] = json!({"html":html,"css":css});
            }
            s.patch("site_settings", "site", fields).await?
        }
        ["site", "homepage"] if m == Method::GET => {
            s.get("site_settings", "site").await?["homepage"].clone()
        }
        ["languages"] if m == Method::GET => {
            let mut values = s.find("languages", doc! {}).await?;
            values.sort_by_key(|v| v["sort"].as_i64().unwrap_or(0));
            Value::Array(values)
        }
        ["languages", id] if m == Method::GET => s.get("languages", id).await?,
        ["captchas"]
        | ["confirm-email-codes"]
        | ["reset-password-codes"]
        | ["reset-email-codes"]
        | ["users"]
        | ["users", _]
        | ["user", "token"]
        | ["user", "info"]
        | ["user", "password"]
        | ["user", "email"]
        | ["admin", "users"]
        | ["admin", "users", _]
        | ["admin", "admin-status"]
        | ["admin", "v-codes"]
        | ["admin", "site-setting"]
        | ["site", "homepage"]
        | ["languages"]
        | ["languages", _] => return Err(ApiError::bad("Unsupported method")),
        _ => return Ok(None),
    };
    Ok(Some(value.into()))
}

const LANGUAGES: &[(&str, &str, &str, &str, &str, bool)] = &[
    ("ja", "Japanese", "日语", "ja", "ja", true),
    ("en", "English", "英语", "en", "en", false),
    ("ko", "Korean", "韩语", "ko", "ko", true),
    (
        "zh-CN",
        "Chinese (Simplified)",
        "中文（简体）",
        "zh-CN",
        "zh",
        true,
    ),
    (
        "zh-TW",
        "Chinese (Traditional)",
        "中文（繁体）",
        "zh-TW",
        "zh",
        true,
    ),
    ("af", "Afrikaans", "南非荷兰语", "af", "af", false),
    ("sq", "Albanian", "阿尔巴尼亚语", "sq", "", false),
    ("am", "Amharic", "阿姆哈拉语", "am", "", false),
    ("as", "Assamese", "阿萨姆语", "", "as", false),
    ("ar", "Arabic", "阿拉伯语", "ar", "ar", false),
    ("hy", "Armenian", "亚美尼亚语", "hy", "", false),
    ("az", "Azerbaijani", "阿塞拜疆语", "az", "az", false),
    ("eu", "Basque", "巴斯克语", "eu", "", false),
    ("be", "Belarusian", "白俄罗斯语", "be", "be", false),
    ("bn", "Bengali", "孟加拉语", "bn", "bn", false),
    ("bs", "Bosnian", "波斯尼亚语", "bs", "", false),
    ("bg", "Bulgarian", "保加利亚语", "bg", "bg", false),
    ("ca", "Catalan", "加泰罗尼亚语", "ca", "ca", false),
    ("ceb", "Cebuano", "宿务语", "ceb", "", false),
    ("co", "Corsican", "科西嘉语", "co", "", false),
    ("hr", "Croatian", "克罗地亚语", "hr", "hr", false),
    ("cs", "Czech", "捷克语", "cs", "cs", false),
    ("da", "Danish", "丹麦语", "da", "da", false),
    ("nl", "Dutch", "荷兰语", "nl", "nl", false),
    ("eo", "Esperanto", "世界语", "eo", "", false),
    ("et", "Estonian", "爱沙尼亚语", "et", "et", false),
    ("fi", "Finnish", "芬兰语", "fi", "fi", false),
    ("fr", "French", "法语", "fr", "fr", false),
    ("fy", "Frisian", "弗里斯兰语", "fy", "", false),
    ("gl", "Galician", "加利西亚语", "gl", "", false),
    ("ka", "Georgian", "格鲁吉亚语", "ka", "", false),
    ("de", "German", "德语", "de", "de", false),
    ("el", "Greek", "希腊语", "el", "el", true),
    ("gu", "Gujarati", "古吉拉特语", "gu", "", false),
    ("ht", "Haitian Creole", "海地克里奥尔语", "ht", "", false),
    ("ha", "Hausa", "豪萨语", "ha", "", false),
    ("haw", "Hawaiian", "夏威夷语", "haw", "", false),
    ("iw", "Hebrew", "希伯来语", "iw", "iw", false),
    ("hi", "Hindi", "印地语", "hi", "hi", false),
    ("hmn", "Hmong", "苗语", "hmn", "", false),
    ("hu", "Hungarian", "匈牙利语", "hu", "hu", false),
    ("is", "Icelandic", "冰岛语", "is", "is", false),
    ("ig", "Igbo", "伊博语", "ig", "", false),
    ("id", "Indonesian", "印度尼西亚语", "id", "id", false),
    ("ga", "Irish", "爱尔兰语", "ga", "", false),
    ("it", "Italian", "意大利语", "it", "it", false),
    ("jw", "Javanese", "爪哇语", "jw", "", true),
    ("kn", "Kannada", "卡纳达语", "kn", "", false),
    ("kk", "Kazakh", "哈萨克语", "kk", "kk", false),
    ("km", "Khmer", "高棉语", "km", "", true),
    ("ku", "Kurdish", "库尔德语", "ku", "", false),
    ("ky", "Kyrgyz", "吉尔吉斯语", "ky", "ky", false),
    ("lo", "Lao", "老挝语", "lo", "", true),
    ("la", "Latin", "拉丁语", "la", "", true),
    ("lv", "Latvian", "拉脱维亚语", "lv", "lv", false),
    ("lt", "Lithuanian", "立陶宛语", "lt", "lt", false),
    ("lb", "Luxembourgish", "卢森堡语", "lb", "", false),
    ("mk", "Macedonian", "马其顿语", "mk", "mk", false),
    ("mg", "Malagasy", "马尔加什语", "mg", "", false),
    ("ms", "Malay", "马来语", "ms", "", false),
    ("ml", "Malayalam", "马拉雅拉姆语", "ml", "", false),
    ("mt", "Maltese", "马耳他语", "mt", "", false),
    ("mi", "Maori", "毛利语", "mi", "", false),
    ("mr", "Marathi", "马拉地语", "mr", "mr", false),
    ("mn", "Mongolian", "蒙古语", "mn", "mn", false),
    ("my", "Myanmar (Burmese)", "缅甸语", "my", "", true),
    ("ne", "Nepali", "尼泊尔语", "ne", "ne", false),
    ("no", "Norwegian", "挪威语", "no", "no", false),
    (
        "ny",
        "Nyanja (Chichewa)",
        "齐切瓦语 (尼扬贾语)",
        "ny",
        "",
        false,
    ),
    ("ps", "Pashto", "普什图语", "ps", "ps", false),
    ("fa", "Persian", "波斯语", "fa", "fa", false),
    ("pl", "Polish", "波兰语", "pl", "pl", false),
    (
        "pt",
        "Portuguese (Portugal, Brazil)",
        "葡萄牙语",
        "pt",
        "pt",
        false,
    ),
    ("pa", "Punjabi", "旁遮普语", "pa", "", false),
    ("ro", "Romanian", "罗马尼亚语", "ro", "ro", false),
    ("ru", "Russian", "俄语", "ru", "ru", false),
    ("sm", "Samoan", "萨摩亚语", "sm", "", false),
    ("gd", "Scots Gaelic", "苏格兰盖尔语", "gd", "", false),
    ("sa", "Sanskrit", "梵文", "", "sa", false),
    ("sr", "Serbian", "塞尔维亚语", "sr", "sr", false),
    ("st", "Sesotho", "塞索托语", "st", "", false),
    ("sn", "Shona", "绍纳语", "sn", "", false),
    ("sd", "Sindhi", "信德语", "sd", "", false),
    ("si", "Sinhala (Sinhalese)", "僧伽罗语", "si", "", false),
    ("sk", "Slovak", "斯洛伐克语", "sk", "sk", false),
    ("sl", "Slovenian", "斯洛文尼亚语", "sl", "sl", false),
    ("so", "Somali", "索马里语", "so", "", false),
    ("es", "Spanish", "西班牙语", "es", "es", false),
    ("su", "Sundanese", "巽他语", "su", "", false),
    ("sw", "Swahili", "斯瓦希里语", "sw", "", false),
    ("sv", "Swedish", "瑞典语", "sv", "sv", false),
    (
        "tl",
        "Tagalog (Filipino)",
        "他加禄语（菲律宾语）",
        "tl",
        "tl",
        false,
    ),
    ("tg", "Tajik", "塔吉克语", "tg", "", false),
    ("ta", "Tamil", "泰米尔语", "ta", "ta", false),
    ("te", "Telugu", "泰卢固语", "te", "", false),
    ("th", "Thai", "泰语", "th", "th", false),
    ("tr", "Turkish", "土耳其语", "tr", "tr", false),
    ("uk", "Ukrainian", "乌克兰语", "uk", "uk", false),
    ("ur", "Urdu", "乌尔都语", "ur", "ur", false),
    ("uz", "Uzbek", "乌兹别克语", "uz", "uz", false),
    ("vi", "Vietnamese", "越南语", "vi", "vi", false),
    ("cy", "Welsh", "威尔士语", "cy", "", false),
    ("xh", "Xhosa", "科萨语", "xh", "", false),
    ("yi", "Yiddish", "意第绪语", "yi", "", false),
    ("yo", "Yoruba", "约鲁巴语", "yo", "", false),
    ("zu", "Zulu", "祖鲁语", "zu", "", false),
];

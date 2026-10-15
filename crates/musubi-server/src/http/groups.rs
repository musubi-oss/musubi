use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::{AccountId, GroupId};
use musubi_social::{
    CreateGroup, Group, GroupFilter, GroupMember, GroupService, MemberState, Page, UpdateGroup,
    UserGroup,
};
use serde::Deserialize;
use serde_json::json;

use crate::app::AppState;
use crate::http::{ApiError, Bearer};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/groups", get(list).post(create))
        .route("/v1/groups/{id}", get(get_one).patch(update).delete(delete))
        .route("/v1/groups/{id}/join", post(join))
        .route("/v1/groups/{id}/leave", post(leave))
        .route("/v1/groups/{id}/add", post(add))
        .route("/v1/groups/{id}/kick", post(kick))
        .route("/v1/groups/{id}/promote", post(promote))
        .route("/v1/groups/{id}/demote", post(demote))
        .route("/v1/groups/{id}/members", get(members))
        .route("/v1/users/{id}/groups", get(user_groups))
}

fn groups(s: &AppState) -> &GroupService {
    &s.groups
}

fn state_of(v: Option<i16>) -> Result<Option<MemberState>, ApiError> {
    v.map(|v| MemberState::try_from(v).map_err(|e| musubi_core::Error::InvalidArgument(e).into()))
        .transpose()
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    lang: Option<String>,
    #[serde(default)]
    open: Option<bool>,
    #[serde(default)]
    members_min: Option<i32>,
    #[serde(default)]
    members_max: Option<i32>,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Deserialize)]
struct PageQuery {
    #[serde(default)]
    state: Option<i16>,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    cursor: Option<String>,
}

fn default_limit() -> usize {
    50
}

#[derive(Deserialize)]
struct Users {
    ids: Vec<AccountId>,
}

async fn list(
    State(s): State<AppState>,
    Bearer(_): Bearer,
    Query(q): Query<ListQuery>,
) -> Result<Json<Page<Group>>, ApiError> {
    let f = GroupFilter {
        name_prefix: q.name.as_deref(),
        lang: q.lang.as_deref(),
        open: q.open,
        members_min: q.members_min,
        members_max: q.members_max,
    };
    Ok(Json(
        groups(&s).list(f, q.limit, q.cursor.as_deref()).await?,
    ))
}

async fn create(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(c): Json<CreateGroup>,
) -> Result<Json<Group>, ApiError> {
    let g = groups(&s).create(me.account_id, c).await?;
    s.events
        .emit(
            "group.created",
            Some(me.account_id),
            json!({ "group_id": g.id, "name": g.name }),
        )
        .await;
    Ok(Json(g))
}

async fn get_one(
    State(s): State<AppState>,
    Bearer(_): Bearer,
    Path(id): Path<GroupId>,
) -> Result<Json<Group>, ApiError> {
    let g = groups(&s)
        .get(&[id])
        .await?
        .pop()
        .ok_or_else(|| musubi_core::Error::NotFound("group".into()))?;
    Ok(Json(g))
}

async fn update(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<GroupId>,
    Json(u): Json<UpdateGroup>,
) -> Result<Json<Group>, ApiError> {
    Ok(Json(groups(&s).update(me.account_id, id, u).await?))
}

async fn delete(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<GroupId>,
) -> Result<Json<serde_json::Value>, ApiError> {
    groups(&s).delete(me.account_id, id).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn join(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<GroupId>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let state = groups(&s).join(me.account_id, id).await?;
    s.events
        .emit(
            "group.joined",
            Some(me.account_id),
            json!({ "group_id": id, "state": state }),
        )
        .await;
    Ok(Json(json!({ "state": state })))
}

async fn leave(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<GroupId>,
) -> Result<Json<serde_json::Value>, ApiError> {
    groups(&s).leave(me.account_id, id).await?;
    s.events
        .emit("group.left", Some(me.account_id), json!({ "group_id": id }))
        .await;
    Ok(Json(json!({ "ok": true })))
}

macro_rules! moderation {
    ($name:ident) => {
        async fn $name(
            State(s): State<AppState>,
            Bearer(me): Bearer,
            Path(id): Path<GroupId>,
            Json(u): Json<Users>,
        ) -> Result<Json<serde_json::Value>, ApiError> {
            groups(&s).$name(me.account_id, id, &u.ids).await?;
            s.events
                .emit(
                    concat!("group.", stringify!($name)),
                    Some(me.account_id),
                    json!({ "group_id": id, "ids": u.ids }),
                )
                .await;
            Ok(Json(json!({ "ok": true })))
        }
    };
}
moderation!(add);
moderation!(kick);
moderation!(promote);
moderation!(demote);

async fn members(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<GroupId>,
    Query(q): Query<PageQuery>,
) -> Result<Json<Page<GroupMember>>, ApiError> {
    Ok(Json(
        groups(&s)
            .members(
                me.account_id,
                id,
                state_of(q.state)?,
                q.limit,
                q.cursor.as_deref(),
            )
            .await?,
    ))
}

async fn user_groups(
    State(s): State<AppState>,
    Bearer(_): Bearer,
    Path(id): Path<AccountId>,
    Query(q): Query<PageQuery>,
) -> Result<Json<Page<UserGroup>>, ApiError> {
    Ok(Json(
        groups(&s)
            .user_groups(id, state_of(q.state)?, q.limit, q.cursor.as_deref())
            .await?,
    ))
}

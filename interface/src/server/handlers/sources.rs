use crate::db::{models::User, queries::sources as sources_queries, Pool};
use crate::server::result::ApiError;
use actix_web::web::{Data, Json, Path, Query};
use feeder::aggregator::AggApp;
use feeder::models::Source;
use feeder::storage::pg::PgStorage;
use serde::Deserialize;
use std::sync::Arc;
use tg_collector::parsers::DefaultTelegramParser;

pub async fn get_list(db_pool: Data<Pool>, user: User) -> Result<Json<Vec<Source>>, ApiError> {
    Ok(Json(sources_queries::get_list(&db_pool, user.id).await?))
}

#[derive(Deserialize)]
pub struct SearchSource {
    origin: String,
}

pub async fn search(
    aggregator: Data<Arc<AggApp<PgStorage, DefaultTelegramParser>>>,
    query: Query<SearchSource>,
) -> Result<Json<Vec<Source>>, ApiError> {
    let sources = aggregator.search_source(query.origin.as_str()).await?;
    Ok(Json(sources))
}

pub async fn unsubscribe(
    db_pool: Data<Pool>,
    user: User,
    source_id: Path<i32>,
) -> Result<Json<()>, ApiError> {
    Ok(Json(
        sources_queries::unsubscribe(&db_pool, source_id.0, user.id).await?,
    ))
}

pub async fn subscribe(
    db_pool: Data<Pool>,
    user: User,
    source_id: Path<i32>,
) -> Result<Json<()>, ApiError> {
    Ok(Json(
        sources_queries::subscribe(&db_pool, source_id.0, user.id).await?,
    ))
}

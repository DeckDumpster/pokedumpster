//! `/api/orders` — purchase orders (PLAN.md §5.2, §9).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use pkdump_db::DbError;
use pkdump_db::orders::{self, NewOrder, Order, OrderDetail, OrderLine};

use crate::{AppError, AppState, blocking};

/// Build the order routes (mounted under `/api`).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/orders", get(list).post(create))
        .route("/orders/{id}", get(detail))
        .route("/orders/{id}/receive", post(receive))
}

/// Request body for committing an order.
#[derive(Deserialize)]
struct CreateOrder {
    order: NewOrder,
    lines: Vec<OrderLine>,
}

/// Result of receiving an order.
#[derive(Serialize)]
struct Received {
    received: usize,
}

async fn list(State(state): State<AppState>) -> Result<Json<Vec<Order>>, AppError> {
    Ok(Json(blocking(&state, |c| orders::list(c)).await?))
}

async fn create(
    State(state): State<AppState>,
    Json(body): Json<CreateOrder>,
) -> Result<(StatusCode, Json<OrderDetail>), AppError> {
    let detail = blocking(&state, move |c| {
        let id = orders::create(c, &body.order, &body.lines)?;
        orders::get_detail(c, id)?.ok_or_else(|| DbError::NotFound(format!("order {id}")))
    })
    .await?;
    Ok((StatusCode::CREATED, Json(detail)))
}

async fn detail(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<OrderDetail>, AppError> {
    let detail = blocking(&state, move |c| {
        orders::get_detail(c, id)?.ok_or_else(|| DbError::NotFound(format!("order {id}")))
    })
    .await?;
    Ok(Json(detail))
}

async fn receive(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Received>, AppError> {
    let received = blocking(&state, move |c| orders::receive(c, id)).await?;
    Ok(Json(Received { received }))
}

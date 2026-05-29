//! `/api/user-printings` — the "Missing Variant" escape hatch (decision
//! pokedumpster-x7k). A single POST creates a user_printings row, adds
//! N collection copies pointing at it, and optionally records the first
//! manual_prices entry — all in one transaction.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use pkdump_db::DbError;
use pkdump_db::collection::{self, CollectionRow, NewCopy};
use pkdump_db::manual_prices::{self, NewManualPrice};
use pkdump_db::user_printings::{self, NewUserPrinting, UserPrinting};

use crate::{AppError, AppState, blocking};

#[derive(Debug, Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct CreateMissingVariant {
    pub card_id: String,
    pub description: Option<String>,
    /// Number of physical copies to add to the collection in this submit.
    /// 0 is allowed — creates the variant slot only.
    #[ts(type = "number")]
    pub qty: i64,
    /// Optional first manual-price entry. Recorded once if `price` is set.
    pub price: Option<f64>,
    pub observed_at: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct CreateMissingVariantResult {
    pub user_printing: UserPrinting,
    pub copies: Vec<CollectionRow>,
}

/// Build the user-printings routes (mounted under `/api`).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/user-printings", post(create))
        .route("/user-printings/{printing_id}", delete(remove))
}

async fn create(
    State(state): State<AppState>,
    Json(body): Json<CreateMissingVariant>,
) -> Result<(StatusCode, Json<CreateMissingVariantResult>), AppError> {
    if body.qty < 0 {
        return Err(AppError::from(DbError::Conflict("qty must be >= 0".into())));
    }
    let result = blocking(&state, move |c| {
        let user_printing = user_printings::insert(
            c,
            &NewUserPrinting {
                card_id: body.card_id.clone(),
                description: body.description.clone(),
            },
        )?;
        let mut copies = Vec::with_capacity(body.qty as usize);
        for _ in 0..body.qty {
            let id = collection::add(
                c,
                &NewCopy {
                    printing_id: user_printing.printing_id.clone(),
                    source: "missing_variant".into(),
                    ..Default::default()
                },
            )?;
            // Fetch the just-inserted row in display form so the client
            // can splice it into its collection-view state without a
            // second round trip.
            if let Some(row) = collection::get_row(c, id)? {
                copies.push(row);
            }
        }
        if let Some(price) = body.price {
            manual_prices::insert(
                c,
                &NewManualPrice {
                    printing_id: user_printing.printing_id.clone(),
                    price,
                    observed_at: body.observed_at.clone(),
                    note: body.note.clone(),
                },
            )?;
        }
        Ok(CreateMissingVariantResult {
            user_printing,
            copies,
        })
    })
    .await?;
    Ok((StatusCode::CREATED, Json(result)))
}

async fn remove(
    State(state): State<AppState>,
    Path(printing_id): Path<String>,
) -> Result<StatusCode, AppError> {
    let ok = blocking(&state, move |c| user_printings::delete(c, &printing_id)).await?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::from(DbError::NotFound("user_printing".into())))
    }
}

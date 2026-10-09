use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: Uuid,
    pub merchant_id: Option<Uuid>,
    pub session_id: Uuid,
    pub exp: usize,
    pub iat: usize,
}

pub const TOKEN_TTL_HOURS: i64 = 24;

pub fn sign(secret: &str, user_id: Uuid, merchant_id: Option<Uuid>) -> Result<(String, Uuid), jsonwebtoken::errors::Error> {
    let now = Utc::now();
    let session_id = Uuid::new_v4();
    let claims = Claims {
        sub: user_id,
        merchant_id,
        session_id,
        iat: now.timestamp() as usize,
        exp: (now + Duration::hours(TOKEN_TTL_HOURS)).timestamp() as usize,
    };
    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )?;
    Ok((token, session_id))
}

pub fn verify(secret: &str, token: &str) -> Result<Claims, jsonwebtoken::errors::Error> {
    decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )
    .map(|d| d.claims)
}

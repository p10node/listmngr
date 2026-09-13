//! Whole-day compatibility for known bounce intervals only.
use super::{ApiError, ApiFlavor, ApiResult, Error};
use serde_json::{Value, json};
const INTERVALS: [&str; 2] = [
    "bounce_info_stale_after",
    "bounce_you_are_disabled_warnings_interval",
];

pub fn project(flavor: ApiFlavor, mut value: Value) -> Value {
    if matches!(flavor, ApiFlavor::Compat31) {
        for key in INTERVALS {
            if let Some(days) = value[key].as_i64() {
                value[key] = json!(format!("{days}d"));
            }
        }
    }
    value
}

pub fn normalize(flavor: ApiFlavor, value: &mut Value) -> ApiResult<()> {
    if matches!(flavor, ApiFlavor::Compat31) {
        for key in INTERVALS {
            if let Some(Value::String(text)) = value.get(key) {
                if let Some(days) = text.strip_suffix('d') {
                    if days.is_empty() || !days.bytes().all(|byte| byte.is_ascii_digit()) {
                        return Err(ApiError(Error::Validation(key.into())));
                    }
                    let days = days
                        .parse::<i64>()
                        .map_err(|_| ApiError(Error::Validation(key.into())))?;
                    value[key] = json!(days);
                }
            }
        }
    }
    Ok(())
}

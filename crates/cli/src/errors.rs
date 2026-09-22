use listmngr_core::Error as CoreError;

#[derive(Debug)]
pub struct MigrationFailure;
impl std::fmt::Display for MigrationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("database migration failed")
    }
}
impl std::error::Error for MigrationFailure {}

pub fn classify(error: &anyhow::Error) -> (u8, &'static str, &'static str) {
    if let Some(error) = error.downcast_ref::<crate::status::Failure>() {
        return match error {
            crate::status::Failure::Unreachable => {
                (3, "CLI-STATUS-UNREACHABLE", "service is unreachable")
            }
            crate::status::Failure::Unhealthy => {
                (4, "CLI-STATUS-UNHEALTHY", "service is unhealthy")
            }
            crate::status::Failure::NotReady => (
                5,
                "CLI-STATUS-NOT-READY",
                "service is healthy but not ready",
            ),
        };
    }
    if error.downcast_ref::<MigrationFailure>().is_some() {
        return (10, "CLI-MIGRATION", "database migration failed");
    }
    if let Some(error) = error.downcast_ref::<CoreError>() {
        return match error {
            CoreError::Config(_) | CoreError::InvalidListId(_) | CoreError::Validation(_) => {
                (2, "CLI-VALIDATION", "invalid input")
            }
            CoreError::Conflict(_) => (6, "CLI-CONFLICT", "operation conflicts with existing data"),
            CoreError::NotFound(_) => (7, "CLI-NOT-FOUND", "resource not found"),
            CoreError::Authentication | CoreError::Forbidden(_) => {
                (8, "CLI-AUTH", "authentication or authorization failed")
            }
            CoreError::RateLimited { .. } => (8, "CLI-RATE-LIMITED", "rate limit exceeded"),
            CoreError::Database(_) => (10, "CLI-DATABASE", "database operation failed"),
        };
    }
    if let Some(error) = error.downcast_ref::<listmngr_import::Error>() {
        return match error {
            listmngr_import::Error::Rest(_) | listmngr_import::Error::Database(_) => (
                11,
                "CLI-IMPORT-SOURCE",
                "the Mailman site could not be read",
            ),
            listmngr_import::Error::Pickle(_) => (2, "CLI-VALIDATION", "invalid input"),
            listmngr_import::Error::Core(_) => (1, "CLI-INTERNAL", "operation failed"),
        };
    }
    if error.downcast_ref::<std::io::Error>().is_some() {
        return (9, "CLI-IO", "input/output operation failed");
    }
    (1, "CLI-INTERNAL", "operation failed")
}

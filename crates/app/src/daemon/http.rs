//! What every `*_api` module shares: running a job on a blocking thread, and
//! turning a failure into an [`HttpError`]. One definition of each, so the
//! codes, the statuses, and the texts of these answers cannot drift apart.

use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::http::StatusCode;
use marsa_core::codes;
use marsa_core::config::ConfigError;
use tokio::task::JoinError;

use super::api::HttpError;

/// Runs `job` on a blocking thread, for work that touches the disk or takes a
/// `std::sync` lock that must not be held across `.await`. `what` names the
/// job in the answer and the log: a job that panics or is cancelled gives 500
/// `internal_error` with "the `what` job failed".
pub(crate) async fn blocking<T, F>(what: &'static str, job: F) -> Result<T, HttpError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, HttpError> + Send + 'static,
{
    tokio::task::spawn_blocking(job)
        .await
        .unwrap_or_else(|err| Err(job_failed(what, &err)))
}

/// The answer for a task that did not end: it panicked or was cancelled.
pub(crate) fn job_failed(what: &str, err: &JoinError) -> HttpError {
    tracing::error!(error = %err, "the {what} job failed");
    HttpError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        codes::INTERNAL_ERROR,
        format!("the {what} job failed"),
    )
}

/// An error answer with the `code` of the route and the `status` and `text`
/// of an extractor's rejection.
pub(crate) fn rejected(code: &'static str, status: StatusCode, text: String) -> HttpError {
    HttpError::new(status, code, text)
}

/// A request part that axum could not read (a path, a query, a JSON body):
/// the answer keeps axum's status and text, and gets the `code` of the route.
pub(crate) trait Rejected {
    fn reject(self, code: &'static str) -> HttpError;
}

macro_rules! rejected_by_axum {
    ($($rejection:ty),+) => {
        $(impl Rejected for $rejection {
            fn reject(self, code: &'static str) -> HttpError {
                rejected(code, self.status(), self.body_text())
            }
        })+
    };
}

rejected_by_axum!(JsonRejection, PathRejection, QueryRejection);

/// `global.toml` cannot be read or written: the request was fine, the file or
/// the disk is not.
pub(crate) fn global_error(err: &ConfigError) -> HttpError {
    tracing::warn!(code = err.code(), error = %err, "global.toml");
    HttpError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        err.code(),
        err.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Json;
    use axum::extract::Path;
    use axum::extract::{FromRequest, FromRequestParts};
    use axum::http::Request;

    #[tokio::test]
    async fn a_job_gives_its_own_answer_or_its_error() {
        let ok = blocking("test", || Ok(7)).await;
        assert_eq!(ok.unwrap(), 7);
        let err = blocking::<(), _>("test", || {
            Err(HttpError::new(StatusCode::CONFLICT, "busy", "no"))
        })
        .await
        .unwrap_err();
        assert_eq!((err.status, err.code), (StatusCode::CONFLICT, "busy"));
    }

    #[tokio::test]
    async fn a_job_that_panics_is_an_internal_error_named_after_it() {
        let err = blocking::<(), _>("sites", || panic!("on purpose"))
            .await
            .unwrap_err();
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(err.code, codes::INTERNAL_ERROR);
        assert_eq!(err.message, "the sites job failed");
    }

    #[tokio::test]
    async fn a_rejection_keeps_the_status_and_text_of_axum_and_the_code_of_the_route() {
        let request = Request::builder()
            .header("content-type", "application/json")
            .body(axum::body::Body::from("{"))
            .unwrap();
        let rejection = Json::<serde_json::Value>::from_request(request, &())
            .await
            .unwrap_err();
        let (status, text) = (rejection.status(), rejection.body_text());
        let err = rejection.reject(codes::INVALID_BODY);
        assert_eq!(
            (err.status, err.code, err.message),
            (status, codes::INVALID_BODY, text)
        );

        let (mut parts, ()) = Request::new(()).into_parts();
        let rejection = Path::<String>::from_request_parts(&mut parts, &())
            .await
            .unwrap_err();
        let status = rejection.status();
        let err = rejection.reject(codes::INVALID_PATH);
        assert_eq!((err.status, err.code), (status, codes::INVALID_PATH));
    }

    #[test]
    fn a_config_failure_is_a_500_with_the_code_of_the_error() {
        let err = global_error(&ConfigError::Invalid {
            path: "global.toml".into(),
            message: "bad".into(),
        });
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(err.code, codes::CONFIG_INVALID);
    }
}

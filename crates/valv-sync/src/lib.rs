//! Valv sync engine library.

pub mod chunking;
pub mod config;
pub mod persistence;
pub mod protocol;
pub mod storage;
pub mod sync_engine;
pub mod update;
pub mod watch;

pub fn api_base(backend_url: &str) -> String {
    format!("{}/api", backend_url.trim_end_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_base_strips_trailing_slash() {
        assert_eq!(
            api_base("http://localhost:4747/"),
            "http://localhost:4747/api"
        );
    }

    #[test]
    fn api_base_handles_no_trailing_slash() {
        assert_eq!(
            api_base("http://localhost:4747"),
            "http://localhost:4747/api"
        );
    }

    #[test]
    fn api_base_handles_https_origin() {
        assert_eq!(api_base("https://api.valv.dev"), "https://api.valv.dev/api");
    }
}

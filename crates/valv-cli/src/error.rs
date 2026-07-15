use std::{
    fmt,
    io::{IsTerminal, Write},
    process::ExitCode,
};

use anyhow::Result;
use serde::Serialize;

pub(crate) const EX_FAILURE: u8 = 1;
pub(crate) const EX_USAGE: u8 = 2;
pub(crate) const EX_TEMPFAIL: u8 = 75;
pub(crate) const EX_NOPERM: u8 = 77;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ErrorPayload {
    pub(crate) code: &'static str,
    pub(crate) message: String,
    pub(crate) hint: Option<String>,
    pub(crate) scope: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ErrorEnvelope<'a> {
    error: &'a ErrorPayload,
}

#[derive(Debug, Clone)]
pub(crate) struct CliError {
    pub(crate) exit_code: u8,
    pub(crate) payload: ErrorPayload,
}

#[allow(dead_code)]
impl CliError {
    pub(crate) fn new(exit_code: u8, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            exit_code,
            payload: ErrorPayload {
                code,
                message: message.into(),
                hint: None,
                scope: None,
            },
        }
    }

    pub(crate) fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.payload.hint = Some(hint.into());
        self
    }

    pub(crate) fn with_scope(mut self, scope: impl Into<String>) -> Self {
        self.payload.scope = Some(scope.into());
        self
    }

    pub(crate) fn usage(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(EX_USAGE, code, message)
    }

    pub(crate) fn refused(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(EX_NOPERM, code, message)
    }

    pub(crate) fn mount_source_required() -> Self {
        Self::usage("mount_source_required", "mount needs exactly one source.").with_hint(
            "--folder <id|name>  attach a folder you can reach\n--key <token>       redeem an access key\n--new               create one from this path",
        )
    }

    pub(crate) fn share_read_only_requires_target() -> Self {
        Self::usage(
            "share_read_only_requires_target",
            "--read-only requires a target: pass --to <email> or --key <name>.",
        )
        .with_hint(
            "share <path> with no flags lists existing grants; --read-only only makes sense when granting access with --to or --key.",
        )
    }

    pub(crate) fn handle_requires_pinned_id() -> Self {
        Self::usage(
            "handle_requires_pinned_id",
            "--json requires --id for a destructive command; a handle is a query, not a pinned reference.",
        )
    }

    pub(crate) fn daemon_not_running() -> Self {
        Self::new(
            EX_TEMPFAIL,
            "daemon_not_running",
            "The Valv daemon is not running.",
        )
        .with_hint("Run any valv command to start it, or run `valv daemon restart`.")
    }

    pub(crate) fn daemon_failed_to_start(detail: impl Into<String>) -> Self {
        Self::new(EX_FAILURE, "daemon_failed_to_start", detail)
    }

    pub(crate) fn not_configured() -> Self {
        Self::new(
            EX_FAILURE,
            "not_configured",
            "Not configured. Run: valv login, or valv mount <path> --key <token> if you were given an access key.",
        )
    }

    pub(crate) fn no_credential() -> Self {
        Self::new(
            EX_FAILURE,
            "no_credential",
            "This machine has no Valv credential. Run: valv login, or valv mount <path> --key <token>.",
        )
    }

    pub(crate) fn backend_unreachable(detail: impl Into<String>) -> Self {
        Self::new(EX_TEMPFAIL, "backend_unreachable", detail)
    }

    pub(crate) fn path_not_mounted(path: impl Into<String>) -> Self {
        let path = path.into();
        Self::new(
            EX_FAILURE,
            "path_not_mounted",
            format!("{path} is not inside any mounted folder."),
        )
    }

    pub(crate) fn path_not_in_mirror(path: impl Into<String>) -> Self {
        let path = path.into();
        Self::new(
            EX_FAILURE,
            "path_not_in_mirror",
            format!("{path} is not present in the local mirror."),
        )
    }

    pub(crate) fn folder_not_found(handle: impl Into<String>) -> Self {
        Self::new(
            EX_FAILURE,
            "folder_not_found",
            format!("Folder not found: {}.", handle.into()),
        )
    }

    pub(crate) fn grant_not_found(handle: impl Into<String>) -> Self {
        Self::new(
            EX_FAILURE,
            "grant_not_found",
            format!("No matching grant for {}.", handle.into()),
        )
    }

    pub(crate) fn ambiguous_grant_handle(matches_description: impl Into<String>) -> Self {
        Self::new(
            EX_FAILURE,
            "ambiguous_grant_handle",
            matches_description.into(),
        )
        .with_hint("Pass --id <id> to choose one.")
    }

    pub(crate) fn access_key_cannot_create_folder() -> Self {
        Self::refused(
            "access_key_cannot_create_folder",
            "An access key cannot create a folder.",
        )
        .with_hint("Ask the folder owner to create it, or attach a folder they already shared with `valv mount <path> --key <token>`.")
    }

    pub(crate) fn access_key_cannot_mount_folder() -> Self {
        Self::refused(
            "access_key_cannot_mount_folder",
            "An access key cannot mount a folder by id.",
        )
        .with_hint("Ask the folder owner for an access key, then run `valv mount <path> --key <token>`.")
    }

    pub(crate) fn access_key_cannot_issue_keys() -> Self {
        Self::refused(
            "access_key_cannot_issue_keys",
            "An access key cannot issue another access key.",
        )
        .with_hint("Ask the folder owner to create it with `valv share <path> --key <name>`.")
    }

    pub(crate) fn access_key_cannot_invite_people() -> Self {
        Self::refused(
            "access_key_cannot_invite_people",
            "An access key cannot invite people to a folder.",
        )
        .with_hint("Ask the folder owner to invite them with `valv share <path> --to <email>`.")
    }

    pub(crate) fn access_key_cannot_revoke() -> Self {
        Self::refused(
            "access_key_cannot_revoke",
            "An access key cannot revoke access, including its own.",
        )
        .with_hint("Ask the folder owner to revoke it with `valv unshare <path> …`.")
    }

    pub(crate) fn access_key_is_read_only(folder: impl Into<String>) -> Self {
        let folder = folder.into();
        Self::refused(
            "access_key_is_read_only",
            format!("This access key is read-only for {folder} and cannot restore a version."),
        )
        .with_hint("Ask the folder owner for a read/write key, or ask them to restore it.")
    }

    pub(crate) fn sync_timed_out(detail: impl Into<String>) -> Self {
        Self::new(EX_TEMPFAIL, "sync_timed_out", detail)
    }

    pub(crate) fn sync_mount_error(mount: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(
            EX_FAILURE,
            "sync_mount_error",
            format!("{}: {}", mount.into(), detail.into()),
        )
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "error: {}", self.payload.message)?;
        if let Some(hint) = &self.payload.hint {
            let mut lines = hint.split('\n');
            if let Some(first) = lines.next() {
                write!(f, "\nhint:  {first}")?;
            }
            for line in lines {
                write!(f, "\n       {line}")?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for CliError {}

fn resolve(error: &anyhow::Error) -> CliError {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<CliError>())
        .cloned()
        .unwrap_or_else(|| CliError::new(EX_FAILURE, "internal_error", format!("{error:#}")))
}

pub(crate) fn report(error: &anyhow::Error, json: bool) -> ExitCode {
    let cli_error = resolve(error);
    if json {
        let envelope = ErrorEnvelope {
            error: &cli_error.payload,
        };
        eprintln!(
            "{}",
            serde_json::to_string(&envelope).unwrap_or_else(|_| "{\"error\":{\"code\":\"internal_error\",\"message\":\"failed to render error\"}}".to_owned())
        );
    } else {
        eprintln!("{cli_error}");
    }
    ExitCode::from(cli_error.exit_code)
}

pub(crate) fn confirm(prompt: &str, assume_yes: bool) -> Result<()> {
    if assume_yes {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        return Err(CliError::new(
            EX_FAILURE,
            "confirmation_required",
            "Refusing a destructive action without --yes in a non-interactive session.",
        )
        .into());
    }
    eprint!("{prompt} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_err() {
        return Err(CliError::new(
            EX_FAILURE,
            "confirmation_required",
            "Failed to read the confirmation prompt.",
        )
        .into());
    }
    if input.trim().eq_ignore_ascii_case("y") {
        Ok(())
    } else {
        Err(CliError::new(
            EX_FAILURE,
            "confirmation_declined",
            "Cancelled; nothing changed.",
        )
        .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_finds_a_cli_error_wrapped_in_context() {
        let error = anyhow::Error::new(CliError::mount_source_required())
            .context("failed to parse command line");
        let resolved = resolve(&error);

        assert_eq!(resolved.payload.code, "mount_source_required");
        assert_eq!(resolved.exit_code, EX_USAGE);
    }

    #[test]
    fn resolve_falls_back_to_internal_error_for_untyped_failures() {
        let error = anyhow::anyhow!("something broke");
        let resolved = resolve(&error);

        assert_eq!(resolved.payload.code, "internal_error");
        assert_eq!(resolved.exit_code, EX_FAILURE);
    }

    #[test]
    fn json_envelope_has_a_fixed_four_key_shape() {
        let error = anyhow::Error::new(CliError::daemon_not_running().with_scope("status"));
        let cli_error = resolve(&error);
        let envelope = ErrorEnvelope {
            error: &cli_error.payload,
        };
        let value = serde_json::to_value(&envelope).unwrap();
        let object = value["error"].as_object().unwrap();

        assert!(object.contains_key("code"));
        assert!(object.contains_key("message"));
        assert!(object.contains_key("hint"));
        assert!(object.contains_key("scope"));
        assert_eq!(object["code"], "daemon_not_running");
    }

    #[test]
    fn exit_codes_match_the_documented_map() {
        assert_eq!(CliError::mount_source_required().exit_code, 2);
        assert_eq!(CliError::daemon_not_running().exit_code, 75);
        assert_eq!(
            CliError::refused("access_key_cannot_revoke", "no").exit_code,
            77
        );
        assert_eq!(CliError::not_configured().exit_code, 1);
    }

    #[test]
    fn access_key_refusals_all_exit_77_with_stable_codes() {
        assert_eq!(CliError::access_key_cannot_create_folder().exit_code, 77);
        assert_eq!(
            CliError::access_key_cannot_create_folder().payload.code,
            "access_key_cannot_create_folder"
        );
        assert_eq!(CliError::access_key_cannot_mount_folder().exit_code, 77);
        assert_eq!(CliError::access_key_cannot_issue_keys().exit_code, 77);
        assert_eq!(CliError::access_key_cannot_invite_people().exit_code, 77);
        assert_eq!(CliError::access_key_cannot_revoke().exit_code, 77);
        assert_eq!(CliError::access_key_is_read_only("Design").exit_code, 77);
    }

    #[test]
    fn sync_timed_out_is_retryable_and_sync_mount_error_is_a_plain_failure() {
        assert_eq!(CliError::sync_timed_out("still waiting").exit_code, EX_TEMPFAIL);
        let error = CliError::sync_mount_error("Design", "quota exceeded");
        assert_eq!(error.exit_code, EX_FAILURE);
        assert!(error.payload.message.contains("Design"));
        assert!(error.payload.message.contains("quota exceeded"));
    }

    #[test]
    fn mount_source_required_renders_one_alternative_per_line_hanging_under_the_hint_label() {
        let rendered = CliError::mount_source_required().to_string();

        assert_eq!(
            rendered,
            "error: mount needs exactly one source.\n\
             hint:  --folder <id|name>  attach a folder you can reach\n       \
             --key <token>       redeem an access key\n       \
             --new               create one from this path"
        );
        assert!(!rendered.contains("--new Pass"));
    }

    #[test]
    fn a_hintless_error_prints_only_the_error_line() {
        let rendered = CliError::not_configured().to_string();

        assert!(rendered.starts_with("error: "));
        assert!(!rendered.contains("hint:"));
        assert_eq!(rendered.lines().count(), 1);
    }

    #[test]
    fn a_single_sentence_hint_prints_as_one_hint_line() {
        let rendered = CliError::access_key_cannot_create_folder().to_string();

        assert_eq!(rendered.lines().count(), 2);
        assert!(rendered.lines().nth(1).unwrap().starts_with("hint:  "));
    }
}

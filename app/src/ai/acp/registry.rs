use std::path::PathBuf;

use warp_cli::agent::Harness;

/// ACPX profile used to run a local harness.
///
/// ACPX owns the ACP transport, session lifecycle, and permission callbacks.
/// Built-in profiles select ACPX's maintained adapter registry; raw profiles
/// retain support for agents ACPX does not expose as a first-class subcommand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AcpxAgentProfile {
    BuiltIn(&'static str),
    RawAcpServer(&'static str),
}

impl AcpxAgentProfile {
    pub(crate) fn command_args(self) -> Vec<String> {
        match self {
            Self::BuiltIn(agent) => vec![agent.to_string()],
            Self::RawAcpServer(command) => vec!["--agent".to_string(), command.to_string()],
        }
    }
}

/// Antigravity is not an ACPX built-in profile. Callers that expose it as a
/// separate product choice must use this raw ACP-server profile rather than
/// bypassing ACPX and spawning `agy-acp` directly.
pub(crate) const ANTIGRAVITY_ACPX_PROFILE: AcpxAgentProfile =
    AcpxAgentProfile::RawAcpServer("agy-acp");

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalAcpAgentSpec {
    pub(crate) harness: Harness,
    /// The ACPX executable. Kept as `command` while callers migrate from
    /// direct adapter spawning to [`AcpxAgentProfile`].
    pub(crate) command: &'static str,
    /// ACPX agent-selector arguments. Prefer [`acpx_profile_for_harness`] in
    /// new code so raw ACP servers cannot be mistaken for built-in profiles.
    pub(crate) args: &'static [&'static str],
    pub(crate) acpx_profile: AcpxAgentProfile,
    pub(crate) install_url: &'static str,
    pub(crate) default_models: &'static [&'static str],
    pub(crate) supports_resume: bool,
}

pub(crate) fn agent_specs() -> &'static [LocalAcpAgentSpec] {
    &[
        LocalAcpAgentSpec {
            harness: Harness::Claude,
            command: "acpx",
            args: &["claude"],
            acpx_profile: AcpxAgentProfile::BuiltIn("claude"),
            install_url: "https://docs.anthropic.com/en/docs/claude-code",
            default_models: &[],
            supports_resume: true,
        },
        LocalAcpAgentSpec {
            harness: Harness::Codex,
            command: "acpx",
            args: &["codex"],
            acpx_profile: AcpxAgentProfile::BuiltIn("codex"),
            install_url: "https://github.com/openai/codex",
            default_models: &[],
            supports_resume: true,
        },
        LocalAcpAgentSpec {
            harness: Harness::Gemini,
            command: "acpx",
            args: &["gemini"],
            acpx_profile: AcpxAgentProfile::BuiltIn("gemini"),
            install_url: "https://github.com/google-gemini/gemini-cli",
            default_models: &["gemini-2.5-pro"],
            supports_resume: true,
        },
        LocalAcpAgentSpec {
            harness: Harness::Cursor,
            command: "acpx",
            args: &["cursor"],
            acpx_profile: AcpxAgentProfile::BuiltIn("cursor"),
            install_url: "https://cursor.com/docs/cli",
            default_models: &[],
            supports_resume: true,
        },
        LocalAcpAgentSpec {
            harness: Harness::Devin,
            command: "acpx",
            args: &["--agent", "devin acp"],
            acpx_profile: AcpxAgentProfile::RawAcpServer("devin acp"),
            install_url: "https://docs.devin.ai/cli/reference/commands#devin-acp",
            default_models: &[],
            supports_resume: true,
        },
    ]
}

pub(crate) fn spec_for_harness(harness: Harness) -> Option<&'static LocalAcpAgentSpec> {
    agent_specs().iter().find(|spec| spec.harness == harness)
}

pub(crate) fn is_local_acp_harness(harness: Harness) -> bool {
    spec_for_harness(harness).is_some()
}

pub(crate) fn acpx_profile_for_harness(harness: Harness) -> Option<AcpxAgentProfile> {
    spec_for_harness(harness).map(|spec| spec.acpx_profile)
}

pub(crate) fn command_for_harness(harness: Harness) -> Option<(PathBuf, Vec<String>)> {
    let spec = spec_for_harness(harness)?;
    Some((
        PathBuf::from(spec.command),
        spec.args.iter().map(|arg| (*arg).to_string()).collect(),
    ))
}

pub(crate) fn should_auto_authenticate(harness: Harness) -> bool {
    // ACPX performs ACP authentication before it emits Warp-readable NDJSON.
    // Keep this function through the direct-client removal so old callers do
    // not accidentally start an interactive provider flow.
    let _ = harness;
    false
}

pub(crate) fn default_session_mode(_harness: Harness) -> Option<&'static str> {
    // ACPX uses --approve-reads (reads auto-approved, writes denied
    // non-interactively) for user prompts. A future permission UI should
    // replace this with interactive allow-once/allow-always/reject prompts.
    None
}

pub(crate) fn process_env_for_harness(
    _harness: Harness,
) -> &'static [(&'static str, &'static str)] {
    &[]
}

pub(crate) fn removed_process_env_for_harness(harness: Harness) -> &'static [&'static str] {
    match harness {
        Harness::Codex => &["ANTHROPIC_API_KEY"],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acpx_owns_authentication_for_every_local_harness() {
        assert!(!should_auto_authenticate(Harness::Codex));
        assert!(!should_auto_authenticate(Harness::Gemini));
        assert!(!should_auto_authenticate(Harness::Claude));
    }

    #[test]
    fn codex_removes_anthropic_key_from_child_env() {
        assert_eq!(
            removed_process_env_for_harness(Harness::Codex),
            &["ANTHROPIC_API_KEY"]
        );
    }

    #[test]
    fn profiles_use_acpx_builtins_or_raw_agent_escape_hatch() {
        assert_eq!(
            acpx_profile_for_harness(Harness::Claude),
            Some(AcpxAgentProfile::BuiltIn("claude"))
        );
        assert_eq!(
            acpx_profile_for_harness(Harness::Codex),
            Some(AcpxAgentProfile::BuiltIn("codex"))
        );
        assert_eq!(
            acpx_profile_for_harness(Harness::Gemini),
            Some(AcpxAgentProfile::BuiltIn("gemini"))
        );
        assert_eq!(
            acpx_profile_for_harness(Harness::Cursor),
            Some(AcpxAgentProfile::BuiltIn("cursor"))
        );
        assert_eq!(
            acpx_profile_for_harness(Harness::Devin),
            Some(AcpxAgentProfile::RawAcpServer("devin acp"))
        );
    }

    #[test]
    fn raw_acp_server_profile_keeps_the_command_as_one_argument() {
        assert_eq!(
            AcpxAgentProfile::RawAcpServer("devin acp").command_args(),
            ["--agent", "devin acp"]
        );
        assert_eq!(
            ANTIGRAVITY_ACPX_PROFILE.command_args(),
            ["--agent", "agy-acp"]
        );
    }
}

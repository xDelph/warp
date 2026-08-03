use std::collections::HashMap;

use settings::macros::define_settings_group;
use settings::{RespectUserSyncSetting, SupportedPlatforms, SyncToCloud};

define_settings_group!(SshSettings,
    settings: [
        reuse_existing_control_master: ReuseExistingSshControlMaster {
            type: bool,
            default: false,
            supported_platforms: SupportedPlatforms::ALL,
            sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
            surface: settings::SettingSurfaces::GUI,
            private: false,
            storage_key: "ReuseExistingSshControlMaster",
            toml_path: "warpify.ssh.reuse_existing_control_master",
            description: "Whether the legacy SSH wrapper attaches to an existing SSH ControlMaster for the destination host instead of always creating its own.",
        },
        inherit_ssh_on_split: InheritSshOnSplit {
            type: bool,
            default: true,
            supported_platforms: SupportedPlatforms::ALL,
            sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
            private: false,
            storage_key: "InheritSshOnSplit",
            toml_path: "warpify.ssh.inherit_ssh_on_split",
            description: "Whether splitting a pane whose active session is remote reconnects the new pane to the same SSH host and working directory.",
        },
        ssh_host_colors: SshHostColors {
            type: HashMap<String, String>,
            default: HashMap::default(),
            supported_platforms: SupportedPlatforms::ALL,
            sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
            private: false,
            storage_key: "SshHostColors",
            toml_path: "warpify.ssh.host_colors",
            description: "Per-host pane background colors, mapping SSH hostname to a hex color (e.g. \"#2a3b4c\"). Hosts without an entry get a deterministic tint derived from the hostname.",
        },
    ]
);

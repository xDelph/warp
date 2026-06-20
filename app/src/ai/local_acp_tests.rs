use settings::Setting;
use warpui::{App, SingletonEntity};

use crate::settings::AISettings;
use crate::test_util::terminal::initialize_app_for_terminal_view;

#[test]
fn local_acp_disabled_when_ai_globally_disabled() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        AISettings::handle(&app).update(&mut app, |settings, ctx| {
            let _ = settings.is_any_ai_enabled.set_value(false, ctx);
            let _ = settings.local_acp_enabled.set_value(true, ctx);
        });

        app.read(|ctx| {
            assert!(!crate::ai::local_acp::local_acp_enabled(ctx));
            assert!(!crate::ai::local_acp::cloud_agent_disabled(ctx));
        });
    });
}

#[test]
fn local_acp_follows_user_setting_when_ai_enabled() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        AISettings::handle(&app).update(&mut app, |settings, ctx| {
            let _ = settings.is_any_ai_enabled.set_value(true, ctx);
            let _ = settings.local_acp_enabled.set_value(true, ctx);
        });

        app.read(|ctx| {
            assert!(crate::ai::local_acp::local_acp_enabled(ctx));
            assert!(crate::ai::local_acp::cloud_agent_disabled(ctx));
        });

        AISettings::handle(&app).update(&mut app, |settings, ctx| {
            let _ = settings.local_acp_enabled.set_value(false, ctx);
        });

        app.read(|ctx| {
            assert!(!crate::ai::local_acp::local_acp_enabled(ctx));
            assert!(!crate::ai::local_acp::cloud_agent_disabled(ctx));
        });
    });
}

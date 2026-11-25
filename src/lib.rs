//! MyBatis SQL Visualizer for OpenIDEA.
//!
//! The port keeps the same split as the IntelliJ edition: this crate owns the
//! platform-facing surfaces (commands first), while rendering itself lives in
//! the engine jar exported by the IntelliJ repository (`engine/`). Until that
//! jar is connected, `render-xml` prints a fixed statement so the
//! command → Output chain can be verified end to end.

mod commands;

use openidea_plugin::{Error, Invocation, Plugin, register_plugin};

/// Plugin state. Every command is stateless for now, so nothing is stored yet;
/// state that must survive idle collection will go through `host::storage`.
#[derive(Default)]
pub struct Main;

impl Plugin for Main {
    fn run_command(
        &mut self,
        id: &str,
        ctx: &Invocation,
        _arg: Option<&str>,
    ) -> Result<Option<String>, Error> {
        match id {
            commands::render_xml::ID => commands::render_xml::run(ctx),
            _ => Err(Error::NotFound(format!("command {id}"))),
        }
    }
}

register_plugin!(Main);

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    #[test]
    fn routes_render_xml_to_its_command() {
        let host = MockHost::install();
        let mut plugin = Main;
        let out = plugin
            .run_command(commands::render_xml::ID, &host.invocation(), None)
            .unwrap();
        assert_eq!(out, None);
        let buffers = host.output();
        let text = buffers
            .get(commands::render_xml::CHANNEL)
            .expect("the command writes to its own channel");
        assert!(text.contains("SELECT id, name, created_at"));
    }

    #[test]
    fn unknown_command_is_an_error() {
        let host = MockHost::install();
        let mut plugin = Main;
        let err = plugin
            .run_command("no-such-command", &host.invocation(), None)
            .unwrap_err();
        assert_eq!(err, Error::NotFound("command no-such-command".into()));
    }
}

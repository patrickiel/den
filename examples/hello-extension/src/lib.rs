//! The smallest den extension: a toast when den starts, a log line per
//! folder den opens, one setting (the greeting, set on its page in den), and
//! one command (Say Hello, Ctrl+Alt+H, in den's menu) that names the file
//! you're in. See README.md for building and publishing one.

use den_extension::{Context, Extension, Host, events, register};
use serde_json::Value;

struct Hello {
    host: Host,
    greeting: String,
    /// The file in the active tab, as `active_file_changed` last said.
    active_file: Option<String>,
}

impl Extension for Hello {
    fn activate(host: Host, context: Context) -> Self {
        // The settings `extension.json` declares, with the user's values.
        let greeting = context.settings["greeting"].as_str().unwrap_or("Hello from an extension").to_string();
        host.toast(format!("{greeting} (den {})", context.den_version));
        Hello { host, greeting, active_file: None }
    }

    fn event(&mut self, name: &str, data: Value) {
        match name {
            events::WORKSPACE_OPENED => self.host.log(format!("opened {}", data["root"].as_str().unwrap_or("?"))),
            // Every setting's value, after the user changed one.
            events::SETTINGS_CHANGED => {
                if let Some(greeting) = data["settings"]["greeting"].as_str() {
                    self.greeting = greeting.to_string();
                }
            }
            events::ACTIVE_FILE_CHANGED => self.active_file = data["path"].as_str().map(str::to_string),
            // `extension.json` declares the command and its keys; den runs it.
            events::COMMAND if data["id"] == "say-hello" => {
                let file = self.active_file.as_deref().and_then(|path| path.rsplit(['\\', '/']).next());
                let place = file.map_or_else(|| "no file open".to_string(), |file| format!("you're in {file}"));
                self.host.toast(format!("{}: {place}", self.greeting));
            }
            _ => {}
        }
    }

    fn deactivate(&mut self) {
        self.host.log("goodbye");
    }
}

register!(Hello);

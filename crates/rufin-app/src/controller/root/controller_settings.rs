use super::*;

impl AppController {
    pub fn load_settings(&self) -> AppSettings {
        self.settings.load_settings()
    }
    pub fn save_settings(&self, settings: &AppSettings) -> Result<(), String> {
        self.settings.save_settings(settings)
    }
    pub fn reload_snapshot(&self) {
        let store = self.store.clone();
        let events = self.events.clone();
        thread::spawn(move || emit_snapshot(&store, &events));
    }
}

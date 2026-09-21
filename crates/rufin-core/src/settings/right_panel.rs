use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SidebarPanel {
    Queue,
    Lyrics,
    Visualizer,
}

impl SidebarPanel {
    pub const ALL: [Self; 3] = [Self::Queue, Self::Lyrics, Self::Visualizer];

    pub fn index(self) -> usize {
        match self {
            Self::Queue => 0,
            Self::Lyrics => 1,
            Self::Visualizer => 2,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RightPanelSettings {
    pub order: Vec<SidebarPanel>,
    pub queue_visible: bool,
    pub combined: bool,
    pub lyrics_height: Option<i32>,
    pub visualizer_height: Option<i32>,
}

impl Default for RightPanelSettings {
    fn default() -> Self {
        Self {
            order: SidebarPanel::ALL.to_vec(),
            queue_visible: true,
            combined: true,
            lyrics_height: None,
            visualizer_height: None,
        }
    }
}

impl RightPanelSettings {
    pub fn sanitize(&mut self) {
        let mut order = Vec::with_capacity(3);
        for panel in self.order.iter().copied().chain(SidebarPanel::ALL) {
            if !order.contains(&panel) {
                order.push(panel);
            }
        }
        self.order = order;
    }

    pub fn visible_panels(&self, lyrics: bool, visualizer: bool) -> Vec<SidebarPanel> {
        self.order
            .iter()
            .copied()
            .filter(|panel| match panel {
                SidebarPanel::Queue => self.queue_visible,
                SidebarPanel::Lyrics => lyrics || self.combined && visualizer,
                SidebarPanel::Visualizer => visualizer && !self.combined,
            })
            .collect()
    }

    pub fn move_panel(
        &mut self,
        panel: SidebarPanel,
        direction: isize,
        lyrics: bool,
        visualizer: bool,
    ) -> bool {
        let visible = self.visible_panels(lyrics, visualizer);
        let Some(index) = visible.iter().position(|candidate| *candidate == panel) else {
            return false;
        };
        let Some(neighbor) = index
            .checked_add_signed(direction)
            .and_then(|index| visible.get(index))
        else {
            return false;
        };
        let from = self
            .order
            .iter()
            .position(|candidate| *candidate == panel)
            .unwrap();
        let to = self
            .order
            .iter()
            .position(|candidate| candidate == neighbor)
            .unwrap();
        self.order.swap(from, to);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_visibility_defaults_on_and_preserves_panel_order() {
        use SidebarPanel::*;
        let mut settings: RightPanelSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.visible_panels(false, false), [Queue]);
        settings.queue_visible = false;
        assert!(settings.visible_panels(false, false).is_empty());
        assert_eq!(settings.visible_panels(true, true), [Lyrics]);
        settings.combined = false;
        assert_eq!(settings.visible_panels(true, true), [Lyrics, Visualizer]);
        assert!(settings.move_panel(Visualizer, -1, true, true));
        let restored: RightPanelSettings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored, settings);
        settings.queue_visible = true;
        assert_eq!(
            settings.visible_panels(true, true),
            [Queue, Visualizer, Lyrics]
        );
    }

    #[test]
    fn moving_combined_panel_keeps_the_separate_visualizer_position() {
        use SidebarPanel::*;
        let mut settings = RightPanelSettings::default();
        assert!(settings.move_panel(Lyrics, -1, true, true));
        assert_eq!(settings.visible_panels(true, true), [Lyrics, Queue]);
        settings.combined = false;
        assert_eq!(
            settings.visible_panels(true, true),
            [Lyrics, Queue, Visualizer]
        );
        assert!(!settings.move_panel(Lyrics, -1, true, true));
    }

    #[test]
    fn moving_skips_hidden_panels_without_changing_their_sizes() {
        use SidebarPanel::*;
        let mut settings = RightPanelSettings {
            combined: false,
            lyrics_height: Some(210),
            visualizer_height: Some(170),
            ..Default::default()
        };
        assert!(settings.move_panel(Visualizer, -1, false, true));
        assert_eq!(settings.visible_panels(false, true), [Visualizer, Queue]);
        assert_eq!(
            settings.visible_panels(true, true),
            [Visualizer, Lyrics, Queue]
        );
        assert_eq!(settings.lyrics_height, Some(210));
        assert_eq!(settings.visualizer_height, Some(170));
        let restored: RightPanelSettings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored, settings);
    }
}

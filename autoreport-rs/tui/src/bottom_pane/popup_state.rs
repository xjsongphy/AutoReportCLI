//! Single-active-popup state, matching Codex's `popup_state.rs` boundary.

use crate::app_state::Mention;
use crate::slash_command::SlashCompletion;

pub(crate) enum ActivePopup {
    Slash(SlashCompletion),
    Mention(Mention),
}

#[derive(Default)]
pub(crate) struct PopupState {
    active: Option<ActivePopup>,
    dismissed_slash: Option<String>,
    dismissed_mention: Option<String>,
}

impl PopupState {
    pub(crate) fn slash(&self) -> Option<&SlashCompletion> {
        match self.active.as_ref() {
            Some(ActivePopup::Slash(popup)) => Some(popup),
            _ => None,
        }
    }

    pub(crate) fn slash_mut(&mut self) -> Option<&mut SlashCompletion> {
        match self.active.as_mut() {
            Some(ActivePopup::Slash(popup)) => Some(popup),
            _ => None,
        }
    }

    pub(crate) fn take_slash(&mut self) -> Option<SlashCompletion> {
        match self.active.take() {
            Some(ActivePopup::Slash(popup)) => Some(popup),
            Some(other) => {
                self.active = Some(other);
                None
            }
            None => None,
        }
    }

    pub(crate) fn set_slash(&mut self, popup: Option<SlashCompletion>) {
        match popup {
            Some(popup) => self.active = Some(ActivePopup::Slash(popup)),
            None if matches!(self.active, Some(ActivePopup::Slash(_))) => self.active = None,
            None => {}
        }
    }

    pub(crate) fn mention(&self) -> Option<&Mention> {
        match self.active.as_ref() {
            Some(ActivePopup::Mention(popup)) => Some(popup),
            _ => None,
        }
    }

    pub(crate) fn mention_mut(&mut self) -> Option<&mut Mention> {
        match self.active.as_mut() {
            Some(ActivePopup::Mention(popup)) => Some(popup),
            _ => None,
        }
    }

    pub(crate) fn take_mention(&mut self) -> Option<Mention> {
        match self.active.take() {
            Some(ActivePopup::Mention(popup)) => Some(popup),
            Some(other) => {
                self.active = Some(other);
                None
            }
            None => None,
        }
    }

    pub(crate) fn set_mention(&mut self, popup: Option<Mention>) {
        match popup {
            Some(popup) => self.active = Some(ActivePopup::Mention(popup)),
            None if matches!(self.active, Some(ActivePopup::Mention(_))) => self.active = None,
            None => {}
        }
    }

    pub(crate) fn dismissed_slash(&self) -> Option<&str> {
        self.dismissed_slash.as_deref()
    }

    pub(crate) fn set_dismissed_slash(&mut self, value: Option<String>) {
        self.dismissed_slash = value;
    }

    pub(crate) fn dismissed_mention(&self) -> Option<&str> {
        self.dismissed_mention.as_deref()
    }

    pub(crate) fn set_dismissed_mention(&mut self, value: Option<String>) {
        self.dismissed_mention = value;
    }

    pub(crate) fn clear(&mut self) {
        self.active = None;
        self.dismissed_slash = None;
        self.dismissed_mention = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::Mention;
    use crate::slash_command::SlashCompletion;

    #[test]
    fn popup_state_has_one_active_popup() {
        let mut state = PopupState::default();
        state.set_slash(Some(SlashCompletion {
            matches: Vec::new(),
            selected: 0,
        }));
        assert!(state.slash().is_some());
        state.set_mention(Some(Mention {
            start: 0,
            cursor: 1,
            matches: Vec::new(),
            selected: 0,
        }));
        assert!(state.slash().is_none());
        assert!(state.mention().is_some());
    }
}

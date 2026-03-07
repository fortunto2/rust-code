//! Focus layer system for TUI input routing.
//!
//! UI components that can intercept input (popups, overlays, pickers)
//! implement `FocusLayer`. A `FocusStack` routes events top-down:
//! the highest-priority active layer gets the event first.
//!
//! ```text
//! FocusStack: [HelpOverlay, FuzzyPicker, SlashPopup, ...]
//!              ↓ is_active?  ↓ is_active?  ↓ is_active?
//!              skip          on_key()       (never reached if picker consumed)
//! ```

use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::layout::Rect;

/// Result of a focus layer handling an input event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusResult {
    /// Event was consumed — stop propagation.
    Consumed,
    /// Event was not handled — pass to next layer.
    Passed,
}

impl FocusResult {
    pub fn consumed(&self) -> bool {
        *self == Self::Consumed
    }
}

/// A UI component that can intercept keyboard and mouse input.
///
/// Layers are checked in priority order (highest first).
/// Only active layers receive events.
pub trait FocusLayer {
    /// Whether this layer should intercept events right now.
    fn is_active(&self) -> bool;

    /// Handle a keyboard event. Return `Consumed` to stop propagation.
    fn on_key(&mut self, key: KeyEvent) -> FocusResult {
        let _ = key;
        FocusResult::Passed
    }

    /// Handle a mouse event. Return `Consumed` to stop propagation.
    ///
    /// Default: if mouse is inside `hit_rect()`, consume scroll and click.
    fn on_mouse(&mut self, mouse: MouseEvent) -> FocusResult {
        let _ = mouse;
        FocusResult::Passed
    }

    /// Bounding rectangle for mouse hit testing (if applicable).
    fn hit_rect(&self) -> Option<Rect> {
        None
    }
}

/// Check if a point (col, row) is inside a Rect.
pub fn point_in_rect(col: u16, row: u16, rect: Rect) -> bool {
    col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height
}

/// Route an event through a stack of focus layers (highest priority first).
///
/// Returns `Consumed` if any layer handled the event.
///
/// Usage:
/// ```ignore
/// let result = route_key(&mut [&mut help, &mut picker, &mut slash], key_event);
/// if result.consumed() { return; }
/// // ... normal input handling
/// ```
pub fn route_key(layers: &mut [&mut dyn FocusLayer], key: KeyEvent) -> FocusResult {
    for layer in layers.iter_mut() {
        if layer.is_active() {
            let result = layer.on_key(key);
            if result.consumed() {
                return FocusResult::Consumed;
            }
        }
    }
    FocusResult::Passed
}

/// Route a mouse event through a stack of focus layers.
pub fn route_mouse(layers: &mut [&mut dyn FocusLayer], mouse: MouseEvent) -> FocusResult {
    for layer in layers.iter_mut() {
        if layer.is_active() {
            let result = layer.on_mouse(mouse);
            if result.consumed() {
                return FocusResult::Consumed;
            }
        }
    }
    FocusResult::Passed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    struct MockPopup {
        active: bool,
        last_key: Option<KeyCode>,
    }

    impl FocusLayer for MockPopup {
        fn is_active(&self) -> bool {
            self.active
        }

        fn on_key(&mut self, key: KeyEvent) -> FocusResult {
            self.last_key = Some(key.code);
            match key.code {
                KeyCode::Esc => {
                    self.active = false;
                    FocusResult::Consumed
                }
                KeyCode::Up | KeyCode::Down | KeyCode::Tab | KeyCode::Enter => {
                    FocusResult::Consumed
                }
                _ => FocusResult::Passed, // Let typing fall through
            }
        }
    }

    fn make_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn inactive_layer_is_skipped() {
        let mut popup = MockPopup {
            active: false,
            last_key: None,
        };
        let result = route_key(&mut [&mut popup], make_key(KeyCode::Tab));
        assert_eq!(result, FocusResult::Passed);
        assert!(popup.last_key.is_none());
    }

    #[test]
    fn active_layer_consumes() {
        let mut popup = MockPopup {
            active: true,
            last_key: None,
        };
        let result = route_key(&mut [&mut popup], make_key(KeyCode::Tab));
        assert_eq!(result, FocusResult::Consumed);
        assert_eq!(popup.last_key, Some(KeyCode::Tab));
    }

    #[test]
    fn typing_falls_through() {
        let mut popup = MockPopup {
            active: true,
            last_key: None,
        };
        let result = route_key(&mut [&mut popup], make_key(KeyCode::Char('a')));
        assert_eq!(result, FocusResult::Passed);
    }

    #[test]
    fn esc_closes_and_consumes() {
        let mut popup = MockPopup {
            active: true,
            last_key: None,
        };
        let result = route_key(&mut [&mut popup], make_key(KeyCode::Esc));
        assert_eq!(result, FocusResult::Consumed);
        assert!(!popup.active);
    }

    #[test]
    fn priority_order_first_active_wins() {
        let mut high = MockPopup {
            active: true,
            last_key: None,
        };
        let mut low = MockPopup {
            active: true,
            last_key: None,
        };
        let result = route_key(&mut [&mut high, &mut low], make_key(KeyCode::Enter));
        assert_eq!(result, FocusResult::Consumed);
        assert_eq!(high.last_key, Some(KeyCode::Enter));
        assert!(low.last_key.is_none()); // Never reached
    }

    #[test]
    fn point_in_rect_works() {
        let rect = Rect::new(10, 20, 30, 10);
        assert!(point_in_rect(10, 20, rect));
        assert!(point_in_rect(39, 29, rect));
        assert!(!point_in_rect(9, 20, rect));
        assert!(!point_in_rect(40, 20, rect));
        assert!(!point_in_rect(10, 30, rect));
    }
}

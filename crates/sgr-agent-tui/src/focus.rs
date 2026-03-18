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

// ============================================================================
// FocusRing — panel focus cycling with click-to-focus
// ============================================================================

use ratatui::style::{Color, Style};

/// A ring of focusable panels. Tracks which panel is active,
/// supports Tab/Shift-Tab cycling, and click-to-focus via Rects.
///
/// ```ignore
/// let mut ring = FocusRing::new(&["input", "chat", "plan", "channels"]);
/// ring.next(); // input → chat
/// ring.prev(); // chat → input
/// ring.focus("plan"); // jump to plan
/// ring.click(col, row); // focus panel under cursor
///
/// // In rendering:
/// let style = ring.border_style("chat"); // Yellow if focused, DarkGray otherwise
/// ```
pub struct FocusRing {
    panels: Vec<&'static str>,
    rects: Vec<Option<Rect>>,
    current: usize,
}

impl FocusRing {
    /// Create a new focus ring. First panel is focused by default.
    pub fn new(panels: &[&'static str]) -> Self {
        let len = panels.len();
        Self {
            panels: panels.to_vec(),
            rects: vec![None; len],
            current: 0,
        }
    }

    /// Current focused panel ID.
    pub fn focused(&self) -> &'static str {
        self.panels[self.current]
    }

    /// Is this panel currently focused?
    pub fn is_focused(&self, id: &str) -> bool {
        self.panels[self.current] == id
    }

    /// Focus a specific panel by ID. Returns true if found.
    pub fn focus(&mut self, id: &str) -> bool {
        if let Some(idx) = self.panels.iter().position(|p| *p == id) {
            self.current = idx;
            true
        } else {
            false
        }
    }

    /// Move focus to next panel (wraps).
    pub fn next(&mut self) {
        self.current = (self.current + 1) % self.panels.len();
    }

    /// Move focus to previous panel (wraps).
    pub fn prev(&mut self) {
        self.current = if self.current == 0 {
            self.panels.len() - 1
        } else {
            self.current - 1
        };
    }

    /// Update the bounding rect for a panel. Call during rendering.
    pub fn set_rect(&mut self, id: &str, rect: Rect) {
        if let Some(idx) = self.panels.iter().position(|p| *p == id) {
            self.rects[idx] = Some(rect);
        }
    }

    /// Try to focus the panel at (col, row). Returns true if a panel was hit.
    pub fn click(&mut self, col: u16, row: u16) -> bool {
        for (idx, rect) in self.rects.iter().enumerate() {
            if let Some(r) = rect
                && point_in_rect(col, row, *r)
            {
                self.current = idx;
                return true;
            }
        }
        false
    }

    /// Border style for a panel: Yellow if focused, DarkGray otherwise.
    pub fn border_style(&self, id: &str) -> Style {
        if self.is_focused(id) {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::DarkGray)
        }
    }
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

    // FocusRing tests

    #[test]
    fn ring_default_is_first() {
        let ring = FocusRing::new(&["input", "chat", "plan"]);
        assert_eq!(ring.focused(), "input");
        assert!(ring.is_focused("input"));
    }

    #[test]
    fn ring_next_wraps() {
        let mut ring = FocusRing::new(&["a", "b", "c"]);
        ring.next();
        assert_eq!(ring.focused(), "b");
        ring.next();
        assert_eq!(ring.focused(), "c");
        ring.next();
        assert_eq!(ring.focused(), "a"); // wrap
    }

    #[test]
    fn ring_prev_wraps() {
        let mut ring = FocusRing::new(&["a", "b", "c"]);
        ring.prev();
        assert_eq!(ring.focused(), "c"); // wrap
        ring.prev();
        assert_eq!(ring.focused(), "b");
    }

    #[test]
    fn ring_focus_by_id() {
        let mut ring = FocusRing::new(&["input", "chat", "plan"]);
        assert!(ring.focus("plan"));
        assert_eq!(ring.focused(), "plan");
        assert!(!ring.focus("nonexistent"));
        assert_eq!(ring.focused(), "plan"); // unchanged
    }

    #[test]
    fn ring_click_to_focus() {
        let mut ring = FocusRing::new(&["input", "chat"]);
        ring.set_rect("input", Rect::new(0, 0, 50, 5));
        ring.set_rect("chat", Rect::new(0, 5, 50, 20));

        assert!(ring.click(10, 10)); // inside chat
        assert_eq!(ring.focused(), "chat");

        assert!(ring.click(10, 2)); // inside input
        assert_eq!(ring.focused(), "input");

        assert!(!ring.click(60, 60)); // outside all
        assert_eq!(ring.focused(), "input"); // unchanged
    }

    #[test]
    fn ring_border_style() {
        let ring = FocusRing::new(&["a", "b"]);
        let focused = ring.border_style("a");
        let unfocused = ring.border_style("b");
        assert_eq!(focused.fg, Some(Color::Yellow));
        assert_eq!(unfocused.fg, Some(Color::DarkGray));
    }
}

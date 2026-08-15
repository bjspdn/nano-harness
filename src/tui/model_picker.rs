use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::provider::ModelMetadata;

const MAX_MODAL_WIDTH: u16 = 96;
const MAX_MODAL_HEIGHT: u16 = 20;
const SEARCH_LABEL: &str = "Search: ";
const FOOTER: &str = "Enter select  Esc close  Up/Down navigate";

/// The locally projected state of runtime model discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogState {
    Loading,
    Loaded(Vec<ModelMetadata>),
    Failed(String),
}

/// The action produced by the model picker key handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelPickerAction {
    Continue,
    SelectModel(String),
}

/// State and behavior for the independently composed model picker modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelPickerState {
    open: bool,
    catalog_state: CatalogState,
    query: String,
    active_row: usize,
    pending_model_id: Option<String>,
    error: Option<String>,
}

impl Default for ModelPickerState {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelPickerState {
    pub fn new() -> Self {
        Self {
            open: false,
            catalog_state: CatalogState::Loading,
            query: String::new(),
            active_row: 0,
            pending_model_id: None,
            error: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn catalog_state(&self) -> &CatalogState {
        &self.catalog_state
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn active_row(&self) -> usize {
        self.active_row
    }

    pub fn pending_model_id(&self) -> Option<&str> {
        self.pending_model_id.as_deref()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Open the modal and start a fresh local search without changing catalog data.
    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.active_row = 0;
        self.error = None;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.pending_model_id = None;
        self.error = None;
    }

    pub fn catalog_loading(&mut self) {
        self.catalog_state = CatalogState::Loading;
        self.active_row = 0;
        self.error = None;
    }

    pub fn catalog_loaded(&mut self, models: Vec<ModelMetadata>) {
        self.catalog_state = CatalogState::Loaded(models);
        self.clamp_active_row("");
        self.error = None;
    }

    pub fn catalog_failed(&mut self, error: String) {
        self.catalog_state = CatalogState::Failed(error);
        self.active_row = 0;
        self.error = None;
    }

    /// Record a successful command enqueue. Selection is not applied until runtime acknowledgment.
    pub fn accept_selection_enqueue(&mut self, model_id: String) {
        self.pending_model_id = Some(model_id);
        self.error = None;
    }

    /// Record a failed selection enqueue without changing the active model.
    pub fn reject_selection_enqueue(&mut self, error: String) {
        self.pending_model_id = None;
        self.error = Some(error);
    }

    /// Record a failed discovery enqueue while preserving the already-open modal.
    pub fn reject_discovery_enqueue(&mut self, error: String) {
        self.error = Some(error);
    }

    /// Apply a runtime selection failure to the current picker request.
    pub fn selection_failed(&mut self, error: String) {
        let had_pending_selection = self.pending_model_id.take().is_some();
        if had_pending_selection {
            self.open = true;
        }
        self.error = Some(error);
    }

    /// Apply a runtime selection acknowledgment and report whether it closed a pending request.
    pub fn selection_acknowledged(&mut self, model_id: &str) -> bool {
        if self.pending_model_id.as_deref() != Some(model_id) {
            return false;
        }

        self.pending_model_id = None;
        self.error = None;
        self.open = false;
        true
    }

    /// Handle a key while the modal owns input. The current model ID controls selected-first order.
    pub fn handle_key(&mut self, key_event: KeyEvent, current_model_id: &str) -> ModelPickerAction {
        if !self.open {
            return ModelPickerAction::Continue;
        }

        if key_event.kind == crossterm::event::KeyEventKind::Release {
            return ModelPickerAction::Continue;
        }

        match key_event.code {
            KeyCode::Esc => {
                self.close();
            }
            KeyCode::Up if key_event.modifiers == KeyModifiers::NONE => {
                self.active_row = self.active_row.saturating_sub(1);
            }
            KeyCode::Down if key_event.modifiers == KeyModifiers::NONE => {
                let filtered_model_count = self.filtered_models(current_model_id).len();
                self.active_row = self
                    .active_row
                    .saturating_add(1)
                    .min(filtered_model_count.saturating_sub(1));
            }
            KeyCode::Backspace if key_event.modifiers == KeyModifiers::NONE => {
                self.query.pop();
                self.clamp_active_row(current_model_id);
                self.error = None;
            }
            KeyCode::Char(character)
                if !key_event
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && !character.is_control() =>
            {
                self.query.push(character);
                self.clamp_active_row(current_model_id);
                self.error = None;
            }
            KeyCode::Enter if key_event.modifiers == KeyModifiers::NONE => {
                if self.pending_model_id.is_some() {
                    return ModelPickerAction::Continue;
                }

                let filtered_models = self.filtered_models(current_model_id);
                let Some(model) = filtered_models.get(self.active_row) else {
                    return ModelPickerAction::Continue;
                };
                return ModelPickerAction::SelectModel(model.model_id.clone());
            }
            _ => {}
        }

        ModelPickerAction::Continue
    }

    pub fn filtered_models(&self, current_model_id: &str) -> Vec<&ModelMetadata> {
        let CatalogState::Loaded(models) = &self.catalog_state else {
            return Vec::new();
        };

        let query = self.query.to_lowercase();
        let mut model_indices = models
            .iter()
            .enumerate()
            .filter(|(_, model)| {
                if query.is_empty() {
                    return true;
                }

                model.display_name.to_lowercase().contains(&query)
                    || model.model_id.to_lowercase().contains(&query)
            })
            .collect::<Vec<_>>();

        model_indices.sort_by(|(_, left), (_, right)| {
            let left_is_current = left.model_id == current_model_id;
            let right_is_current = right.model_id == current_model_id;
            right_is_current
                .cmp(&left_is_current)
                .then_with(|| {
                    left.display_name
                        .to_lowercase()
                        .cmp(&right.display_name.to_lowercase())
                })
                .then_with(|| {
                    left.model_id
                        .to_lowercase()
                        .cmp(&right.model_id.to_lowercase())
                })
                .then_with(|| left.display_name.cmp(&right.display_name))
                .then_with(|| left.model_id.cmp(&right.model_id))
        });

        model_indices.into_iter().map(|(_, model)| model).collect()
    }

    fn clamp_active_row(&mut self, current_model_id: &str) {
        let filtered_model_count = self.filtered_models(current_model_id).len();
        self.active_row = self.active_row.min(filtered_model_count.saturating_sub(1));
    }
}

/// Render the model picker over the base TUI. The overlay is a no-op for an unopened or empty area.
pub fn render(frame: &mut Frame, area: Rect, picker: &ModelPickerState, current_model_id: &str) {
    if !picker.is_open() || area.is_empty() {
        return;
    }

    let filtered_models = picker.filtered_models(current_model_id);
    let status_line_count = usize::from(!picker.error().is_none());
    let body_line_count = match picker.catalog_state() {
        CatalogState::Loading | CatalogState::Failed(_) => 1,
        CatalogState::Loaded(_) if filtered_models.is_empty() => 1,
        CatalogState::Loaded(_) => filtered_models.len(),
    };
    let desired_height = 4usize
        .saturating_add(status_line_count)
        .saturating_add(body_line_count);
    let modal_area = centered_modal_area(area, desired_height);
    if modal_area.is_empty() {
        return;
    }

    frame.render_widget(Clear, modal_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Models ")
        .border_style(Style::default().fg(Color::Cyan));
    let inner_area = block.inner(modal_area);
    frame.render_widget(block, modal_area);
    if inner_area.is_empty() {
        return;
    }

    let search_text = format!("{}{}", SEARCH_LABEL, single_line(picker.query()));
    let search_area = Rect::new(inner_area.x, inner_area.y, inner_area.width, 1);
    frame.render_widget(Paragraph::new(search_text.clone()), search_area);
    let cursor_offset = Line::from(search_text)
        .width()
        .min(usize::from(inner_area.width).saturating_sub(1));
    frame.set_cursor_position((
        inner_area
            .x
            .saturating_add(u16::try_from(cursor_offset).unwrap_or(u16::MAX)),
        inner_area.y,
    ));

    let mut lines = Vec::new();

    if let Some(error) = picker.error() {
        lines.push(Line::from(Span::styled(
            format!("Error: {}", single_line(error)),
            Style::default().fg(Color::Red),
        )));
    }

    match picker.catalog_state() {
        CatalogState::Loading => lines.push(Line::from("Loading models...")),
        CatalogState::Failed(error) => {
            lines.push(Line::from(format!("Catalog error: {}", single_line(error))))
        }
        CatalogState::Loaded(_) if filtered_models.is_empty() => {
            lines.push(Line::from("No matching models."));
        }
        CatalogState::Loaded(_) => {
            for (row_index, model) in filtered_models.iter().enumerate() {
                lines.push(Line::from(format_model_row(
                    model,
                    row_index == picker.active_row(),
                    model.model_id == current_model_id,
                )));
            }
        }
    }

    let footer_is_visible = inner_area.height >= 3;
    let content_height = usize::from(inner_area.height)
        .saturating_sub(1)
        .saturating_sub(usize::from(footer_is_visible));
    let content_area = Rect::new(
        inner_area.x,
        inner_area.y.saturating_add(1),
        inner_area.width,
        u16::try_from(content_height).unwrap_or(u16::MAX),
    );
    if !content_area.is_empty() {
        let scroll_offset = content_scroll_offset(picker, lines.len(), content_area.height);
        frame.render_widget(
            Paragraph::new(lines).scroll((scroll_offset, 0)),
            content_area,
        );
    }

    if footer_is_visible {
        let footer_y = inner_area
            .y
            .saturating_add(inner_area.height.saturating_sub(1));
        let footer_area = Rect::new(inner_area.x, footer_y, inner_area.width, 1);
        frame.render_widget(
            Paragraph::new(if picker.pending_model_id().is_some() {
                "Waiting for runtime selection..."
            } else {
                FOOTER
            }),
            footer_area,
        );
    }
}

fn centered_modal_area(area: Rect, desired_height: usize) -> Rect {
    if area.is_empty() {
        return Rect::default();
    }

    let width = area.width.min(MAX_MODAL_WIDTH);
    let height = area.height.min(
        u16::try_from(desired_height)
            .unwrap_or(MAX_MODAL_HEIGHT)
            .min(MAX_MODAL_HEIGHT),
    );
    let x = area.x.saturating_add(area.width.saturating_sub(width) / 2);
    let y = area
        .y
        .saturating_add(area.height.saturating_sub(height) / 2);
    Rect::new(x, y, width, height)
}

fn content_scroll_offset(picker: &ModelPickerState, line_count: usize, visible_height: u16) -> u16 {
    let active_line = picker
        .active_row()
        .saturating_add(usize::from(picker.error().is_some()));
    let visible_height = usize::from(visible_height);
    let maximum_offset = line_count.saturating_sub(visible_height);
    let desired_offset = active_line.saturating_sub(visible_height.saturating_sub(1));
    u16::try_from(desired_offset.min(maximum_offset)).unwrap_or(u16::MAX)
}

fn format_model_row(model: &ModelMetadata, is_active: bool, is_current: bool) -> String {
    let active_marker = if is_active { '>' } else { ' ' };
    let current_marker = if is_current { '*' } else { ' ' };
    format!(
        "{active_marker}{current_marker} {} | {} | ctx {} | in {} | out {}",
        single_line(&model.display_name),
        single_line(&model.model_id),
        model.limits.context_window_tokens,
        format_price(model.prompt_price_usd_per_million_tokens.as_deref()),
        format_price(model.completion_price_usd_per_million_tokens.as_deref()),
    )
}

fn format_price(price: Option<&str>) -> String {
    price
        .map(|price| format!("${}/M", single_line(price)))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn single_line(content: &str) -> String {
    content
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{CatalogState, ModelPickerAction, ModelPickerState, format_model_row};
    use crate::provider::{ModelLimits, ModelMetadata};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key_event(key_code: KeyCode) -> KeyEvent {
        KeyEvent::new(key_code, KeyModifiers::NONE)
    }

    fn model(
        model_id: &str,
        display_name: &str,
        prompt_price: Option<&str>,
        completion_price: Option<&str>,
    ) -> ModelMetadata {
        ModelMetadata {
            model_id: model_id.to_owned(),
            display_name: display_name.to_owned(),
            limits: ModelLimits {
                context_window_tokens: 16_384,
                maximum_output_tokens: Some(2_048),
            },
            prompt_price_usd_per_million_tokens: prompt_price.map(str::to_owned),
            completion_price_usd_per_million_tokens: completion_price.map(str::to_owned),
        }
    }

    fn loaded_picker(models: Vec<ModelMetadata>) -> ModelPickerState {
        let mut picker = ModelPickerState::new();
        picker.catalog_loaded(models);
        picker.open();
        picker
    }

    #[test]
    fn selected_model_is_first_then_names_and_ids_are_case_insensitive() {
        let picker = loaded_picker(vec![
            model("z-model", "Alpha", None, None),
            model("current-id", "Zulu", None, None),
            model("a-model", "alpha", None, None),
            model("b-model", "Alpha", None, None),
        ]);

        let ordered_ids = picker
            .filtered_models("current-id")
            .into_iter()
            .map(|model| model.model_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            ordered_ids,
            vec!["current-id", "a-model", "b-model", "z-model"]
        );
    }

    #[test]
    fn search_matches_name_and_id_without_case_sensitivity() {
        let mut picker = loaded_picker(vec![
            model("provider/Alpha", "First model", None, None),
            model("provider/beta", "Second model", None, None),
        ]);

        for character in "ALPHA".chars() {
            assert_eq!(
                picker.handle_key(key_event(KeyCode::Char(character)), "none"),
                ModelPickerAction::Continue
            );
        }
        assert_eq!(picker.filtered_models("none").len(), 1);
        assert_eq!(picker.filtered_models("none")[0].model_id, "provider/Alpha");

        picker.query.clear();
        for character in "BETA".chars() {
            picker.handle_key(key_event(KeyCode::Char(character)), "none");
        }
        assert_eq!(
            picker.filtered_models("none")[0].display_name,
            "Second model"
        );
    }

    #[test]
    fn backspace_removes_one_unicode_scalar_from_the_search_end() {
        let mut picker = loaded_picker(vec![model("model", "model", None, None)]);
        for character in "界é".chars() {
            picker.handle_key(key_event(KeyCode::Char(character)), "none");
        }

        picker.handle_key(key_event(KeyCode::Backspace), "none");
        assert_eq!(picker.query(), "界");
        picker.handle_key(key_event(KeyCode::Backspace), "none");
        assert_eq!(picker.query(), "");
    }

    #[test]
    fn navigation_clamps_at_both_filtered_result_bounds() {
        let mut picker = loaded_picker(vec![
            model("a", "A", None, None),
            model("b", "B", None, None),
        ]);
        picker.handle_key(key_event(KeyCode::Up), "none");
        assert_eq!(picker.active_row(), 0);
        picker.handle_key(key_event(KeyCode::Down), "none");
        picker.handle_key(key_event(KeyCode::Down), "none");
        assert_eq!(picker.active_row(), 1);
        picker.handle_key(key_event(KeyCode::Char('a')), "none");
        picker.handle_key(key_event(KeyCode::Down), "none");
        assert_eq!(picker.active_row(), 0);
    }

    #[test]
    fn missing_prices_are_rendered_as_unknown_without_arithmetic() {
        let row = format_model_row(
            &model("free-model", "Free", None, Some("0.25")),
            true,
            false,
        );
        assert!(row.contains("in unknown"));
        assert!(row.contains("out $0.25/M"));
        assert!(row.contains("ctx 16384"));
    }

    #[test]
    fn selection_waits_for_acknowledgment_and_failure_keeps_modal_open() {
        let model = model("selected", "Selected", Some("0.1"), Some("0.2"));
        let mut picker = loaded_picker(vec![model.clone()]);

        assert_eq!(
            picker.handle_key(key_event(KeyCode::Enter), "none"),
            ModelPickerAction::SelectModel("selected".to_owned())
        );
        assert!(picker.is_open());
        picker.accept_selection_enqueue("selected".to_owned());
        assert_eq!(picker.pending_model_id(), Some("selected"));
        assert!(picker.is_open());
        assert!(!picker.selection_acknowledged("other"));
        assert!(picker.is_open());
        picker.selection_failed("selection rejected".to_owned());
        assert_eq!(picker.pending_model_id(), None);
        assert!(picker.is_open());
        assert_eq!(picker.error(), Some("selection rejected"));

        picker.accept_selection_enqueue("selected".to_owned());
        assert!(picker.selection_acknowledged(&model.model_id));
        assert!(!picker.is_open());
        assert_eq!(picker.pending_model_id(), None);
    }

    #[test]
    fn catalog_loaded_preserves_query_and_catalog_failures_are_local() {
        let mut picker = ModelPickerState::new();
        picker.open();
        picker.handle_key(key_event(KeyCode::Char('x')), "none");
        picker.catalog_loaded(vec![model("x-model", "X", None, None)]);
        assert_eq!(picker.query(), "x");
        assert!(matches!(picker.catalog_state(), CatalogState::Loaded(_)));
        picker.catalog_failed("provider\nfailed".to_owned());
        assert_eq!(picker.error(), None);
        assert!(matches!(
            picker.catalog_state(),
            CatalogState::Failed(message) if message == "provider\nfailed"
        ));
    }
}

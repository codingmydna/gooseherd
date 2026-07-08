use console::{style, Key, Term};
use serde::Deserialize;
use std::io;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(super) struct OrchQuestionSet {
    pub(super) questions: Vec<OrchQuestion>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(super) struct OrchQuestion {
    pub(super) header: String,
    pub(super) question: String,
    #[serde(default)]
    pub(super) recommended: usize,
    pub(super) options: Vec<OrchOption>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(super) struct OrchOption {
    pub(super) label: String,
    pub(super) description: String,
    #[serde(default)]
    pub(super) preview: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Selection {
    Option(usize),
    FreeText(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OrchAnswer {
    pub(super) question_index: usize,
    pub(super) selection: Selection,
    pub(super) note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AskOutcome {
    Submitted(Vec<OrchAnswer>),
    Chat { question_index: usize, text: String },
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StateEvent {
    AnswerRecorded,
    FreeTextRequested,
    ChatRequested,
    ReviewRequested,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RowKind {
    Option(usize),
    FreeText,
    Chat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AskRow {
    pub(super) label: String,
    pub(super) description: Option<String>,
    pub(super) recommended: bool,
    pub(super) preview: Option<String>,
    kind: RowKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AskScreen {
    Question,
    Review,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReviewLine {
    pub(super) header: String,
    pub(super) question: String,
    pub(super) answer: String,
    pub(super) note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AskState {
    set: OrchQuestionSet,
    current_tab: usize,
    cursors: Vec<usize>,
    answers: Vec<Option<OrchAnswer>>,
    screen: AskScreen,
    review_cursor: usize,
}

pub(super) fn parse_orch_question_block(text: &str) -> Option<OrchQuestionSet> {
    let mut rest = text;
    while let Some(fence_start) = rest.find("```") {
        let after_fence = rest.get(fence_start + 3..)?;
        let info_end = after_fence.find('\n')?;
        let info = after_fence.get(..info_end)?.trim();
        let body_and_after = after_fence.get(info_end + 1..)?;
        let close = body_and_after.find("```")?;
        if info.starts_with("orch-question") {
            let body = body_and_after.get(..close)?;
            return parse_question_json(body);
        }
        rest = body_and_after.get(close + 3..).unwrap_or_default();
    }
    None
}

fn parse_question_json(body: &str) -> Option<OrchQuestionSet> {
    let mut set: OrchQuestionSet = serde_json::from_str(body).ok()?;
    if !(1..=3).contains(&set.questions.len()) {
        return None;
    }
    for question in &mut set.questions {
        if !(2..=4).contains(&question.options.len()) {
            return None;
        }
        if question.recommended >= question.options.len() {
            question.recommended = 0;
        }
    }
    Some(set)
}

impl AskState {
    pub(super) fn new(set: OrchQuestionSet) -> Self {
        let count = set.questions.len();
        Self {
            set,
            current_tab: 0,
            cursors: vec![0; count],
            answers: vec![None; count],
            screen: AskScreen::Question,
            review_cursor: 0,
        }
    }

    pub(super) fn current_tab(&self) -> usize {
        self.current_tab
    }

    pub(super) fn is_submit_tab(&self) -> bool {
        self.current_tab == self.set.questions.len()
    }

    pub(super) fn next_tab(&mut self) {
        self.screen = AskScreen::Question;
        self.current_tab = (self.current_tab + 1) % (self.set.questions.len() + 1);
    }

    pub(super) fn prev_tab(&mut self) {
        self.screen = AskScreen::Question;
        let tab_count = self.set.questions.len() + 1;
        self.current_tab = (self.current_tab + tab_count - 1) % tab_count;
    }

    pub(super) fn move_up(&mut self) {
        if self.screen == AskScreen::Review {
            self.review_cursor = self.review_cursor.saturating_sub(1);
            return;
        }
        if self.is_submit_tab() {
            return;
        }
        let cursor = &mut self.cursors[self.current_tab];
        *cursor = cursor.saturating_sub(1);
    }

    pub(super) fn move_down(&mut self) {
        if self.screen == AskScreen::Review {
            self.review_cursor = (self.review_cursor + 1).min(1);
            return;
        }
        if self.is_submit_tab() {
            return;
        }
        let rows = self.rows_for(self.current_tab);
        let cursor = &mut self.cursors[self.current_tab];
        *cursor = (*cursor + 1).min(rows.len().saturating_sub(1));
    }

    pub(super) fn rows_for(&self, question_index: usize) -> Vec<AskRow> {
        let question = &self.set.questions[question_index];
        let mut rows = Vec::with_capacity(question.options.len() + 2);
        let recommended = question.recommended;
        rows.push(option_row(question, recommended, true));
        for index in 0..question.options.len() {
            if index != recommended {
                rows.push(option_row(question, index, false));
            }
        }
        rows.push(AskRow {
            label: "Type something.".to_string(),
            description: Some("Write a custom answer for this question.".to_string()),
            recommended: false,
            preview: None,
            kind: RowKind::FreeText,
        });
        rows.push(AskRow {
            label: "Chat about this".to_string(),
            description: Some("Ask the planner a follow-up instead of answering yet.".to_string()),
            recommended: false,
            preview: None,
            kind: RowKind::Chat,
        });
        rows
    }

    pub(super) fn select_current(&mut self) -> Option<StateEvent> {
        if self.screen == AskScreen::Review {
            return Some(StateEvent::ReviewRequested);
        }
        if self.is_submit_tab() {
            self.enter_review();
            return Some(StateEvent::ReviewRequested);
        }
        let row = self.rows_for(self.current_tab)[self.cursors[self.current_tab]].clone();
        match row.kind {
            RowKind::Option(option_index) => {
                self.answers[self.current_tab] = Some(OrchAnswer {
                    question_index: self.current_tab,
                    selection: Selection::Option(option_index),
                    note: self.answers[self.current_tab]
                        .as_ref()
                        .and_then(|answer| answer.note.clone()),
                });
                if self.all_answered() {
                    self.enter_review();
                }
                Some(StateEvent::AnswerRecorded)
            }
            RowKind::FreeText => Some(StateEvent::FreeTextRequested),
            RowKind::Chat => Some(StateEvent::ChatRequested),
        }
    }

    pub(super) fn set_free_text(&mut self, question_index: usize, text: String) {
        let text = text.trim().to_string();
        if text.is_empty() || question_index >= self.answers.len() {
            return;
        }
        self.answers[question_index] = Some(OrchAnswer {
            question_index,
            selection: Selection::FreeText(text),
            note: self.answers[question_index]
                .as_ref()
                .and_then(|answer| answer.note.clone()),
        });
        if self.all_answered() {
            self.enter_review();
        }
    }

    pub(super) fn set_note(&mut self, question_index: usize, note: String) {
        let Some(answer) = self
            .answers
            .get_mut(question_index)
            .and_then(Option::as_mut)
        else {
            return;
        };
        let note = note.trim();
        answer.note = if note.is_empty() {
            None
        } else {
            Some(note.to_string())
        };
    }

    pub(super) fn answered_count(&self) -> usize {
        self.answers
            .iter()
            .filter(|answer| answer.is_some())
            .count()
    }

    pub(super) fn all_answered(&self) -> bool {
        self.answered_count() == self.answers.len()
    }

    pub(super) fn enter_review(&mut self) {
        self.screen = AskScreen::Review;
        self.current_tab = self.set.questions.len();
        self.review_cursor = 0;
    }

    pub(super) fn review_summary(&self) -> Vec<ReviewLine> {
        self.answers_or_recommended()
            .iter()
            .map(|answer| {
                let question = &self.set.questions[answer.question_index];
                ReviewLine {
                    header: question.header.clone(),
                    question: question.question.clone(),
                    answer: selection_label(question, &answer.selection),
                    note: answer.note.clone(),
                }
            })
            .collect()
    }

    pub(super) fn submit(&self) -> AskOutcome {
        AskOutcome::Submitted(self.answers_or_recommended())
    }

    pub(super) fn cancel(&self) -> AskOutcome {
        AskOutcome::Cancelled
    }

    fn current_row(&self) -> Option<AskRow> {
        if self.is_submit_tab() || self.screen == AskScreen::Review {
            return None;
        }
        self.rows_for(self.current_tab)
            .get(self.cursors[self.current_tab])
            .cloned()
    }

    fn answers_or_recommended(&self) -> Vec<OrchAnswer> {
        self.set
            .questions
            .iter()
            .enumerate()
            .map(|(question_index, question)| {
                self.answers[question_index].clone().unwrap_or(OrchAnswer {
                    question_index,
                    selection: Selection::Option(question.recommended),
                    note: None,
                })
            })
            .collect()
    }
}

fn option_row(question: &OrchQuestion, option_index: usize, recommended: bool) -> AskRow {
    let option = &question.options[option_index];
    AskRow {
        label: option.label.clone(),
        description: Some(option.description.clone()),
        recommended,
        preview: option.preview.clone(),
        kind: RowKind::Option(option_index),
    }
}

pub(super) fn auto_recommended_answers(set: &OrchQuestionSet) -> Vec<OrchAnswer> {
    set.questions
        .iter()
        .enumerate()
        .map(|(question_index, question)| OrchAnswer {
            question_index,
            selection: Selection::Option(question.recommended.min(question.options.len() - 1)),
            note: None,
        })
        .collect()
}

pub(super) fn format_answers_message(set: &OrchQuestionSet, answers: &[OrchAnswer]) -> String {
    let mut out = String::from(
        "The user answered the planner questions below. Continue planning with these answers and produce the plan unless another question round is truly necessary.\n\n",
    );
    for answer in answers {
        let Some(question) = set.questions.get(answer.question_index) else {
            continue;
        };
        out.push_str(&format!(
            "{}. {}\nQuestion: {}\nAnswer: {}\n",
            answer.question_index + 1,
            question.header,
            question.question,
            selection_label(question, &answer.selection)
        ));
        if let Some(note) = &answer.note {
            out.push_str(&format!("Notes: {note}\n"));
        }
        out.push('\n');
    }
    out
}

pub(super) fn qa_markdown_section(rounds: &[(OrchQuestionSet, Vec<OrchAnswer>)]) -> String {
    let mut out = String::from("## Q&A\n");
    for (round_index, (set, answers)) in rounds.iter().enumerate() {
        if rounds.len() > 1 {
            out.push_str(&format!("\n### Round {}\n", round_index + 1));
        }
        for answer in answers {
            let Some(question) = set.questions.get(answer.question_index) else {
                continue;
            };
            out.push_str(&format!(
                "\n- **{}**: {}\n  - Answer: {}\n",
                question.header,
                question.question,
                selection_label(question, &answer.selection)
            ));
            if let Some(note) = &answer.note {
                out.push_str(&format!("  - Notes: {note}\n"));
            }
        }
    }
    out.trim_end().to_string()
}

pub(super) fn chat_reply_message(
    set: &OrchQuestionSet,
    question_index: usize,
    text: &str,
) -> String {
    let question = set.questions.get(question_index);
    format!(
        "The user chose to chat about a planner question instead of answering it.\nQuestion: {}\nUser message: {}\n\nRespond to the user message in your next turn. If you still need a decision afterwards, ask another orch-question block; otherwise produce the plan.",
        question
            .map(|question| question.question.as_str())
            .unwrap_or("(unknown question)"),
        text.trim()
    )
}

fn selection_label(question: &OrchQuestion, selection: &Selection) -> String {
    match selection {
        Selection::Option(index) => question
            .options
            .get(*index)
            .map(|option| option.label.clone())
            .unwrap_or_else(|| format!("Option {}", index + 1)),
        Selection::FreeText(text) => format!("Custom: {text}"),
    }
}

pub(super) fn run_ask_ui(term: &Term, set: &OrchQuestionSet) -> io::Result<AskOutcome> {
    let _guard = TerminalGuard::new(term)?;
    let mut state = AskState::new(set.clone());

    loop {
        render(term, &state)?;
        match term.read_key()? {
            Key::ArrowUp => state.move_up(),
            Key::ArrowDown => state.move_down(),
            Key::ArrowLeft | Key::BackTab => state.prev_tab(),
            Key::ArrowRight | Key::Tab => state.next_tab(),
            Key::Escape | Key::CtrlC => return Ok(state.cancel()),
            Key::Enter => {
                if state.screen == AskScreen::Review {
                    return Ok(if state.review_cursor == 0 {
                        state.submit()
                    } else {
                        state.cancel()
                    });
                }
                let question_index = state.current_tab();
                match state.select_current() {
                    Some(StateEvent::FreeTextRequested) => {
                        let Some(text) = read_line_keyed(term, "Type your answer: ")? else {
                            return Ok(state.cancel());
                        };
                        state.set_free_text(question_index, text);
                    }
                    Some(StateEvent::ChatRequested) => {
                        let Some(text) = read_line_keyed(term, "Message to planner: ")? else {
                            return Ok(state.cancel());
                        };
                        let text = text.trim().to_string();
                        if !text.is_empty() {
                            return Ok(AskOutcome::Chat {
                                question_index,
                                text,
                            });
                        }
                    }
                    _ => {}
                }
            }
            Key::Char('1') if state.screen == AskScreen::Review => return Ok(state.submit()),
            Key::Char('2') if state.screen == AskScreen::Review => return Ok(state.cancel()),
            Key::Char('n') | Key::Char('N') => {
                if !state.is_submit_tab() && state.answers[state.current_tab()].is_some() {
                    let question_index = state.current_tab();
                    let Some(note) = read_line_keyed(term, "Notes: ")? else {
                        return Ok(state.cancel());
                    };
                    state.set_note(question_index, note);
                }
            }
            _ => {}
        }
    }
}

struct TerminalGuard<'a> {
    term: &'a Term,
}

impl<'a> TerminalGuard<'a> {
    fn new(term: &'a Term) -> io::Result<Self> {
        term.hide_cursor()?;
        Ok(Self { term })
    }
}

impl Drop for TerminalGuard<'_> {
    fn drop(&mut self) {
        let _ = self.term.show_cursor();
        let _ = self.term.clear_screen();
        let _ = self.term.flush();
    }
}

fn render(term: &Term, state: &AskState) -> io::Result<()> {
    term.clear_screen()?;
    if state.set.questions.len() > 1 {
        term.write_line(&render_tab_bar(state))?;
        term.write_line("")?;
    }
    if state.screen == AskScreen::Review {
        render_review(term, state)?;
    } else if state.is_submit_tab() {
        term.write_line("Review your answers")?;
        term.write_line("")?;
        for line in state.review_summary() {
            term.write_line(&format!("{}: {}", line.header, line.answer))?;
            if let Some(note) = line.note {
                term.write_line(&format!("  {}", style(format!("Notes: {note}")).dim()))?;
            }
        }
        term.write_line("")?;
        term.write_line("Press Enter to review and submit.")?;
    } else {
        render_question(term, state)?;
    }
    term.write_line("")?;
    term.write_line(&style("Enter to select · ↑/↓ to navigate · n to add notes · Tab to switch questions · Esc to cancel").dim().to_string())?;
    term.flush()
}

fn render_tab_bar(state: &AskState) -> String {
    let mut parts = vec!["←".to_string()];
    for (index, question) in state.set.questions.iter().enumerate() {
        let marker = if state.answers[index].is_some() {
            "✔"
        } else {
            "□"
        };
        let label = format!("{}{}", marker, truncate_chars(&question.header, 12));
        if state.current_tab() == index && state.screen != AskScreen::Review {
            parts.push(style(label).reverse().to_string());
        } else {
            parts.push(label);
        }
    }
    let submit =
        if state.current_tab() == state.set.questions.len() || state.screen == AskScreen::Review {
            style("✔Submit").reverse().to_string()
        } else {
            "✔Submit".to_string()
        };
    parts.push(submit);
    parts.push("→".to_string());
    parts.join(" ")
}

fn render_question(term: &Term, state: &AskState) -> io::Result<()> {
    let question = &state.set.questions[state.current_tab()];
    term.write_line(&style(&question.question).bold().to_string())?;
    term.write_line("")?;

    let rows = state.rows_for(state.current_tab());
    let left = question_lines(&rows, state.cursors[state.current_tab()]);
    let preview = state.current_row().and_then(|row| row.preview);
    let (_, cols) = term.size();
    if cols >= 100 {
        let preview_lines = preview
            .as_deref()
            .map(preview_box_lines)
            .unwrap_or_default();
        let max = left.len().max(preview_lines.len());
        for index in 0..max {
            let left_line = left.get(index).cloned().unwrap_or_default();
            let preview_line = preview_lines.get(index).cloned().unwrap_or_default();
            term.write_line(&format!("{left_line:<58} {preview_line}"))?;
        }
    } else {
        for line in left {
            term.write_line(&line)?;
        }
        if let Some(preview) = preview {
            term.write_line("")?;
            for line in preview_box_lines(&preview) {
                term.write_line(&line)?;
            }
        }
    }

    if state.answers[state.current_tab()].is_some() {
        term.write_line("")?;
        term.write_line(&style("Notes: press n to add notes").dim().to_string())?;
    }
    Ok(())
}

fn question_lines(rows: &[AskRow], cursor: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        if index == 1 && rows.first().is_some_and(|row| row.recommended) {
            lines.push(style("  ─ other options ─").dim().to_string());
        }
        let marker = if cursor == index { "›" } else { " " };
        let recommended = if row.recommended {
            " (recommended)"
        } else {
            ""
        };
        let label = format!("{marker} {}. {}{}", index + 1, row.label, recommended);
        if cursor == index {
            lines.push(style(label).reverse().to_string());
        } else {
            lines.push(label);
        }
        if let Some(description) = &row.description {
            lines.push(format!("   {}", style(description).dim()));
        }
    }
    lines
}

fn preview_box_lines(preview: &str) -> Vec<String> {
    let content: Vec<String> = preview
        .lines()
        .map(|line| truncate_chars(line, 42))
        .collect();
    let width = content
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(7)
        .max(7);
    let mut lines = vec![format!("┌{}┐", "─".repeat(width + 2))];
    lines.push(format!("│ {:width$} │", "Preview", width = width));
    lines.push(format!("├{}┤", "─".repeat(width + 2)));
    for line in content {
        lines.push(format!("│ {:width$} │", line, width = width));
    }
    lines.push(format!("└{}┘", "─".repeat(width + 2)));
    lines
}

fn render_review(term: &Term, state: &AskState) -> io::Result<()> {
    term.write_line(&style("Review your answers").bold().to_string())?;
    term.write_line("")?;
    for line in state.review_summary() {
        term.write_line(&format!("{}: {}", style(line.header).bold(), line.question))?;
        term.write_line(&format!("  Answer: {}", line.answer))?;
        if let Some(note) = line.note {
            term.write_line(&format!("  Notes: {note}"))?;
        }
        term.write_line("")?;
    }
    for (index, label) in ["Submit answers", "Cancel"].iter().enumerate() {
        let line = format!("{}. {}", index + 1, label);
        if state.review_cursor == index {
            term.write_line(&style(line).reverse().to_string())?;
        } else {
            term.write_line(&line)?;
        }
    }
    Ok(())
}

fn read_line_keyed(term: &Term, prompt: &str) -> io::Result<Option<String>> {
    let mut value = String::new();
    term.show_cursor()?;
    loop {
        term.clear_screen()?;
        term.write_str(prompt)?;
        term.write_str(&value)?;
        term.flush()?;
        match term.read_key()? {
            Key::Enter => {
                term.hide_cursor()?;
                return Ok(Some(value));
            }
            Key::Escape | Key::CtrlC => {
                term.hide_cursor()?;
                return Ok(None);
            }
            Key::Backspace => {
                value.pop();
            }
            Key::Char(ch) => value.push(ch),
            _ => {}
        }
    }
}

fn truncate_chars(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let keep = max.saturating_sub(1);
    format!("{}…", value.chars().take(keep).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question_json() -> &'static str {
        r#"
before
```orch-question
{
  "questions": [
    {
      "header": "Storage",
      "question": "Where should settings live?",
      "recommended": 1,
      "options": [
        {"label": "Config file", "description": "Simple but less discoverable."},
        {"label": "Database", "description": "Recommended because existing state already lives there.", "preview": "settings\n  table"}
      ]
    }
  ]
}
```
after
"#
    }

    #[test]
    fn parses_question_block_with_surrounding_text_and_preview() {
        let parsed = parse_orch_question_block(question_json()).expect("question block");

        assert_eq!(parsed.questions.len(), 1);
        assert_eq!(parsed.questions[0].header, "Storage");
        assert_eq!(parsed.questions[0].recommended, 1);
        assert_eq!(
            parsed.questions[0].options[1].preview.as_deref(),
            Some("settings\n  table")
        );
    }

    #[test]
    fn rejects_malformed_json_as_not_a_question() {
        let text = "```orch-question\n{\"questions\": [}\n```";

        assert!(parse_orch_question_block(text).is_none());
    }

    #[test]
    fn parses_multiple_questions_and_clamps_recommended_index() {
        let text = r#"```orch-question
{
  "questions": [
    {
      "header": "API",
      "question": "Which API?",
      "recommended": 99,
      "options": [
        {"label": "A", "description": "Fast."},
        {"label": "B", "description": "Flexible."}
      ]
    },
    {
      "header": "Tests",
      "question": "How broad?",
      "recommended": 0,
      "options": [
        {"label": "Unit", "description": "Focused."},
        {"label": "Integration", "description": "More coverage."}
      ]
    }
  ]
}
```"#;

        let parsed = parse_orch_question_block(text).expect("question block");

        assert_eq!(parsed.questions.len(), 2);
        assert_eq!(parsed.questions[0].recommended, 0);
    }

    #[test]
    fn rejects_invalid_question_and_option_counts() {
        let too_many_questions = r#"```orch-question
{"questions": [
  {"header":"A","question":"A?","recommended":0,"options":[{"label":"1","description":"a"},{"label":"2","description":"b"}]},
  {"header":"B","question":"B?","recommended":0,"options":[{"label":"1","description":"a"},{"label":"2","description":"b"}]},
  {"header":"C","question":"C?","recommended":0,"options":[{"label":"1","description":"a"},{"label":"2","description":"b"}]},
  {"header":"D","question":"D?","recommended":0,"options":[{"label":"1","description":"a"},{"label":"2","description":"b"}]}
]}
```"#;
        let too_few_options = r#"```orch-question
{"questions": [
  {"header":"A","question":"A?","recommended":0,"options":[{"label":"1","description":"a"}]}
]}
```"#;

        assert!(parse_orch_question_block(too_many_questions).is_none());
        assert!(parse_orch_question_block(too_few_options).is_none());
    }

    #[test]
    fn auto_selects_recommended_answers_and_formats_them() {
        let set = parse_orch_question_block(question_json()).expect("question block");

        let mut answers = auto_recommended_answers(&set);
        answers[0].note = Some("Prefer the existing migration path.".to_string());

        assert_eq!(
            answers[0].selection,
            Selection::Option(set.questions[0].recommended)
        );
        let message = format_answers_message(&set, &answers);
        let markdown = qa_markdown_section(&[(set, answers)]);

        assert!(message.contains("Database"));
        assert!(message.contains("Prefer the existing migration path."));
        assert!(markdown.starts_with("## Q&A"));
        assert!(markdown.contains("Storage"));
        assert!(markdown.contains("Database"));
    }

    #[test]
    fn state_tracks_tabs_answers_notes_review_and_cancel() {
        let set = parse_orch_question_block(
            r#"```orch-question
{"questions": [
  {"header":"API","question":"Which API?","recommended":1,"options":[{"label":"A","description":"a"},{"label":"B","description":"b"}]},
  {"header":"Tests","question":"How broad?","recommended":0,"options":[{"label":"Unit","description":"u"},{"label":"E2E","description":"e"}]}
]}
```"#,
        )
        .expect("question block");
        let mut state = AskState::new(set);

        assert_eq!(state.current_tab(), 0);
        state.next_tab();
        assert_eq!(state.current_tab(), 1);
        state.next_tab();
        assert!(state.is_submit_tab());
        state.prev_tab();
        assert_eq!(state.current_tab(), 1);

        let first_rows = state.rows_for(0);
        assert_eq!(first_rows[0].label, "B");
        assert!(first_rows[0].recommended);

        state.prev_tab();
        assert_eq!(state.select_current(), Some(StateEvent::AnswerRecorded));
        assert_eq!(state.answered_count(), 1);
        state.set_note(0, "Keep it small.".to_string());

        state.next_tab();
        assert_eq!(state.select_current(), Some(StateEvent::AnswerRecorded));
        assert!(state.all_answered());

        state.enter_review();
        let summary = state.review_summary();
        assert_eq!(summary.len(), 2);
        assert_eq!(summary[0].header, "API");
        assert_eq!(summary[0].answer, "B");
        assert_eq!(summary[0].note.as_deref(), Some("Keep it small."));

        assert!(matches!(state.cancel(), AskOutcome::Cancelled));
        assert!(matches!(state.submit(), AskOutcome::Submitted(answers) if answers.len() == 2));
    }

    #[test]
    fn free_text_and_chat_rows_signal_input_needed() {
        let set = parse_orch_question_block(question_json()).expect("question block");
        let mut state = AskState::new(set.clone());

        state.move_down();
        state.move_down();
        assert_eq!(state.select_current(), Some(StateEvent::FreeTextRequested));
        state.set_free_text(0, "Use the existing config loader.".to_string());
        assert_eq!(state.answered_count(), 1);

        let mut state = AskState::new(set);
        state.move_down();
        state.move_down();
        state.move_down();
        assert_eq!(state.select_current(), Some(StateEvent::ChatRequested));
    }
}

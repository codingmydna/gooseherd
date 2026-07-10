use rand::seq::IndexedRandom;

/// Fallback spinner labels, used only when no informative context label (the
/// provider/model or orchestration role) is available. Kept short and
/// competent rather than whimsical.
const THINKING_MESSAGES: &[&str] = &[
    "Thinking",
    "Working",
    "Analyzing",
    "Reasoning",
    "Planning",
    "Reading the code",
    "Tracing the logic",
    "Considering approaches",
    "Checking the details",
    "Untangling the problem",
    "Synthesizing an answer",
    "Reviewing the context",
    "Weighing the options",
    "Following the references",
    "Drafting a response",
    "Verifying assumptions",
    "Connecting the pieces",
    "Sketching the solution",
    "Refining the plan",
    "Double-checking the work",
];

/// Returns a random fallback thinking message.
pub fn get_random_thinking_message() -> &'static str {
    THINKING_MESSAGES
        .choose(&mut rand::rng())
        .unwrap_or(&THINKING_MESSAGES[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_message_list_is_non_empty() {
        assert!(!THINKING_MESSAGES.is_empty());
        assert!(!get_random_thinking_message().is_empty());
    }
}

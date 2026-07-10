use anyhow::Result;

use crate::action_required_manager::{ActionRequiredManager, ElicitationOutcome};
use crate::conversation::message::Message;
use crate::session::SessionManager;

pub(crate) async fn complete_elicitation_with_message(
    session_manager: &SessionManager,
    session_id: &str,
    elicitation_id: &str,
    response: ElicitationOutcome,
    response_message: &Message,
) -> Result<()> {
    let claim = ActionRequiredManager::global()
        .claim_response(session_id, elicitation_id)
        .await?;

    session_manager
        .add_message(session_id, response_message)
        .await?;

    claim.submit(response)
}

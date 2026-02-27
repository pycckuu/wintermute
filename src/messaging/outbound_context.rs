//! Build isolated outbound context from brief only.
//!
//! The outbound composer gets ONLY the brief data -- no USER.md, no memories,
//! no AGENTS.md, no main conversation history. This is the key privacy mechanism.

use super::brief::{CommitmentLevel, Constraint, TaskBrief};

/// Build the system prompt for the outbound composer.
///
/// The prompt contains only information from the brief. Budget ceilings are
/// intentionally omitted to prevent accidental disclosure.
pub fn build_outbound_system_prompt(brief: &TaskBrief) -> String {
    let mut prompt = String::with_capacity(2048);

    prompt.push_str(
        "You are writing messages in a WhatsApp conversation on behalf of the user. \
         Write in first person as if you ARE the user.\n\n",
    );

    prompt.push_str("## Your task\n");
    prompt.push_str(&brief.objective);
    prompt.push_str("\n\n");

    prompt.push_str("## Information you CAN share\n");
    for info in &brief.shareable_info {
        prompt.push_str("- ");
        prompt.push_str(info);
        prompt.push('\n');
    }
    prompt.push('\n');

    if !brief.constraints.is_empty() {
        prompt.push_str("## Constraints\n");
        for constraint in &brief.constraints {
            prompt.push_str("- ");
            prompt.push_str(&format_constraint(constraint));
            prompt.push('\n');
        }
        prompt.push('\n');
    }

    prompt.push_str("## Rules\n");
    prompt.push_str("- Share ONLY information listed above. Nothing else about the user.\n");
    prompt.push_str("- Write in first person. You ARE the user.\n");
    prompt.push_str("- Never mention AI, agents, automation, or that someone else writes.\n");
    prompt.push_str("- Never share info from other conversations or contacts.\n");
    prompt.push_str(
        "- Never share financial details beyond what constraints allow \
         (don't disclose budget ceilings -- negotiate naturally).\n",
    );
    prompt.push_str(
        "- If the contact asks for info not in the brief: \
         say you'll check and get back, then ESCALATE.\n",
    );
    prompt.push_str("- If conversation goes outside task scope: ESCALATE.\n");

    for trigger in &brief.escalation_triggers {
        prompt.push_str(&format!("- ESCALATE if: {trigger}\n"));
    }
    prompt.push('\n');

    prompt.push_str("## Commitment\n");
    match brief.commitment_level {
        CommitmentLevel::CanCommit => {
            prompt.push_str("You can confirm/agree/book if all constraints are met.\n");
        }
        CommitmentLevel::NegotiateOnly => {
            prompt.push_str(
                "Negotiate but don't finalize. Say 'let me confirm and get back to you.' \
                 Then ESCALATE for user approval.\n",
            );
        }
        CommitmentLevel::InformationOnly => {
            prompt.push_str(
                "Only gather information. Don't commit to anything. \
                 Say 'thanks, I'll think it over' when done.\n",
            );
        }
    }
    prompt.push('\n');

    if let Some(ref tone) = brief.tone {
        prompt.push_str("## Tone\n");
        prompt.push_str(tone);
        prompt.push('\n');
    } else {
        prompt.push_str(
            "## Tone\nMatch the relationship. Professional for work, casual for personal.\n",
        );
    }

    prompt
}

/// Format a constraint for display in the system prompt.
///
/// Budget ceilings are intentionally NOT included -- only the start price.
fn format_constraint(constraint: &Constraint) -> String {
    match constraint {
        Constraint::Budget {
            start,
            ceiling: _,
            currency,
        } => {
            format!("Budget: starting at {currency}{start}")
        }
        Constraint::TimeWindow { earliest, latest } => {
            format!("Time window: {earliest} to {latest}")
        }
        Constraint::MustInclude(s) => format!("Must include: {s}"),
        Constraint::MustAvoid(s) => format!("Must avoid: {s}"),
        Constraint::Custom(s) => s.clone(),
    }
}

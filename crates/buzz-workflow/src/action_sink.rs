//! Action sink trait — interface for workflow side-effects.
//!
//! The relay implements [`ActionSink`] to provide direct DB access to the
//! executor, replacing the HTTP loopback pattern.

use std::future::Future;
use std::pin::Pin;

use buzz_core::tenant::CommunityId;
use nostr::Tag;
use uuid::Uuid;

/// Verified workflow context attached to messages produced by a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowMessageRoute {
    /// The workflow run that produced the message.
    pub run_id: Uuid,
    /// The workflow definition used by the run.
    pub workflow_id: Uuid,
    /// The channel that owns the workflow run.
    pub home_channel_id: Uuid,
    /// The repository event coordinate associated with the run.
    pub repository_coordinate: String,
    /// The project event coordinate associated with the run.
    pub project_coordinate: String,
}

/// Build repository, project, and run provenance tags from verified context.
pub fn route_provenance_tags(route: &WorkflowMessageRoute) -> Result<[Tag; 3], ActionSinkError> {
    Ok([
        Tag::parse(["a", &route.repository_coordinate])
            .map_err(|e| ActionSinkError::EventBuild(format!("repository a tag: {e}")))?,
        Tag::parse(["a", &route.project_coordinate])
            .map_err(|e| ActionSinkError::EventBuild(format!("project a tag: {e}")))?,
        Tag::parse(["buzz:workflow-run", &route.run_id.to_string()])
            .map_err(|e| ActionSinkError::EventBuild(format!("workflow-run tag: {e}")))?,
    ])
}

/// Errors from action sink operations.
#[derive(Debug, thiserror::Error)]
pub enum ActionSinkError {
    /// An input parameter is malformed (e.g. invalid UUID).
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// The target channel does not exist.
    #[error("channel not found: {0}")]
    ChannelNotFound(String),
    /// The target channel is archived.
    #[error("channel is archived: {0}")]
    ChannelArchived(String),
    /// Nostr event construction or signing failed.
    #[error("event construction failed: {0}")]
    EventBuild(String),
    /// A database operation failed.
    #[error("database error: {0}")]
    Database(String),
    /// Message content is empty or whitespace-only.
    #[error("empty message content")]
    EmptyContent,
}

impl From<ActionSinkError> for crate::WorkflowError {
    fn from(e: ActionSinkError) -> Self {
        crate::WorkflowError::WebhookError(e.to_string())
    }
}

/// Interface for workflow actions that produce side effects.
///
/// Implemented by the relay to provide direct DB/event access to the executor.
/// This replaces the HTTP loopback where the executor POSTed to the relay's
/// REST API (which failed with 401 auth errors).
///
/// Returns `Pin<Box<dyn Future>>` for dyn-compatibility — required because
/// `WorkflowEngine` stores `Arc<dyn ActionSink>`.
pub trait ActionSink: Send + Sync {
    /// Post a message to a channel on behalf of a workflow owner.
    ///
    /// - `community_id`: the server-resolved community that owns the workflow
    ///   run driving this side effect. The relay-signed message is published
    ///   under *this* community, never the deployment/default tenant — the run
    ///   carries its owning community so a workflow in community B posts into B
    ///   even though the side effect has no inbound connection to bind.
    /// - `channel_id`: UUID string of the target channel
    /// - `text`: rendered message body (must not be empty/whitespace-only)
    /// - `authored_text`: the workflow owner's stored, unrendered step template;
    ///   consumers must use this rather than trigger-controlled rendered output
    ///   when attaching authority-bearing metadata
    /// - `author_pubkey`: hex-encoded pubkey of the workflow owner (used for
    ///   the `p` attribution tag; the relay keypair signs the event)
    /// - `reply_to`: when `Some(event_id_hex)`, the message is posted as a
    ///   threaded reply to that event (NIP-10 root/reply tags + real thread
    ///   metadata); when `None`, it is a top-level channel message.
    /// - `route`: verified project and workflow context for provenance tags.
    ///
    /// Returns the event ID hex string on success.
    #[expect(
        clippy::too_many_arguments,
        reason = "Keep the existing public workflow action sink contract."
    )]
    fn send_message(
        &self,
        community_id: CommunityId,
        channel_id: &str,
        text: &str,
        authored_text: &str,
        author_pubkey: &str,
        reply_to: Option<&str>,
        route: Option<WorkflowMessageRoute>,
    ) -> Pin<Box<dyn Future<Output = Result<String, ActionSinkError>> + Send + '_>>;
}

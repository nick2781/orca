use serde::{Deserialize, Serialize};

/// A request from a worker to escalate a decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationRequest {
    pub id: String,
    pub task_id: String,
    pub worker_id: String,
    pub category: EscalationCategory,
    pub summary: String,
    pub options: Vec<EscalationOption>,
    pub context: EscalationContext,
}

/// Category of escalation, determines default routing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EscalationCategory {
    ImplementationChoice,
    TestFailure,
    Timeout,
    ArchitectureChange,
    DestructiveOperation,
    ScopeExceeded,
    Conflict,
}

/// An option presented in an escalation request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationOption {
    pub id: String,
    pub desc: String,
}

/// Context attached to an escalation request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EscalationContext {
    pub relevant_files: Vec<String>,
    pub worker_recommendation: Option<String>,
}

/// How an escalation should be routed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EscalationRoute {
    AutoApprove,
    CcFirst,
    AlwaysUser,
}

/// A decision made on an escalation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationDecision {
    pub escalation_id: String,
    pub decision: String,
    pub reason: String,
    pub decided_by: DecidedBy,
}

/// Who made the escalation decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecidedBy {
    Worker,
    Cc,
    User,
}

impl EscalationCategory {
    /// Return the snake_case string representation of this category,
    /// matching the format used in configuration files.
    pub fn as_str(&self) -> &'static str {
        match self {
            EscalationCategory::ImplementationChoice => "implementation_choice",
            EscalationCategory::TestFailure => "test_failure",
            EscalationCategory::Timeout => "timeout",
            EscalationCategory::ArchitectureChange => "architecture_change",
            EscalationCategory::DestructiveOperation => "destructive_operation",
            EscalationCategory::ScopeExceeded => "scope_exceeded",
            EscalationCategory::Conflict => "conflict",
        }
    }

    /// Return the default routing for this escalation category.
    pub fn default_route(&self) -> EscalationRoute {
        match self {
            EscalationCategory::ImplementationChoice => EscalationRoute::AutoApprove,
            EscalationCategory::TestFailure => EscalationRoute::CcFirst,
            EscalationCategory::Timeout => EscalationRoute::CcFirst,
            EscalationCategory::ArchitectureChange => EscalationRoute::AlwaysUser,
            EscalationCategory::DestructiveOperation => EscalationRoute::AlwaysUser,
            EscalationCategory::ScopeExceeded => EscalationRoute::AlwaysUser,
            EscalationCategory::Conflict => EscalationRoute::CcFirst,
        }
    }
}

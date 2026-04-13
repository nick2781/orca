use crate::config::EscalationConfig;
use crate::escalation::{DecidedBy, EscalationDecision, EscalationRequest, EscalationRoute};

/// Routes escalations based on configured rules, falling back to category defaults.
pub struct EscalationRouter {
    config: EscalationConfig,
}

impl EscalationRouter {
    pub fn new(config: EscalationConfig) -> Self {
        Self { config }
    }

    /// Determine routing for an escalation based on config overrides,
    /// falling back to the category's default route.
    pub fn route(&self, escalation: &EscalationRequest) -> EscalationRoute {
        let category_str = escalation.category.as_str();

        if self.config.auto_approve.iter().any(|s| s == category_str) {
            EscalationRoute::AutoApprove
        } else if self.config.always_user.iter().any(|s| s == category_str) {
            EscalationRoute::AlwaysUser
        } else if self.config.cc_first.iter().any(|s| s == category_str) {
            EscalationRoute::CcFirst
        } else {
            escalation.category.default_route()
        }
    }

    /// Auto-resolve an escalation for the AutoApprove route.
    ///
    /// Prefers the worker's recommendation if available, otherwise falls back
    /// to the first option's id. Returns `None` if neither is available.
    pub fn auto_resolve(&self, escalation: &EscalationRequest) -> Option<EscalationDecision> {
        let decision = escalation
            .context
            .worker_recommendation
            .clone()
            .or_else(|| escalation.options.first().map(|o| o.id.clone()))?;

        Some(EscalationDecision {
            escalation_id: escalation.id.clone(),
            decision,
            reason: "auto-approved by escalation routing config".into(),
            decided_by: DecidedBy::Worker,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EscalationConfig;
    use crate::escalation::{EscalationCategory, EscalationContext, EscalationRequest};

    fn make_request(category: EscalationCategory) -> EscalationRequest {
        EscalationRequest {
            id: "esc-1".to_string(),
            task_id: "t1".to_string(),
            worker_id: "w1".to_string(),
            category,
            summary: "test escalation".to_string(),
            options: vec![],
            context: EscalationContext {
                relevant_files: vec![],
                worker_recommendation: None,
            },
        }
    }

    #[test]
    fn test_route_uses_config_auto_approve() {
        let config = EscalationConfig::default();
        let router = EscalationRouter::new(config);
        let req = make_request(EscalationCategory::ImplementationChoice);
        assert_eq!(router.route(&req), EscalationRoute::AutoApprove);
    }

    #[test]
    fn test_route_uses_config_always_user() {
        let config = EscalationConfig::default();
        let router = EscalationRouter::new(config);
        let req = make_request(EscalationCategory::DestructiveOperation);
        assert_eq!(router.route(&req), EscalationRoute::AlwaysUser);
    }

    #[test]
    fn test_route_uses_config_cc_first() {
        let config = EscalationConfig::default();
        let router = EscalationRouter::new(config);
        let req = make_request(EscalationCategory::Conflict);
        assert_eq!(router.route(&req), EscalationRoute::CcFirst);
    }

    #[test]
    fn test_route_config_override() {
        // Move "conflict" from cc_first to auto_approve.
        let config = EscalationConfig {
            auto_approve: vec!["implementation_choice".to_string(), "conflict".to_string()],
            always_user: vec![],
            cc_first: vec![],
            ..Default::default()
        };
        let router = EscalationRouter::new(config);
        let req = make_request(EscalationCategory::Conflict);
        // Default for Conflict is CcFirst, but config overrides to AutoApprove.
        assert_eq!(router.route(&req), EscalationRoute::AutoApprove);
    }

    #[test]
    fn test_route_falls_back_to_default() {
        // Config with empty lists -- everything falls back to category defaults.
        let config = EscalationConfig {
            auto_approve: vec![],
            always_user: vec![],
            cc_first: vec![],
            ..Default::default()
        };
        let router = EscalationRouter::new(config);

        assert_eq!(
            router.route(&make_request(EscalationCategory::ImplementationChoice)),
            EscalationRoute::AutoApprove
        );
        assert_eq!(
            router.route(&make_request(EscalationCategory::ArchitectureChange)),
            EscalationRoute::AlwaysUser
        );
        assert_eq!(
            router.route(&make_request(EscalationCategory::Conflict)),
            EscalationRoute::CcFirst
        );
    }
}

use orca::config::EscalationConfig;
use orca::daemon::escalation_router::EscalationRouter;
use orca::escalation::{
    DecidedBy, EscalationCategory, EscalationContext, EscalationOption, EscalationRequest,
    EscalationRoute,
};

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

fn make_request_with_options(
    category: EscalationCategory,
    options: Vec<EscalationOption>,
    recommendation: Option<String>,
) -> EscalationRequest {
    EscalationRequest {
        id: "esc-1".to_string(),
        task_id: "t1".to_string(),
        worker_id: "w1".to_string(),
        category,
        summary: "test escalation".to_string(),
        options,
        context: EscalationContext {
            relevant_files: vec![],
            worker_recommendation: recommendation,
        },
    }
}

#[test]
fn test_route_auto_approve() {
    let config = EscalationConfig::default();
    let router = EscalationRouter::new(config);
    let req = make_request(EscalationCategory::ImplementationChoice);
    assert_eq!(router.route(&req), EscalationRoute::AutoApprove);
}

#[test]
fn test_route_always_user() {
    let config = EscalationConfig::default();
    let router = EscalationRouter::new(config);
    let req = make_request(EscalationCategory::DestructiveOperation);
    assert_eq!(router.route(&req), EscalationRoute::AlwaysUser);
}

#[test]
fn test_route_cc_first() {
    let config = EscalationConfig::default();
    let router = EscalationRouter::new(config);
    let req = make_request(EscalationCategory::Conflict);
    assert_eq!(router.route(&req), EscalationRoute::CcFirst);
}

#[test]
fn test_route_config_override() {
    // Override: move "conflict" from cc_first (its default) to auto_approve via config.
    let config = EscalationConfig {
        auto_approve: vec!["implementation_choice".to_string(), "conflict".to_string()],
        always_user: vec![],
        cc_first: vec![],
        ..Default::default()
    };
    let router = EscalationRouter::new(config);
    let req = make_request(EscalationCategory::Conflict);
    assert_eq!(router.route(&req), EscalationRoute::AutoApprove);
}

#[test]
fn test_auto_resolve_uses_worker_recommendation() {
    let config = EscalationConfig::default();
    let router = EscalationRouter::new(config);

    let req = make_request_with_options(
        EscalationCategory::ImplementationChoice,
        vec![
            EscalationOption {
                id: "option_a".to_string(),
                desc: "First option".to_string(),
            },
            EscalationOption {
                id: "option_b".to_string(),
                desc: "Second option".to_string(),
            },
        ],
        Some("option_b".to_string()),
    );

    let decision = router.auto_resolve(&req).unwrap();
    assert_eq!(decision.escalation_id, "esc-1");
    assert_eq!(decision.decision, "option_b");
    assert_eq!(decision.decided_by, DecidedBy::Worker);
    assert!(decision.reason.contains("auto-approved"));
}

#[test]
fn test_auto_resolve_uses_first_option() {
    let config = EscalationConfig::default();
    let router = EscalationRouter::new(config);

    let req = make_request_with_options(
        EscalationCategory::ImplementationChoice,
        vec![
            EscalationOption {
                id: "option_a".to_string(),
                desc: "First option".to_string(),
            },
            EscalationOption {
                id: "option_b".to_string(),
                desc: "Second option".to_string(),
            },
        ],
        None, // no recommendation
    );

    let decision = router.auto_resolve(&req).unwrap();
    assert_eq!(decision.decision, "option_a");
}

#[test]
fn test_auto_resolve_no_options() {
    let config = EscalationConfig::default();
    let router = EscalationRouter::new(config);

    // No options, no recommendation.
    let req = make_request(EscalationCategory::ImplementationChoice);
    let decision = router.auto_resolve(&req);
    assert!(decision.is_none());
}

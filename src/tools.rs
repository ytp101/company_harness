//! Tools are pure functions over fictional fixtures. No file, shell, or network access.
use crate::message::Role;
use serde_json::{Value, json};
pub fn allowed(role: Role) -> &'static [&'static str] {
    match role {
        Role::Owner => &[],
        Role::Ceo => &["get_company_profile"],
        Role::Cto => &["get_company_profile", "get_product_roadmap"],
        Role::Cfo => &["get_company_profile", "get_financial_summary"],
        Role::Cmo => &["get_company_profile", "get_marketing_metrics"],
        Role::Legal => &["get_company_profile", "get_policy_notes"],
    }
}
pub fn schemas(role: Role) -> Vec<Value> {
    allowed(role).iter().map(|name| json!({"type":"function","function":{
        "name":name,"description":format!("Read the selected company context section: {name}. Read-only; missing owner data is unknown."),
        "parameters":{"type":"object","properties":{},"required":[],"additionalProperties":false}
    }})).collect()
}
pub fn dispatch(role: Role, name: &str, args: &Value) -> Result<Value, String> {
    if !allowed(role).contains(&name) {
        return Err(format!("{role} is not authorized for tool {name}"));
    }
    if !args.as_object().is_some_and(|o| o.is_empty()) {
        return Err("Arguments must be an empty JSON object {}".into());
    }
    let data = match name {
        "get_company_profile" => {
            json!({"name":"Northstar Cloud (fictional)","product":"Team planning SaaS","employees":12,"target":"Small creative agencies","goal":"Evaluate a public launch next month","currency":"USD"})
        }
        "get_product_roadmap" => {
            json!({"core_features":"ready for beta","release_blockers":["Tenant isolation audit incomplete","Restore drill not yet passed"],"open_critical_bugs":2,"estimated_remediation_weeks":3,"load_test":"500 concurrent users passed","launch_target_weeks":4})
        }
        "get_financial_summary" => {
            json!({"cash_usd":240000,"monthly_burn_usd":40000,"monthly_revenue_usd":12000,"launch_budget_usd":35000,"runway_months_at_net_burn":8.57,"note":"Launch cost is additional; revenue is uncertain"})
        }
        "get_marketing_metrics" => {
            json!({"waitlist":1800,"qualified_leads":420,"beta_customers":35,"weekly_active_beta_percent":64,"trial_to_paid_percent":12,"estimated_cac_usd":180,"campaign_ready":true,"risk":"Small sample size; retention unproven"})
        }
        "get_policy_notes" => {
            json!({"privacy_notice":"draft","data_processing_agreement":"needs review","security_audit":"pending","launch_conditions":["Approve privacy notice and DPA","Complete tenant isolation audit"],"note":"Fictional simulation; not legal advice"})
        }
        _ => return Err("Unknown tool".into()),
    };
    Ok(json!({"fictional":true,"data":data}))
}
/// Owner mode never falls back to fixtures, even when a section is empty.
pub fn dispatch_with_context(
    role: Role,
    name: &str,
    args: &Value,
    context: &crate::config::CompanyContext,
) -> Result<Value, String> {
    if !context.use_owner_data {
        return dispatch(role, name, args);
    }
    if !allowed(role).contains(&name) {
        return Err(format!("{role} is not authorized for tool {name}"));
    }
    if !args.as_object().is_some_and(|o| o.is_empty()) {
        return Err("Arguments must be an empty JSON object {}".into());
    }
    let text = context.section(name);
    Ok(
        json!({"fictional":false,"source":"owner-provided, unverified","available":!text.trim().is_empty(),"content":text,"note":"Missing information is unknown. No fictional fallback."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authorization_and_arguments() {
        assert!(dispatch(Role::Cto, "get_financial_summary", &json!({})).is_err());
        assert!(
            dispatch(
                Role::Cfo,
                "get_financial_summary",
                &json!({"file":"secret"})
            )
            .is_err()
        );
        assert!(dispatch(Role::Cfo, "get_financial_summary", &json!(null)).is_err());
        assert!(dispatch(Role::Owner, "get_company_profile", &json!({})).is_err());
        assert_eq!(
            dispatch(Role::Cfo, "get_financial_summary", &json!({})).unwrap()["fictional"],
            true
        );
    }
}

#[cfg(test)]
mod owner_context_tests {
    use super::*;
    #[test]
    fn owner_data_never_falls_back_to_fictional_finances() {
        let context = crate::config::CompanyContext {
            use_owner_data: true,
            profile: "Real bakery".into(),
            ..Default::default()
        };
        let data = dispatch_with_context(Role::Cfo, "get_financial_summary", &json!({}), &context)
            .unwrap();
        assert_eq!(data["fictional"], false);
        assert_eq!(data["available"], false);
        assert!(!data.to_string().contains("240000"));
        assert!(
            dispatch_with_context(Role::Cto, "get_financial_summary", &json!({}), &context)
                .is_err()
        );
        assert!(
            dispatch_with_context(
                Role::Cfo,
                "get_financial_summary",
                &json!({"path":"x"}),
                &context
            )
            .is_err()
        );
    }
}

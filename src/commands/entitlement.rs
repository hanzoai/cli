//! What the caller's plan lets them do, as the cloud states it.
//!
//! `GET /v1/entitlement` answers the plan table for the caller: the rung they live on,
//! the streamed Tabs they may keep open and have open, whether their other coding tools
//! may route through Hanzo, the free chat limit, and the next rung on sale. The cloud is
//! the authority and enforces every one of them; this is how the CLI says so before it
//! asks a refused question, and how it reads a refusal it was given (HTTP 402,
//! `upgrade_required`) so the person reads one sentence and a link rather than a status.
//!
//! A number the cloud did not answer is absent, never zero: a plan that could not be read
//! (`resolved` false) is Free with nothing granted, and a pre-flight that cannot ask does
//! not refuse — the cloud refuses what it must.

use hanzo_client::{Error as ApiError, Method};
use serde_json::Value;

use crate::commands::product::{envelope_error, Seam};
use crate::config::Config;

/// Where a person raises their plan.
pub const PRICING: &str = "https://hanzo.ai/pricing";

/// The rung above the caller's that is on sale.
#[derive(Debug, Clone, PartialEq)]
pub struct Next {
    pub name: String,
    /// Dollars a month; absent for a rung sold by a conversation.
    pub price: Option<f64>,
    pub contact_sales: bool,
}

/// The caller's rung of the plan table.
#[derive(Debug, Clone, PartialEq)]
pub struct Standing {
    /// The plan was read. False is Free with nothing granted.
    pub resolved: bool,
    pub plan: String,
    pub name: String,
    pub accelerate: bool,
    pub local_engine: bool,
    pub local_kai_enso: bool,
    /// Tabs the plan keeps open (-1 unlimited, 0 none) and how many are open.
    pub tabs_limit: i64,
    pub tabs_used: u64,
    pub next: Option<Next>,
}

impl Standing {
    /// Read the body of `GET /v1/entitlement`. A cloud that does not state the plan
    /// table (one from before it) answers `None`, and the caller treats that as "cannot
    /// say", not as Free.
    pub fn parse(body: &Value) -> Option<Standing> {
        let v = body.get("data").unwrap_or(body);
        let plan = v.get("plan")?.as_str()?.to_string();
        let flag = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
        let next = v.get("next").filter(|n| !n.is_null()).map(|n| Next {
            name: n.get("name").and_then(Value::as_str).unwrap_or_default().to_string(),
            price: n.get("priceMonthly").and_then(Value::as_f64),
            contact_sales: n.get("contactSales").and_then(Value::as_bool).unwrap_or(false),
        });
        Some(Standing {
            resolved: flag("resolved"),
            name: v.get("name").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or(&plan).to_string(),
            plan,
            accelerate: flag("accelerate"),
            local_engine: flag("localEngine"),
            local_kai_enso: flag("localKaiEnso"),
            tabs_limit: v.pointer("/tabs/limit").and_then(Value::as_i64).unwrap_or(0),
            tabs_used: v.pointer("/tabs/used").and_then(Value::as_u64).unwrap_or(0),
            next,
        })
    }

    /// Whether every Tab the plan keeps is open.
    pub fn tabs_full(&self) -> bool {
        self.tabs_limit >= 0 && self.tabs_used >= self.tabs_limit as u64
    }

    /// The rung that raises the plan, as a sentence: its name, its price and where.
    pub fn way_up(&self) -> String {
        match &self.next {
            Some(n) if n.contact_sales => format!("{}: contact sales at {PRICING}", n.name),
            Some(n) => match n.price {
                Some(p) => format!("{} (${p:.0} a month): {PRICING}", n.name),
                None => format!("{}: {PRICING}", n.name),
            },
            None => PRICING.to_string(),
        }
    }

    /// What acceleration needs, said when this plan lacks it.
    pub fn acceleration_refusal(&self) -> Option<String> {
        if self.accelerate {
            return None;
        }
        Some(format!(
            "Accelerating your other coding tools through Hanzo needs a subscription — you are on {}.\n  Upgrade: {}",
            self.name,
            self.way_up()
        ))
    }
}

/// `GET /v1/entitlement` through the one authenticated seam. A refusal, a transport
/// fault and a cloud that does not state the plan table are each a reason.
pub async fn read(cfg: &mut Config) -> Result<Standing, String> {
    let seam = Seam::open(cfg).await.map_err(|e| format!("{e:#}"))?;
    let reply = seam
        .send(Method::GET, "/v1/entitlement", &[], None)
        .await
        .map_err(|e| format!("{e:#}"))?;
    if !(200..300).contains(&reply.status) {
        let said = envelope_error(&reply.body).unwrap_or_default();
        return Err(format!("{} {said}", reply.status).trim_end().to_string());
    }
    Standing::parse(&reply.body).ok_or_else(|| "the cloud does not state the plan table".to_string())
}

/// A refusal of a feature the plan does not offer: HTTP 402 `upgrade_required`.
#[derive(Debug, Clone, PartialEq)]
pub struct Upgrade {
    pub feature: String,
    pub plan: String,
    /// The rung that offers it and is on sale, empty when none is.
    pub upgrade: String,
    pub message: String,
    pub url: String,
}

impl Upgrade {
    /// Read a refusal body: the problem document the cloud's own routes answer
    /// (`code` at the top) or the model API's `{"error": {...}}`.
    pub fn parse(body: &Value) -> Option<Upgrade> {
        let at = if body.get("code").is_some() { body } else { body.get("error")? };
        if at.get("code").and_then(Value::as_str) != Some("upgrade_required") {
            return None;
        }
        let text = |k: &str| at.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        let message = [text("message"), text("detail")].into_iter().find(|s| !s.is_empty())?;
        Some(Upgrade {
            feature: text("feature"),
            plan: text("plan"),
            upgrade: text("upgrade"),
            message,
            url: Some(text("url")).filter(|u| !u.is_empty()).unwrap_or_else(|| PRICING.to_string()),
        })
    }

    /// An upgrade carried by a failed call, when the call failed with one.
    pub fn of(error: &anyhow::Error) -> Option<Upgrade> {
        error.chain().find_map(|e| match e.downcast_ref::<ApiError>() {
            Some(ApiError::Api { status: 402, body }) => Upgrade::parse(body),
            _ => None,
        })
    }

    /// The sentence and the link, as a person reads them.
    pub fn say(&self) -> String {
        format!("{}\n  Upgrade: {}", self.message, self.url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pro() -> Value {
        json!({"resolved": true, "plan": "dev", "name": "Pro", "accelerate": true, "localEngine": false,
               "localKaiEnso": false, "tabs": {"limit": 2, "used": 2},
               "next": {"plan": "max-5x", "name": "Max 5x", "priceMonthly": 100.0, "contactSales": false}})
    }

    #[test]
    fn a_standing_reads_the_plan_table() {
        let s = Standing::parse(&pro()).unwrap();
        assert!(s.resolved && s.accelerate && !s.local_engine && !s.local_kai_enso);
        assert_eq!((s.plan.as_str(), s.name.as_str()), ("dev", "Pro"));
        assert!(s.tabs_full(), "2 of 2 Tabs is full");
        assert_eq!(s.way_up(), "Max 5x ($100 a month): https://hanzo.ai/pricing");
        assert!(s.acceleration_refusal().is_none(), "Pro accelerates");
    }

    #[test]
    fn unlimited_tabs_are_never_full_and_enterprise_is_a_conversation() {
        let mut v = pro();
        v["tabs"] = json!({"limit": -1, "used": 400});
        v["next"] = json!({"plan": "enterprise", "name": "Enterprise", "priceMonthly": null, "contactSales": true});
        let s = Standing::parse(&v).unwrap();
        assert!(!s.tabs_full());
        assert_eq!(s.way_up(), "Enterprise: contact sales at https://hanzo.ai/pricing");
    }

    #[test]
    fn free_is_refused_acceleration_with_the_way_up() {
        let v = json!({"resolved": true, "plan": "free", "name": "Free", "accelerate": false,
                       "tabs": {"limit": 0, "used": 0},
                       "next": {"name": "Pro", "priceMonthly": 20.0, "contactSales": false}});
        let said = Standing::parse(&v).unwrap().acceleration_refusal().unwrap();
        assert!(said.contains("needs a subscription — you are on Free"), "{said}");
        assert!(said.contains("Pro ($20 a month): https://hanzo.ai/pricing"), "{said}");
    }

    #[test]
    fn a_cloud_that_states_no_plan_table_is_cannot_say_not_free() {
        assert!(Standing::parse(&json!({"tier": "dev", "apps": {}})).is_none());
        assert!(Standing::parse(&json!({"data": pro()})).is_some(), "the enveloped form reads too");
    }

    #[test]
    fn an_upgrade_reads_from_either_error_shape() {
        let problem = json!({"status": 402, "code": "upgrade_required", "detail": "Tabs need a subscription.",
                             "feature": "tabs", "plan": "free", "upgrade": "dev", "url": "https://hanzo.ai/pricing"});
        let u = Upgrade::parse(&problem).unwrap();
        assert_eq!((u.feature.as_str(), u.plan.as_str(), u.upgrade.as_str()), ("tabs", "free", "dev"));
        assert_eq!(u.say(), "Tabs need a subscription.\n  Upgrade: https://hanzo.ai/pricing");

        let wire = json!({"type": "error", "error": {"type": "upgrade_required", "code": "upgrade_required",
                          "message": "Accelerating your other coding tools needs a subscription.", "feature": "accelerate"}});
        let u = Upgrade::parse(&wire).unwrap();
        assert_eq!(u.feature, "accelerate");
        assert_eq!(u.url, PRICING, "a refusal that names no link still points at the plans");

        assert!(Upgrade::parse(&json!({"code": "insufficient_balance", "detail": "Add credits"})).is_none());
    }

    #[test]
    fn a_failed_call_carries_its_upgrade() {
        let body = json!({"code": "upgrade_required", "detail": "Tabs need a subscription.", "feature": "tabs"});
        let err = anyhow::Error::from(ApiError::Api { status: 402, body: body.clone() }).context("could not publish");
        assert_eq!(Upgrade::of(&err).unwrap().feature, "tabs");
        let other = anyhow::Error::from(ApiError::Api { status: 500, body });
        assert!(Upgrade::of(&other).is_none(), "only a 402 is an upgrade");
    }
}

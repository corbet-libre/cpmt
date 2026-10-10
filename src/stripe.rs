//! Stripe SDK adapter. Commercial parameters come from the authenticated caller.
use crate::payments::*;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::time::Duration;
use stripe::{Client, RequestStrategy};

/// Stripe adapter using upstream authentication, encoding and API-version handling.
/// No connected-account headers, transfers, balances or wallet custody are exposed.
pub struct Stripe {
    id: String,
    client: Client,
    timeout: Duration,
    max_input_bytes: usize,
}

// Route declarations are provider capabilities, not application payment policy.
const ROUTES: &[(&str, &str, &str, &str)] = &[
    ("stripe.intent.create", "payment_intents", "", ""),
    (
        "stripe.intent.confirm",
        "payment_intents",
        "payment_intents",
        "/confirm",
    ),
    (
        "stripe.intent.capture",
        "payment_intents",
        "payment_intents",
        "/capture",
    ),
    (
        "stripe.intent.cancel",
        "payment_intents",
        "payment_intents",
        "/cancel",
    ),
    ("stripe.refund.create", "refunds", "payment_intents", ""),
    ("stripe.subscription.create", "subscriptions", "", ""),
    (
        "stripe.subscription.update",
        "subscriptions",
        "subscriptions",
        "",
    ),
    (
        "stripe.subscription.cancel",
        "subscriptions",
        "subscriptions",
        "",
    ),
];

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reference {
    resource: String,
    id: String,
}

#[derive(Deserialize)]
struct Reply {
    id: String,
    object: String,
    status: String,
    client_secret: Option<String>,
    next_action: Option<Value>,
}

impl Stripe {
    /// Construct a production adapter for Stripe's own HTTPS API.
    /// Inject credentials and resource limits; no secret or environment discovery.
    pub fn new(
        id: String,
        secret: Private,
        timeout: Duration,
        max_input_bytes: usize,
    ) -> Result<Self> {
        crate::payments::identifier(&id)?;
        let secret = std::str::from_utf8(secret.expose()).map_err(|_| Error::Invalid)?;
        if secret.is_empty() || timeout.is_zero() || max_input_bytes == 0 {
            return Err(Error::Invalid);
        }
        Ok(Self {
            id,
            client: Client::new(secret),
            timeout,
            max_input_bytes,
        })
    }

    fn parameters(&self, request: &Instruction) -> Result<Map<String, Value>> {
        if request
            .terms
            .expose()
            .len()
            .saturating_add(request.evidence.expose().len())
            > self.max_input_bytes
        {
            return Err(Error::Invalid);
        }
        let mut params: Map<String, Value> =
            serde_json::from_slice(request.terms.expose()).map_err(|_| Error::Invalid)?;
        // Member input is only a provider-issued payment-method token. It cannot
        // replace amount, currency, customer, recipient, price or renewal rules.
        if !request.evidence.expose().is_empty() {
            let token: String =
                serde_json::from_slice(request.evidence.expose()).map_err(|_| Error::Invalid)?;
            valid_id(&token)?;
            let field = if request.operation == "stripe.subscription.create" {
                "default_payment_method"
            } else {
                "payment_method"
            };
            if params.insert(field.into(), Value::String(token)).is_some() {
                return Err(Error::Invalid);
            }
        }
        if forbidden(&Value::Object(params.clone())) {
            return Err(Error::Unsupported);
        }
        // Object ownership is established by the journal's parent binding.
        if params.contains_key("payment_intent") || params.contains_key("charge") {
            return Err(Error::Invalid);
        }
        Ok(params)
    }

    fn reduce(reply: Reply, resource: &str, expected: Option<&str>) -> Result<Observation> {
        valid_id(&reply.id)?;
        let object = match resource {
            "payment_intents" => "payment_intent",
            "refunds" => "refund",
            "subscriptions" => "subscription",
            _ => return Err(Error::Invalid),
        };
        if reply.object != object || expected.is_some_and(|id| id != reply.id) {
            return Err(Error::Rejected);
        }
        let state = match (resource, reply.status.as_str()) {
            ("payment_intents", "succeeded") => State::Collected,
            ("payment_intents", "requires_capture") => State::Authorized,
            ("payment_intents", "canceled") => State::Released,
            ("refunds", "succeeded") => State::Refunded,
            ("refunds", "failed" | "canceled") => State::Failed,
            ("subscriptions", "active" | "trialing") => State::Active,
            ("subscriptions", "canceled") => State::Cancelled,
            (
                _,
                "requires_action"
                | "requires_payment_method"
                | "requires_confirmation"
                | "past_due"
                | "unpaid",
            ) => State::RequiresAction,
            (_, "processing" | "pending" | "incomplete" | "paused") => State::Pending,
            (_, "incomplete_expired") => State::Failed,
            _ => State::Unknown,
        };
        let continuation = serde_json::to_vec(&Reference {
            resource: resource.into(),
            id: reply.id.clone(),
        })
        .map_err(|_| Error::Invalid)?;
        let member_action = if state == State::RequiresAction {
            Some(Private::new(serde_json::to_vec(&serde_json::json!({"client_secret":reply.client_secret, "next_action":reply.next_action})).map_err(|_|Error::Invalid)?))
        } else {
            None
        };
        Ok(Observation {
            state,
            valid_until: None,
            continuation: Some(Private::new(continuation)),
            claim: Some(Private::new(
                format!("stripe:{object}:{}:{state:?}", reply.id).into_bytes(),
            )),
            member_action,
        })
    }
}

impl Adapter for Stripe {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            id: self.id.clone(),
            operations: ROUTES.iter().map(|r| r.0.into()).collect(),
        }
    }
    async fn execute(
        &self,
        request: &Instruction,
        key: &str,
        previous: Option<&Private>,
    ) -> Result<Observation> {
        let (_, resource, parent_resource, suffix) = ROUTES
            .iter()
            .find(|r| r.0 == request.operation)
            .ok_or(Error::Unsupported)?;
        let mut params = self.parameters(request)?;
        let parent = previous.map(reference).transpose()?;
        if parent.as_ref().map_or("", |r| r.resource.as_str()) != *parent_resource {
            return Err(Error::Rejected);
        }
        let mut path = (*resource).to_owned();
        let mut expected = None;
        if let Some(parent) = &parent {
            if *resource == "refunds" {
                params.insert("payment_intent".into(), Value::String(parent.id.clone()));
            } else {
                path.push('/');
                path.push_str(&parent.id);
                path.push_str(suffix);
                expected = Some(parent.id.as_str());
            }
        }
        let client = self
            .client
            .clone()
            .with_strategy(RequestStrategy::Idempotent(key.into()));
        let future = async {
            if request.operation == "stripe.subscription.cancel" {
                client.delete_query::<Reply, _>(&path, &params).await
            } else {
                client.post_form::<Reply, _>(&path, &params).await
            }
        };
        match tokio::time::timeout(self.timeout, future).await {
            Ok(Ok(reply)) => Ok(Self::reduce(reply, resource, expected)
                .unwrap_or_else(|_| Observation::fact(State::Unknown))),
            // A transport/server/decoding error cannot prove that nothing happened.
            _ => Ok(Observation::fact(State::Unknown)),
        }
    }
    async fn observe(
        &self,
        _operation: &str,
        continuation: Option<&Private>,
    ) -> Result<Observation> {
        let Some(continuation) = continuation else {
            return Ok(Observation::fact(State::Unknown));
        };
        let reference = reference(continuation)?;
        let path = format!("{}/{}", reference.resource, reference.id);
        match tokio::time::timeout(self.timeout, self.client.get::<Reply>(&path)).await {
            Ok(Ok(reply)) => Self::reduce(reply, &reference.resource, Some(&reference.id)),
            _ => Ok(Observation {
                continuation: Some(Private::new(continuation.expose().to_vec())),
                ..Observation::fact(State::Unknown)
            }),
        }
    }
}

fn reference(bytes: &Private) -> Result<Reference> {
    let value: Reference = serde_json::from_slice(bytes.expose()).map_err(|_| Error::Invalid)?;
    valid_id(&value.id)?;
    if !["payment_intents", "refunds", "subscriptions"].contains(&value.resource.as_str()) {
        return Err(Error::Invalid);
    }
    Ok(value)
}

fn valid_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 256
        || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err(Error::Invalid);
    }
    Ok(())
}

fn forbidden(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(key, value)| {
            [
                "transfer_data",
                "on_behalf_of",
                "application_fee_amount",
                "application_fee_percent",
            ]
            .contains(&key.as_str())
                || key.contains(['[', ']'])
                || key.chars().any(char::is_control)
                || forbidden(value)
        }),
        Value::Array(values) => values.iter().any(forbidden),
        _ => false,
    }
}

#[cfg(test)]
#[path = "tests/stripe.rs"]
mod tests;

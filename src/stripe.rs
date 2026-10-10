//! Stripe SDK adapter. Commercial parameters come from the authenticated caller.
use crate::cost::{Dispatch, Mode};
use crate::details::ProviderReply;
use crate::payments::*;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::time::Duration;
use stripe::{Client, ClientBuilder, IdempotencyKey, RequestStrategy};
use stripe_client_core::{RequestBuilder, StripeMethod};
use tracing::instrument::WithSubscriber;

/// Stripe adapter using upstream authentication, encoding and API-version handling.
/// No connected-account headers, transfers, balances or wallet custody are exposed.
pub struct Stripe {
    mode: Mode,
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
    #[serde(default)]
    financial: Option<Financial>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Financial {
    amount: u64,
    currency: String,
}
impl Financial {
    fn from_terms(terms: &Map<String, Value>, parent: Option<&Self>) -> Option<Self> {
        let amount = terms
            .get("amount_to_capture")
            .or_else(|| terms.get("amount"))
            .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
            .or_else(|| parent.map(|p| p.amount))?;
        let currency = terms
            .get("currency")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| parent.map(|p| p.currency.clone()))?;
        Some(Self { amount, currency })
    }
    fn satisfied(
        &self,
        amount: Option<u64>,
        received: Option<u64>,
        capturable: Option<u64>,
        currency: Option<&str>,
        state: State,
    ) -> bool {
        let amount = match state {
            State::Collected => received,
            State::Authorized => capturable,
            State::Refunded | State::Released => amount,
            _ => None,
        };
        amount == Some(self.amount)
            && currency.is_some_and(|c| c.eq_ignore_ascii_case(&self.currency))
    }
}

#[derive(Deserialize)]
struct Reply {
    id: String,
    object: String,
    status: String,
    client_secret: Option<String>,
    next_action: Option<Value>,
    amount: Option<u64>,
    amount_received: Option<u64>,
    amount_capturable: Option<u64>,
    currency: Option<String>,
}

fn decode(mut object: miniserde::json::Object) -> Result<Reply> {
    // Reduce the SDK's transient object before crossing into our typed schema.
    // Serde also bounds nesting in provider-supplied member actions.
    object.retain(|key, _| {
        [
            "id",
            "object",
            "status",
            "client_secret",
            "next_action",
            "amount",
            "amount_received",
            "amount_capturable",
            "currency",
        ]
        .contains(&key.as_str())
    });
    let json = zeroize::Zeroizing::new(miniserde::json::to_string(&object));
    serde_json::from_str(&json).map_err(|_| Error::Rejected)
}

impl Stripe {
    /// Construct a production adapter for Stripe's own HTTPS API.
    /// Inject credentials and resource limits; no secret or environment discovery.
    pub fn new(
        id: String,
        secret: Private,
        timeout: Duration,
        max_input_bytes: usize,
        mode: Mode,
    ) -> Result<Self> {
        crate::payments::identifier(&id)?;
        let secret = std::str::from_utf8(secret.expose()).map_err(|_| Error::Invalid)?;
        if secret.is_empty()
            || timeout.is_zero()
            || max_input_bytes == 0
            || mode == Mode::Simulation
        {
            return Err(Error::Invalid);
        }
        Ok(Self {
            mode,
            id,
            client: ClientBuilder::new(secret)
                .build()
                .map_err(|_| Error::Invalid)?,
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
            financial: None,
        })
        .expect("provider reference strings serialize to JSON");
        let member_action = if state == State::RequiresAction {
            Some(Private::new(serde_json::to_vec(&serde_json::json!({"client_secret":reply.client_secret, "next_action":reply.next_action})).expect("JSON value serializes")))
        } else {
            None
        };
        Ok(Observation {
            satisfies_terms: false,
            actual_cost: None,
            state,
            valid_until: None,
            continuation: Some(Private::new(continuation)),
            claim: Some(Private::new(
                format!("stripe:{object}:{}:{state:?}", reply.id).into_bytes(),
            )),
            member_action,
            upstream: Vec::new(),
        })
    }

    fn project(
        reply: miniserde::json::Object,
        resource: &str,
        expected: Option<&str>,
        financial: Option<&Financial>,
    ) -> Observation {
        let payload = Private::new(miniserde::json::to_string(&reply).into_bytes());
        let mut observation = decode(reply)
            .and_then(|reply| {
                let money = (
                    reply.amount,
                    reply.amount_received,
                    reply.amount_capturable,
                    reply.currency.clone(),
                );
                let mut observation = Self::reduce(reply, resource, expected)?;
                if let Some(terms) = financial {
                    observation.satisfies_terms = terms.satisfied(
                        money.0,
                        money.1,
                        money.2,
                        money.3.as_deref(),
                        observation.state,
                    );
                    if let Some(retained) = observation.continuation.as_ref() {
                        let mut value = reference(retained)?;
                        value.financial = Some(terms.clone());
                        observation.continuation = Some(Private::new(
                            serde_json::to_vec(&value).expect("reference serializes"),
                        ));
                    }
                }
                Ok(observation)
            })
            .unwrap_or_else(|_| Observation::fact(State::Unknown));
        observation.upstream.push(ProviderReply {
            phase: "response",
            http_status: None,
            payload,
        });
        observation
    }
}

impl Adapter for Stripe {
    fn mode(&self) -> Mode {
        self.mode
    }
    fn max_input_bytes(&self) -> usize {
        self.max_input_bytes
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            id: self.id.clone(),
            operations: ROUTES.iter().map(|r| r.0.into()).collect(),
        }
    }
    async fn execute(
        &self,
        request: &Dispatch<'_>,
        key: &str,
        previous: Option<&Private>,
    ) -> Result<Observation> {
        request.check(&self.id, self.mode)?;
        if request.kind != Action::Execute {
            return Err(Error::Invalid);
        }
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
        let path = format!("/{path}");
        let financial =
            Financial::from_terms(&params, parent.as_ref().and_then(|p| p.financial.as_ref()));
        let cancel = request.operation == "stripe.subscription.cancel";
        let method = if cancel {
            StripeMethod::Delete
        } else {
            StripeMethod::Post
        };
        let request = RequestBuilder::new(method, path);
        let request = if params.is_empty() {
            request
        } else if cancel {
            request.query(&params)
        } else {
            request.form(&params)
        };
        let request = request
            .customize::<miniserde::json::Object>()
            .request_strategy(RequestStrategy::Idempotent(
                IdempotencyKey::new(key).map_err(|_| Error::Invalid)?,
            ));
        // The SDK instruments paths and idempotency keys. Keep those private even
        // when the host enables tracing; the override is scoped to each poll.
        let future = request
            .send(&self.client)
            .with_subscriber(tracing::subscriber::NoSubscriber::default());
        let uncertain = || Observation {
            // Known-object operations can still be read after a lost response.
            // Refund creation returns a different object; its ID remains unknown.
            continuation: parent
                .as_ref()
                .filter(|p| p.resource == *resource)
                .map(|p| {
                    Private::new(
                        serde_json::to_vec(p)
                            .expect("provider reference strings serialize to JSON"),
                    )
                }),
            ..Observation::fact(State::Unknown)
        };
        match tokio::time::timeout(self.timeout, future).await {
            Ok(Ok(reply)) => {
                let mut observation = Self::project(reply, resource, expected, financial.as_ref());
                if observation.continuation.is_none() {
                    observation.continuation = uncertain().continuation;
                }
                Ok(observation)
            }
            // A transport/server/decoding error cannot prove that nothing happened.
            _ => Ok(uncertain()),
        }
    }
    async fn observe(
        &self,
        request: &Dispatch<'_>,
        continuation: Option<&Private>,
    ) -> Result<Observation> {
        request.check(&self.id, self.mode)?;
        if request.kind != Action::Observe {
            return Err(Error::Invalid);
        }
        let Some(continuation) = continuation else {
            return Ok(Observation::fact(State::Unknown));
        };
        let reference = reference(continuation)?;
        let path = format!("{}/{}", reference.resource, reference.id);
        let read = RequestBuilder::new(StripeMethod::Get, format!("/{path}"))
            .customize::<miniserde::json::Object>();
        match tokio::time::timeout(
            self.timeout,
            read.send(&self.client)
                .with_subscriber(tracing::subscriber::NoSubscriber::default()),
        )
        .await
        {
            Ok(Ok(reply)) => {
                let mut observation = Self::project(
                    reply,
                    &reference.resource,
                    Some(&reference.id),
                    reference.financial.as_ref(),
                );
                if observation.continuation.is_none() {
                    observation.continuation = Some(Private::new(continuation.expose().to_vec()));
                }
                Ok(observation)
            }
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

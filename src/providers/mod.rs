//! Optional provider adapters using bounded, single-dispatch HTTPS requests.
//! See `docs/PROVIDERS.md` for operations, native terms and reconciliation limits.
use crate::cost::{Dispatch, Mode};
use crate::details::ProviderReply;
use crate::payments::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{
    Client, Method, Url,
    header::{HeaderMap, HeaderValue},
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tracing::instrument::WithSubscriber;
use zeroize::Zeroizing;
mod profile;
mod projection;

/// Built-in integrations. The core `Adapter` contract remains open to others.
pub enum Provider {
    /// Mollie profile API key or OAuth bearer token.
    Mollie {
        /// OAuth test mode. Profile API keys require `None`.
        test_mode: Option<bool>,
    },
    /// GoCardless account access token; mandates are supplied by trusted terms.
    GoCardless {
        /// Explicit sandbox selection.
        sandbox: bool,
    },
    /// Square access token and explicit API contract version.
    Square {
        /// Explicit sandbox selection.
        sandbox: bool,
        /// API version date, supplied by the host.
        version: String,
    },
    /// A configured BTCPay server and store-scoped Greenfield API key.
    Btcpay {
        /// HTTPS server base URL, including any installation prefix.
        base: String,
        /// Operator-owned store identifier.
        store: String,
    },
    /// CoinGate API application token.
    CoinGate {
        /// Explicit sandbox selection.
        sandbox: bool,
    },
}

/// Limits supplied by the host, independent of commercial payment policy.
#[derive(Clone, Copy)]
pub struct Limits {
    /// Total HTTP deadline, including response consumption.
    pub timeout: Duration,
    /// Combined commercial terms and member input limit.
    pub input_bytes: usize,
    /// Maximum response body bytes before parsing.
    pub response_bytes: usize,
}

/// One explicitly configured provider/account. No automatic fallback or retry.
/// Credentials, raw responses and input are never exposed through Debug/errors.
pub struct Rest {
    mode: Mode,
    id: String,
    provider: Provider,
    base: Url,
    client: Client,
    limits: Limits,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Amount {
    value: String,
    currency: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reference {
    resource: String,
    id: String,
    parent: Option<String>,
    amount: Amount,
}
struct Prepared {
    method: Method,
    path: String,
    body: Value,
    resource: &'static str,
    parent: Option<String>,
    expected: Option<String>,
    amount: Amount,
}

impl Rest {
    /// Inject an account credential, provider configuration and resource limits.
    /// No environment, filesystem, secret-manager or provider-account discovery.
    pub fn new(
        id: String,
        provider: Provider,
        credential: Private,
        limits: Limits,
        mode: Mode,
    ) -> Result<Self> {
        Self::build(id, provider, credential, limits, None, mode)
    }
    fn build(
        id: String,
        provider: Provider,
        credential: Private,
        limits: Limits,
        fixture: Option<Url>,
        mode: Mode,
    ) -> Result<Self> {
        crate::payments::identifier(&id)?;
        if mode == Mode::Simulation {
            return Err(Error::Invalid);
        }
        let sandbox = match &provider {
            Provider::Square { sandbox, .. }
            | Provider::GoCardless { sandbox }
            | Provider::CoinGate { sandbox } => Some(*sandbox),
            Provider::Mollie { test_mode } => *test_mode,
            Provider::Btcpay { .. } => None,
        };
        if sandbox.is_some_and(|sandbox| sandbox != (mode == Mode::Sandbox)) {
            return Err(Error::Invalid);
        }
        let (endpoint, prefix) = provider.endpoint()?;
        let local = fixture.is_some();
        let mut base = fixture.unwrap_or(endpoint);
        let scheme_ok = base.scheme() == "https"
            || (local && base.scheme() == "http" && base.host_str() == Some("127.0.0.1"));
        if !scheme_ok
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || limits.timeout.is_zero()
            || limits.input_bytes == 0
            || limits.response_bytes == 0
        {
            return Err(Error::Invalid);
        }
        base.set_path(&format!("{}/", base.path().trim_end_matches('/')));
        let secret = std::str::from_utf8(credential.expose()).map_err(|_| Error::Invalid)?;
        if secret.is_empty() {
            return Err(Error::Invalid);
        }
        let auth = Zeroizing::new(format!("{prefix} {secret}"));
        let mut authorization = HeaderValue::from_str(&auth).map_err(|_| Error::Invalid)?;
        authorization.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert("authorization", authorization);
        match &provider {
            Provider::Square { version, .. } => {
                headers.insert(
                    "square-version",
                    HeaderValue::from_str(version).map_err(|_| Error::Invalid)?,
                );
            }
            Provider::GoCardless { .. } => {
                headers.insert("gocardless-version", HeaderValue::from_static("2015-07-06"));
            }
            _ => {}
        }
        let client = Client::builder()
            .default_headers(headers)
            .timeout(limits.timeout)
            .https_only(!local)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .build()
            .map_err(http_error)?;
        Ok(Self {
            mode,
            id,
            provider,
            base,
            client,
            limits,
        })
    }
    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        key: Option<&str>,
    ) -> Result<ProviderReply> {
        let url = self.base.join(path).map_err(|_| Error::Invalid)?;
        let read = method == Method::GET;
        let mut request = self.client.request(method, url);
        if let Provider::Mollie {
            test_mode: Some(mode),
        } = self.provider
            && read
        {
            request = request.query(&[("testmode", mode)]);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        if let Some(key) = key {
            request = request.header("idempotency-key", key);
        }
        let mut response = request.send().await.map_err(http_error)?;
        let status = response.status().as_u16();
        if response
            .content_length()
            .is_some_and(|n| n > self.limits.response_bytes as u64)
        {
            return Err(Error::Rejected);
        }
        let mut bytes = Zeroizing::new(Vec::new());
        while let Some(chunk) = response.chunk().await.map_err(http_error)? {
            if bytes.len().saturating_add(chunk.len()) > self.limits.response_bytes {
                return Err(Error::Rejected);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(ProviderReply {
            phase: "response",
            http_status: Some(status),
            payload: Private::new(bytes.to_vec()),
        })
    }
    fn reduce(&self, reply: Value, prepared: &Prepared) -> Result<Observation> {
        let object = self.provider.object(&reply, prepared.resource)?;
        let id = object.get("id").and_then(scalar).ok_or(Error::Rejected)?;
        valid_id(&id)?;
        if prepared
            .expected
            .as_ref()
            .is_some_and(|expected| expected != &id)
        {
            return Err(Error::Rejected);
        }
        if let Provider::Btcpay { store, .. } = &self.provider
            && object["storeId"].as_str() != Some(store)
        {
            return Err(Error::Rejected);
        }
        if prepared.resource == "refunds" {
            let parent = match &self.provider {
                Provider::Mollie { .. } => object.get("paymentId"),
                Provider::Square { .. } => object.get("payment_id"),
                _ => object.pointer("/links/payment"),
            }
            .and_then(Value::as_str)
            .ok_or(Error::Rejected)?;
            if prepared.parent.as_deref() != Some(parent) {
                return Err(Error::Rejected);
            }
        }
        let state = self
            .provider
            .state(object, prepared.resource, &prepared.amount);
        let reference = Reference {
            resource: prepared.resource.into(),
            id: id.clone(),
            parent: prepared.parent.clone(),
            amount: prepared.amount.clone(),
        };
        // A real partial authorization still needs an ID for cancellation.
        // It does not establish the requested amount or a spendable receipt.
        let Ok(state) = state else {
            return Ok(uncertain(Some(&reference)));
        };
        let valid_until = if state == State::Authorized {
            match self.provider.authorization_expiry(object) {
                Ok(expiry) => expiry,
                Err(_) => return Ok(uncertain(Some(&reference))),
            }
        } else {
            None
        };
        // IDs and expected amounts are the only retained provider fields.
        let continuation = encode(&reference);
        let claim = Private::new(
            serde_json::to_vec(&(self.provider.name(), prepared.resource, id, state))
                .expect("fixed receipt fields serialize"),
        );
        let member_action = self
            .provider
            .action(object)
            .map(|action| Private::new(serde_json::to_vec(action).expect("JSON serializes")));
        Ok(Observation {
            satisfies_terms: true,
            actual_cost: None,
            state,
            valid_until,
            continuation: Some(continuation),
            claim: Some(claim),
            member_action,
            upstream: Vec::new(),
        })
    }

    fn project(&self, response: ProviderReply, prepared: &Prepared) -> Observation {
        let mut result = if response
            .http_status
            .is_some_and(|code| (200..300).contains(&code))
        {
            serde_json::from_slice(response.payload.expose())
                .map_err(|_| Error::Rejected)
                .and_then(|reply| self.reduce(reply, prepared))
                .unwrap_or_else(|_| Observation::fact(State::Unknown))
        } else {
            Observation::fact(State::Unknown)
        };
        result.upstream.push(response);
        result
    }
}
impl Adapter for Rest {
    fn mode(&self) -> Mode {
        self.mode
    }
    fn max_input_bytes(&self) -> usize {
        self.limits.input_bytes
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            id: self.id.clone(),
            operations: self
                .provider
                .operations()
                .iter()
                .map(|op| format!("{}.{}", self.provider.name(), op))
                .collect(),
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
        if request
            .terms
            .expose()
            .len()
            .saturating_add(request.evidence.expose().len())
            > self.limits.input_bytes
        {
            return Err(Error::Invalid);
        }
        let operation = self.provider.operation(&request.operation)?;
        let parent = previous.map(decode).transpose()?;
        let mut terms: Map<String, Value> =
            serde_json::from_slice(request.terms.expose()).map_err(|_| Error::Invalid)?;
        if forbidden(&Value::Object(terms.clone())) {
            return Err(Error::Unsupported);
        }
        self.provider
            .member_input(operation, &mut terms, request.evidence.expose())?;
        // 43 ASCII characters: Square's 45-character limit, with full 256-bit entropy.
        let key = URL_SAFE_NO_PAD.encode(Sha256::digest(key.as_bytes()));
        let prepared = self
            .provider
            .prepare(operation, terms, &key, parent.as_ref())?;
        let header_key = matches!(
            self.provider,
            Provider::Mollie { .. } | Provider::GoCardless { .. }
        )
        .then_some(key.as_str());
        let response = self
            .send(
                prepared.method.clone(),
                &prepared.path,
                Some(&prepared.body),
                header_key,
            )
            .with_subscriber(tracing::subscriber::NoSubscriber::default())
            .await;
        let mut result = response.map_or_else(
            |_| Observation::fact(State::Unknown),
            |reply| self.project(reply, &prepared),
        );
        if result.continuation.is_none() {
            result.continuation = parent
                .as_ref()
                .filter(|p| Some(&p.id) == prepared.expected.as_ref())
                .map(encode);
        }
        Ok(result)
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
        self.provider.operation(&request.operation)?;
        let reference = decode(continuation.ok_or(Error::Unsupported)?)?;
        let prepared = self.provider.read(&reference)?;
        let reply = self
            .send(Method::GET, &prepared.path, None, None)
            .with_subscriber(tracing::subscriber::NoSubscriber::default())
            .await;
        let mut result = reply.map_or_else(
            |_| Observation::fact(State::Unknown),
            |reply| self.project(reply, &prepared),
        );
        if result.continuation.is_none() {
            result.continuation = Some(encode(&reference));
        }
        Ok(result)
    }
}
fn http_error(_: reqwest::Error) -> Error {
    Error::Rejected
}
fn valid_id(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 256
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(Error::Invalid);
    }
    Ok(())
}
fn encode(reference: &Reference) -> Private {
    Private::new(serde_json::to_vec(reference).expect("fixed reference fields serialize"))
}
fn decode(value: &Private) -> Result<Reference> {
    let reference: Reference =
        serde_json::from_slice(value.expose()).map_err(|_| Error::Invalid)?;
    valid_id(&reference.id)?;
    if let Some(parent) = &reference.parent {
        valid_id(parent)?;
    }
    reference.amount.validate()?;
    Ok(reference)
}
fn uncertain(reference: Option<&Reference>) -> Observation {
    Observation {
        continuation: reference.map(encode),
        ..Observation::fact(State::Unknown)
    }
}
fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}
impl Amount {
    fn from(value: &Value, amount_key: &str, currency_key: &str) -> Result<Self> {
        let amount = Self {
            value: value
                .get(amount_key)
                .and_then(scalar)
                .ok_or(Error::Invalid)?,
            currency: value
                .get(currency_key)
                .and_then(Value::as_str)
                .ok_or(Error::Invalid)?
                .into(),
        };
        amount.validate()?;
        Ok(amount)
    }
    fn validate(&self) -> Result<()> {
        let value = Decimal::from_str_exact(&self.value).map_err(|_| Error::Invalid)?;
        if value <= Decimal::ZERO
            || self.currency.is_empty()
            || self.currency.len() > 32
            || !self.currency.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    fn matches(&self, other: Self) -> bool {
        self.currency == other.currency
            && Decimal::from_str_exact(&self.value).ok()
                == Decimal::from_str_exact(&other.value).ok()
    }
}
fn forbidden(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(key, value)| {
            [
                "transfer_data",
                "on_behalf_of",
                "applicationFee",
                "app_fee",
                "application_fee_amount",
                "application_fee_percent",
                "app_fee_money",
                "app_fee_allocations",
                "routing",
                "split",
                "splits",
                "split_payments",
                "destination",
                "destination_id",
                "payout_address",
                "creditor",
            ]
            .contains(&key.as_str())
                || forbidden(value)
        }),
        Value::Array(values) => values.iter().any(forbidden),
        _ => false,
    }
}
#[cfg(test)]
#[path = "../tests/providers.rs"]
mod tests;

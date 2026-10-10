use super::*;
impl Provider {
    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::Mollie { .. } => "mollie",
            Self::Square { .. } => "square",
            Self::GoCardless { .. } => "gocardless",
            Self::Btcpay { .. } => "btcpay",
            Self::CoinGate { .. } => "coingate",
        }
    }
    pub(super) fn endpoint(&self) -> Result<(Url, &'static str)> {
        let (base, prefix) = match self {
            Self::Mollie { .. } => ("https://api.mollie.com/v2/", "Bearer"),
            Self::GoCardless { sandbox } => (
                if *sandbox {
                    "https://api-sandbox.gocardless.com/"
                } else {
                    "https://api.gocardless.com/"
                },
                "Bearer",
            ),
            Self::Square { sandbox, version } => {
                if version.len() != 10
                    || !version.bytes().enumerate().all(|(i, b)| {
                        if i == 4 || i == 7 {
                            b == b'-'
                        } else {
                            b.is_ascii_digit()
                        }
                    })
                {
                    return Err(Error::Invalid);
                }
                (
                    if *sandbox {
                        "https://connect.squareupsandbox.com/v2/"
                    } else {
                        "https://connect.squareup.com/v2/"
                    },
                    "Bearer",
                )
            }
            Self::Btcpay { base, store } => {
                valid_id(store)?;
                (base.as_str(), "token")
            }
            Self::CoinGate { sandbox } => (
                if *sandbox {
                    "https://api-sandbox.coingate.com/api/v2/"
                } else {
                    "https://api.coingate.com/api/v2/"
                },
                "Token",
            ),
        };
        Ok((Url::parse(base).map_err(|_| Error::Invalid)?, prefix))
    }
    pub(super) fn operations(&self) -> &'static [&'static str] {
        match self {
            Self::Mollie { .. } | Self::GoCardless { .. } => {
                &["payment.create", "payment.cancel", "refund.create"]
            }
            Self::Square { .. } => &[
                "payment.create",
                "payment.capture",
                "payment.cancel",
                "refund.create",
            ],
            Self::Btcpay { .. } => &["invoice.create"],
            Self::CoinGate { .. } => &["order.create"],
        }
    }
    pub(super) fn operation<'a>(&self, operation: &'a str) -> Result<&'a str> {
        let (provider, operation) = operation.split_once('.').ok_or(Error::Unsupported)?;
        if provider != self.name() || !self.operations().contains(&operation) {
            return Err(Error::Unsupported);
        }
        Ok(operation)
    }
    pub(super) fn member_input(
        &self,
        operation: &str,
        terms: &mut Map<String, Value>,
        evidence: &[u8],
    ) -> Result<()> {
        if evidence.is_empty() {
            return Ok(());
        }
        let field = match (self, operation) {
            (Self::Mollie { .. }, "payment.create") => "cardToken",
            (Self::Square { .. }, "payment.create") => "source_id",
            _ => return Err(Error::Unsupported),
        };
        let token: String = serde_json::from_slice(evidence).map_err(|_| Error::Invalid)?;
        if token.is_empty() || terms.insert(field.into(), Value::String(token)).is_some() {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    pub(super) fn amount(&self, value: &Value) -> Result<Amount> {
        match self {
            Self::Mollie { .. } => Amount::from(&value["amount"], "value", "currency"),
            Self::Square { .. } => Amount::from(&value["amount_money"], "amount", "currency"),
            Self::GoCardless { .. } => Amount::from(value, "amount", "currency"),
            Self::Btcpay { .. } => Amount::from(value, "amount", "currency"),
            Self::CoinGate { .. } => Amount::from(value, "price_amount", "price_currency"),
        }
    }
    pub(super) fn prepare(
        &self,
        operation: &str,
        mut terms: Map<String, Value>,
        key: &str,
        parent: Option<&Reference>,
    ) -> Result<Prepared> {
        let child = matches!(
            operation,
            "payment.cancel" | "payment.capture" | "refund.create"
        );
        if child != parent.is_some() || parent.is_some_and(|p| p.resource != "payments") {
            return Err(Error::Rejected);
        }
        let same = matches!(operation, "payment.cancel" | "payment.capture");
        let amount = if same {
            parent.expect("validated parent").amount.clone()
        } else {
            if matches!(self, Self::GoCardless { .. }) && operation == "refund.create" {
                if terms.contains_key("currency") {
                    return Err(Error::Invalid);
                }
                Amount::from(
                    &json!({"amount":terms.get("amount"),"currency":parent.unwrap().amount.currency}),
                    "amount",
                    "currency",
                )?
            } else {
                self.amount(&Value::Object(terms.clone()))?
            }
        };
        // Resource IDs and idempotency belong to the authenticated journal.
        if terms.contains_key("idempotency_key")
            || terms.contains_key("payment_id")
            || terms.contains_key("id")
        {
            return Err(Error::Invalid);
        }
        let parent_id = parent.map(|p| p.id.clone());
        let mut method = Method::POST;
        let (resource, path) = match (self, operation) {
            (Self::Mollie { .. }, "payment.create") => ("payments", "payments".into()),
            (Self::Mollie { .. }, "payment.cancel") => {
                method = Method::DELETE;
                (
                    "payments",
                    format!("payments/{}", parent_id.as_ref().unwrap()),
                )
            }
            (Self::Mollie { .. }, "refund.create") => (
                "refunds",
                format!("payments/{}/refunds", parent_id.as_ref().unwrap()),
            ),
            (Self::Square { .. }, "payment.create") => {
                let source = terms
                    .get("source_id")
                    .and_then(Value::as_str)
                    .ok_or(Error::Invalid)?;
                if ["CASH", "EXTERNAL"].contains(&source) {
                    return Err(Error::Unsupported);
                }
                terms.insert("idempotency_key".into(), json!(key));
                ("payments", "payments".into())
            }
            (Self::Square { .. }, "payment.capture" | "payment.cancel") => (
                "payments",
                format!(
                    "payments/{}/{}",
                    parent_id.as_ref().unwrap(),
                    if operation == "payment.capture" {
                        "complete"
                    } else {
                        "cancel"
                    }
                ),
            ),
            (Self::Square { .. }, "refund.create") => {
                terms.insert("payment_id".into(), json!(parent_id));
                terms.insert("idempotency_key".into(), json!(key));
                ("refunds", "refunds".into())
            }
            (Self::GoCardless { .. }, "payment.create") => ("payments", "payments".into()),
            (Self::GoCardless { .. }, "payment.cancel") => (
                "payments",
                format!("payments/{}/actions/cancel", parent_id.as_ref().unwrap()),
            ),
            (Self::GoCardless { .. }, "refund.create") => {
                let links = terms
                    .entry("links")
                    .or_insert_with(|| json!({}))
                    .as_object_mut()
                    .ok_or(Error::Invalid)?;
                if links.insert("payment".into(), json!(parent_id)).is_some() {
                    return Err(Error::Invalid);
                }
                ("refunds", "refunds".into())
            }
            (Self::Btcpay { store, .. }, "invoice.create") => {
                ("invoices", format!("api/v1/stores/{store}/invoices"))
            }
            (Self::CoinGate { .. }, "order.create") => {
                terms.insert("order_id".into(), json!(key));
                ("orders", "orders".into())
            }
            _ => return Err(Error::Unsupported),
        };
        if terms.contains_key("testmode") {
            return Err(Error::Invalid);
        }
        if let Self::Mollie {
            test_mode: Some(mode),
        } = self
        {
            terms.insert("testmode".into(), json!(mode));
        }
        let body = if matches!(self, Self::GoCardless { .. }) {
            if same {
                json!({"data": terms})
            } else {
                json!({resource: terms})
            }
        } else {
            Value::Object(terms)
        };
        Ok(Prepared {
            method,
            path,
            body,
            resource,
            parent: if resource == "refunds" {
                parent_id.clone()
            } else {
                None
            },
            expected: if same { parent_id } else { None },
            amount,
        })
    }
    pub(super) fn read(&self, reference: &Reference) -> Result<Prepared> {
        let resource = match (self, reference.resource.as_str()) {
            (Self::Mollie { .. } | Self::Square { .. } | Self::GoCardless { .. }, "payments") => {
                "payments"
            }
            (Self::Mollie { .. } | Self::Square { .. } | Self::GoCardless { .. }, "refunds") => {
                "refunds"
            }
            (Self::Btcpay { .. }, "invoices") => "invoices",
            (Self::CoinGate { .. }, "orders") => "orders",
            _ => return Err(Error::Invalid),
        };
        let path = match self {
            Self::Btcpay { .. } => format!("api/v1/invoices/{}", reference.id),
            Self::Mollie { .. } if resource == "refunds" => format!(
                "payments/{}/refunds/{}",
                reference.parent.as_ref().ok_or(Error::Invalid)?,
                reference.id
            ),
            _ => format!("{resource}/{}", reference.id),
        };
        Ok(Prepared {
            method: Method::GET,
            path,
            body: Value::Null,
            resource,
            parent: reference.parent.clone(),
            expected: Some(reference.id.clone()),
            amount: reference.amount.clone(),
        })
    }
    pub(super) fn object<'a>(&self, value: &'a Value, resource: &str) -> Result<&'a Value> {
        let value = match self {
            Self::Square { .. } => {
                &value[if resource == "refunds" {
                    "refund"
                } else {
                    "payment"
                }]
            }
            Self::GoCardless { .. } => &value[resource],
            _ => value,
        };
        if !value.is_object() {
            return Err(Error::Rejected);
        }
        Ok(value)
    }
}
